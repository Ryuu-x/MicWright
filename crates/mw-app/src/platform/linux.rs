//! Linux desktop integration: one running copy, tray icon, global hotkeys, starting at
//! login, and the virtual / default mic.

use super::{Event, KeyState};
use crate::hotkey::Accel;
use mw_engine::linux as pulse;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const SEND_HINT: &str = "Choose MicWright Virtual Mic Input, then pick MicWright Virtual Mic as the mic in Discord or OBS.";
/// Matches the .desktop file `install.sh` puts in ~/.local/share/applications.
pub const APP_ID: &str = "io.github.ryuu_x.MicWright";

// ------------------------------------------------------------------ one running copy

fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("micwright.sock"),
        None => std::env::temp_dir().join(format!("micwright-{}.sock", std::env::var("USER").unwrap_or_default())),
    }
}

/// Becomes the running copy: later launches pass their command line here as events.
/// False if another copy took the socket first.
pub fn claim_instance(events: Sender<Event>) -> bool {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        return false;
    }
    // Left behind by a copy that crashed.
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else { return true };
    std::thread::Builder::new()
        .name("mw-instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if let Some(event) = Event::from_line(&line) {
                        let _ = events.send(event);
                    }
                }
            }
        })
        .ok();
    true
}

/// Hands `event` to the running copy. False when none is running.
pub fn forward_to_running(event: &Event) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket_path()) else { return false };
    writeln!(stream, "{}", event.to_line()).is_ok()
}

/// Logging out, `kill` and Ctrl+C quit the way the tray's Quit does, so the desktop gets
/// its default mic back and the virtual mic goes away.
pub fn quit_on_signals(events: Sender<Event>) {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGTERM, SIGINT, SIGHUP]) else { return };
    std::thread::Builder::new()
        .name("mw-signals".into())
        .spawn(move || {
            if signals.forever().next().is_some() {
                let _ = events.send(Event::Quit);
            }
        })
        .ok();
}

// ------------------------------------------------------------------ tray

struct TrayModel {
    events: Sender<Event>,
    muted: bool,
}

impl ksni::Tray for TrayModel {
    fn id(&self) -> String {
        "micwright".into()
    }
    fn title(&self) -> String {
        "MicWright".into()
    }
    fn icon_name(&self) -> String {
        if self.muted { "microphone-sensitivity-muted" } else { "audio-input-microphone" }.into()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: if self.muted { "MicWright: muted" } else { "MicWright" }.into(), ..Default::default() }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.events.send(Event::Show);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem { label: "Open MicWright".into(), activate: Box::new(|t: &mut Self| drop(t.events.send(Event::Show))), ..Default::default() }.into(),
            CheckmarkItem {
                label: "Mute".into(),
                checked: self.muted,
                activate: Box::new(|t: &mut Self| drop(t.events.send(Event::ToggleMute))),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem { label: "Quit".into(), activate: Box::new(|t: &mut Self| drop(t.events.send(Event::Quit))), ..Default::default() }.into(),
        ]
    }
}

pub struct Tray(ksni::blocking::Handle<TrayModel>);

impl Tray {
    /// None when the desktop has no tray (e.g. GNOME without the AppIndicator extension).
    pub fn spawn(events: Sender<Event>) -> Option<Tray> {
        use ksni::blocking::TrayMethods;
        TrayModel { events, muted: false }.spawn().ok().map(Tray)
    }

    pub fn set_muted(&self, muted: bool) {
        self.0.update(|t| t.muted = muted);
    }
}

// ------------------------------------------------------------------ global hotkeys

/// Wayland only lets apps have global hotkeys through the desktop portal, which asks the
/// user to confirm them. X11 lets any app grab keys on the root window.
pub struct Hotkeys {
    events: Sender<Event>,
    states: Arc<Mutex<HashMap<String, KeyState>>>,
    problem: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    /// The worker holding the current keys; a new one waits for it to let go of them.
    worker: Option<std::thread::JoinHandle<()>>,
    wayland: bool,
}

impl Hotkeys {
    pub fn new(events: Sender<Event>) -> Self {
        Hotkeys {
            events,
            states: Default::default(),
            problem: Default::default(),
            stop: Arc::new(AtomicBool::new(true)),
            worker: None,
            wayland: std::env::var_os("WAYLAND_DISPLAY").is_some(),
        }
    }

    /// "portal" (Wayland) or "x11".
    pub fn kind(&self) -> &'static str {
        if self.wayland {
            "portal"
        } else {
            "x11"
        }
    }

    /// Replaces every hotkey. `keys` holds (id, description, combination). Results arrive
    /// in the background: on Wayland the desktop may ask the user first.
    pub fn set(&mut self, keys: Vec<(String, String, Accel)>) {
        self.clear();
        if keys.is_empty() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        let (events, states, problem) = (self.events.clone(), self.states.clone(), self.problem.clone());
        let wayland = self.wayland;
        let previous = self.worker.take();
        self.worker = std::thread::Builder::new()
            .name("mw-hotkeys".into())
            .spawn(move || {
                // Grabbing a key the old worker still holds would fail as "in use".
                if let Some(previous) = previous {
                    let _ = previous.join();
                }
                states.lock().unwrap().clear();
                let result = if wayland {
                    async_io::block_on(portal_worker(&keys, &stop, &states, &events))
                } else {
                    x11_worker(&keys, &stop, &states, &events)
                };
                if let Err(e) = result {
                    *problem.lock().unwrap() = Some(e);
                }
            })
            .ok();
    }

    pub fn clear(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.states.lock().unwrap().clear();
        *self.problem.lock().unwrap() = None;
    }

    pub fn states(&self) -> HashMap<String, KeyState> {
        self.states.lock().unwrap().clone()
    }

    /// Why no hotkey can work right now, if so.
    pub fn problem(&self) -> Option<String> {
        self.problem.lock().unwrap().clone()
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn x11_worker(
    keys: &[(String, String, Accel)],
    stop: &AtomicBool,
    states: &Mutex<HashMap<String, KeyState>>,
    events: &Sender<Event>,
) -> Result<(), String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
    use x11rb::protocol::Event as XEvent;

    let (conn, screen) = x11rb::connect(None).map_err(|_| "Can't reach the X display for hotkeys.".to_string())?;
    let root = conn.setup().roots[screen].root;
    let (min, max) = (conn.setup().min_keycode, conn.setup().max_keycode);
    let map = conn.get_keyboard_mapping(min, max - min + 1).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
    let per = map.keysyms_per_keycode as usize;
    let keycode = |sym: u32| map.keysyms.chunks(per).position(|syms| syms.contains(&sym)).map(|i| min + i as u8);
    let relevant = u16::from(ModMask::CONTROL | ModMask::M1 | ModMask::SHIFT);

    let mut grabbed: Vec<(u8, u16, String)> = Vec::new();
    for (id, _, accel) in keys {
        let mut mods = ModMask::from(0u16);
        for (on, m) in [(accel.ctrl, ModMask::CONTROL), (accel.alt, ModMask::M1), (accel.shift, ModMask::SHIFT)] {
            if on {
                mods |= m;
            }
        }
        let Some(code) = accel.keysym().and_then(keycode) else {
            states.lock().unwrap().insert(id.clone(), KeyState::NotAssigned);
            continue;
        };
        // Caps Lock and Num Lock count as modifiers to X: grab with every combination of them.
        let locks = [0u16, u16::from(ModMask::LOCK), u16::from(ModMask::M2), u16::from(ModMask::LOCK | ModMask::M2)];
        let ok = locks.iter().all(|&lock| {
            let m = ModMask::from(u16::from(mods) | lock);
            conn.grab_key(true, root, m, code, GrabMode::ASYNC, GrabMode::ASYNC).map(|c| c.check().is_ok()).unwrap_or(false)
        });
        if ok {
            grabbed.push((code, u16::from(mods), id.clone()));
            states.lock().unwrap().insert(id.clone(), KeyState::Active(accel.label()));
        } else {
            for &lock in &locks {
                let _ = conn.ungrab_key(code, root, ModMask::from(u16::from(mods) | lock));
            }
            states.lock().unwrap().insert(id.clone(), KeyState::Taken);
        }
    }
    let _ = conn.flush();

    while !stop.load(Ordering::Acquire) {
        while let Ok(Some(event)) = conn.poll_for_event() {
            if let XEvent::KeyPress(e) = event {
                let state = u16::from(e.state) & relevant;
                if let Some((_, _, id)) = grabbed.iter().find(|(code, mods, _)| *code == e.detail && *mods == state) {
                    let _ = events.send(Event::Hotkey(id.clone()));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Closing the connection releases the grabs.
    Ok(())
}

async fn portal_worker(
    keys: &[(String, String, Accel)],
    stop: &AtomicBool,
    states: &Mutex<HashMap<String, KeyState>>,
    events: &Sender<Event>,
) -> Result<(), String> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_util::future::{select, Either};
    use futures_util::StreamExt;

    const UNSUPPORTED: &str = "Your desktop doesn't let apps use global hotkeys. KDE Plasma, GNOME 48 or later and Hyprland do. You can still bind the commands below in your desktop's keyboard settings.";
    // KDE and GNOME only give hotkeys to an app they can name. A host app names itself with
    // the Registry, which must be the first portal call on its D-Bus connection; GPUI's
    // shared one has made calls already, so this gets a connection of its own. Older
    // portals don't have the Registry.
    let bus = ashpd::zbus::Connection::session().await.map_err(|e| format!("Can't reach D-Bus for hotkeys: {e}"))?;
    if let Ok(app_id) = ashpd::AppID::try_from(APP_ID) {
        if let Err(e) = ashpd::register_host_app_with_connection(bus.clone(), app_id).await {
            if std::env::var_os("MW_DEBUG").is_some() {
                eprintln!("registering the app id with the portal: {e}");
            }
        }
    }
    let portal = GlobalShortcuts::with_connection(bus).await.map_err(|_| UNSUPPORTED.to_string())?;
    let session = portal.create_session(Default::default()).await.map_err(|e| format!("The hotkey portal failed: {e}"))?;
    let triggers: Vec<String> = keys.iter().map(|(_, _, a)| a.portal_trigger()).collect();
    let shortcuts: Vec<NewShortcut> = keys
        .iter()
        .zip(&triggers)
        .map(|((id, description, _), trigger)| NewShortcut::new(id.as_str(), description.as_str()).preferred_trigger(Some(trigger.as_str())))
        .collect();
    let bound = portal
        .bind_shortcuts(&session, &shortcuts, None, Default::default())
        .await
        .and_then(|request| request.response())
        .map_err(|e| format!("The desktop didn't set up the hotkeys: {e}"))?;
    {
        let mut states = states.lock().unwrap();
        for (id, _, accel) in keys {
            let state = match bound.shortcuts().iter().find(|s| s.id() == id) {
                Some(s) if !s.trigger_description().is_empty() => KeyState::Active(s.trigger_description().to_string()),
                Some(_) => KeyState::Active(accel.label()),
                None => KeyState::NotAssigned,
            };
            states.insert(id.clone(), state);
        }
    }
    let mut activated = Box::pin(portal.receive_activated().await.map_err(|e| e.to_string())?);
    while !stop.load(Ordering::Acquire) {
        match select(activated.next(), async_io::Timer::after(Duration::from_millis(100))).await {
            Either::Left((Some(a), _)) => drop(events.send(Event::Hotkey(a.shortcut_id().to_string()))),
            Either::Left((None, _)) => break,
            Either::Right(_) => {}
        }
    }
    let _ = session.close().await;
    Ok(())
}

// ------------------------------------------------------------------ start at login

fn autostart_file() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    config.join("autostart").join(format!("{APP_ID}.desktop"))
}

pub fn start_with_system() -> bool {
    autostart_file().exists()
}

pub fn set_start_with_system(on: bool) -> Result<(), String> {
    let file = autostart_file();
    if !on {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    // An AppImage's own path changes on every run; $APPIMAGE is the file the user keeps.
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok()).ok_or("can't find the app's own path")?;
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=MicWright\nComment=Enhances your microphone\nExec=\"{}\" --background\nIcon=audio-input-microphone\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&file, entry).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ virtual mic, default mic

pub fn ensure_virtual_mic() -> Result<(), String> {
    pulse::ensure_virtual_mic().map_err(|e| format!("Couldn't create the virtual mic: {e:#}"))
}

pub fn remove_virtual_mic() {
    let _ = pulse::remove_virtual_mic();
}

/// Makes the virtual mic the desktop's default, remembering the one it replaces.
pub fn default_mic_apply(prev: &mut Option<String>) -> Result<(), String> {
    ensure_virtual_mic()?;
    let current = pulse::default_source().map_err(|e| format!("{e:#}"))?;
    if current.as_deref() != Some(pulse::SOURCE) {
        *prev = current;
    }
    pulse::set_default_source(pulse::SOURCE).map_err(|e| format!("Couldn't make it the default mic: {e:#}"))
}

/// Puts the remembered default mic back, if ours is still the default.
pub fn default_mic_restore(prev: &mut Option<String>) {
    if pulse::default_source().ok().flatten().as_deref() == Some(pulse::SOURCE) {
        if let Some(p) = prev.as_deref().filter(|p| pulse::source_exists(p)) {
            let _ = pulse::set_default_source(p);
        }
    }
    *prev = None;
}

pub fn open_path(path: &std::path::Path) {
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

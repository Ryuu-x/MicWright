//! Everything the app keeps while its window is closed to the tray: the engine, devices,
//! preferences, tray, hotkeys and alerts. The window only draws this and calls into it.

use crate::hotkey::Accel;
use crate::platform::{self, Event, Hotkeys, KeyState, Tray};
use crate::player::Player;
use crate::prefs::Prefs;
use mw_dsp::Meters;
use mw_engine::audio;
use mw_engine::control::{recordings_dir, Controller, Preset, Severity};
use mw_engine::settings;
use mw_engine::soundboard::Prepared;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

pub const RECORD_SECONDS: u32 = 15;
pub const WAVE_BARS: usize = 72;
/// The mic's good range. The ends match the engine's "quiet" and "clipping" tips.
const QUIET_DB: f32 = -35.0;
const HOT_DB: f32 = -3.0;
const CLIP_DB: f32 = -1.0;
pub const SILENT_DB: f32 = -60.0;
/// A condition must hold this long before its warning appears…
const SHOW_AFTER: Duration = Duration::from_millis(1500);
/// …and the warning stays this long after the condition clears, so it never flickers.
const HIDE_AFTER: Duration = Duration::from_secs(4);
const MAX_TOASTS: usize = 3;
/// Warnings that describe the same thing at different severities.
const EXCLUSIVE: &[&str] = &["no-signal", "quiet", "clipping"];
const STOP_KEY_ID: &str = "stop";

/// Pill colours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Good,
    Warn,
    Bad,
    Off,
}

#[derive(Clone)]
pub struct Alert {
    pub key: String,
    pub severity: Severity,
    pub title: String,
    pub message: String,
    first_seen: Instant,
    last_seen: Instant,
}

/// What the app (not the window) must do after an event.
pub enum AppAction {
    ShowWindow,
    Quit,
}

pub struct Core {
    pub ctl: Controller,
    pub prefs: Prefs,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub input: Option<String>,
    pub send: Option<String>,
    /// None = monitor off.
    pub monitor: Option<String>,
    /// Bumped when the device lists or picks change, so the window rebuilds its dropdowns.
    pub devices_gen: u64,
    /// Bumped when the sound changes other than by a slider, so the window resyncs them.
    pub params_gen: u64,
    /// The user pressed Stop, so a device change mustn't start the engine again.
    stopped_by_user: bool,
    pub default_mic_error: Option<String>,
    pub virtual_mic_error: Option<String>,
    tray: Option<Tray>,
    pub has_tray: bool,
    tray_muted: bool,
    pub hotkeys: Hotkeys,
    pub key_states: HashMap<String, KeyState>,
    /// Hotkeys are paused while the user picks a new one.
    pub picking_hotkey: bool,
    events: Receiver<Event>,
    pub meters: Meters,
    pub hold: f32,
    pub wave: VecDeque<f32>,
    pub board_problem: Option<String>,
    /// Sound files being decoded and levelled on worker threads: (file name, result).
    adding: Vec<Receiver<(String, Result<Prepared, String>)>>,
    adding_problems: Vec<String>,
    stopped_until: Instant,
    playing_count: usize,
    pub player: Option<(Player, bool)>,
    pub play_error: Option<String>,
    pub latest: Option<(PathBuf, PathBuf)>,
    was_recording: bool,
    /// What "Undo reset" puts back; cleared by the next change to the sound.
    pub undo: Option<Vec<(String, f32)>>,
    alerts: Vec<Alert>,
    pub dismissed: HashSet<String>,
    device_scan: Option<Receiver<(Vec<String>, Vec<String>)>>,
    last_scan: Instant,
    ticks: u64,
    pub window_open: bool,
}

impl Core {
    pub fn new(events: Receiver<Event>, events_tx: Sender<Event>) -> Core {
        let mut prefs = Prefs::load();
        let mut ctl = Controller::new();
        ctl.load_state();
        ctl.board_load();
        let virtual_mic_error = platform::ensure_virtual_mic().err();
        let tray = Tray::spawn(events_tx.clone());
        let hotkeys = Hotkeys::new(events_tx.clone());
        if prefs.default_mic && virtual_mic_error.is_some() {
            prefs.default_mic = false;
        }
        // A copy that crashed while it was the default mic left the desktop on a silent one.
        if !prefs.default_mic {
            platform::default_mic_restore(&mut prefs.prev_default_mic);
        }
        let mut core = Core {
            ctl,
            prefs,
            inputs: Vec::new(),
            outputs: Vec::new(),
            input: None,
            send: None,
            monitor: None,
            devices_gen: 0,
            params_gen: 0,
            stopped_by_user: false,
            default_mic_error: None,
            virtual_mic_error,
            has_tray: tray.is_some(),
            tray,
            tray_muted: false,
            hotkeys,
            key_states: HashMap::new(),
            picking_hotkey: false,
            events,
            meters: Meters::default(),
            hold: -120.0,
            wave: VecDeque::from(vec![0.0; WAVE_BARS]),
            board_problem: None,
            adding: Vec::new(),
            adding_problems: Vec::new(),
            stopped_until: Instant::now(),
            playing_count: 0,
            player: None,
            play_error: None,
            latest: latest_recording(),
            was_recording: false,
            undo: None,
            alerts: Vec::new(),
            dismissed: HashSet::new(),
            device_scan: None,
            last_scan: Instant::now(),
            ticks: 0,
            window_open: false,
        };
        let (inputs, outputs) = (audio::input_names(), audio::output_names());
        core.load_devices(inputs, outputs);
        core.ctl.set_monitor(core.prefs.monitor_on);
        core.apply_board_levels();
        core.start_engine();
        core.register_hotkeys();
        core.prefs.save();
        core
    }

    // ------------------------------------------------------------------ devices / engine

    fn load_devices(&mut self, inputs: Vec<String>, outputs: Vec<String>) {
        let (din, dsend, dmonitor) = audio::default_devices();
        let choose = |saved: &Option<String>, list: &[String], fallback: Option<String>| match saved {
            Some(s) if list.contains(s) => Some(s.clone()),
            _ => fallback.filter(|f| list.contains(f)),
        };
        self.input = choose(&self.prefs.input_device, &inputs, din);
        self.send = choose(&self.prefs.send_device, &outputs, dsend);
        self.monitor = match self.prefs.monitor_device.as_deref() {
            Some(crate::ui::MONITOR_OFF) => None,
            _ => choose(&self.prefs.monitor_device, &outputs, dmonitor).filter(|m| !audio::is_send_device(m)),
        };
        self.inputs = inputs;
        self.outputs = outputs;
        self.devices_gen += 1;
    }

    /// A device came or went: update the lists, and restart only if that changes our route.
    fn devices_scanned(&mut self, inputs: Vec<String>, outputs: Vec<String>) {
        if inputs == self.inputs && outputs == self.outputs {
            return;
        }
        let route = (self.input.clone(), self.send.clone(), self.monitor.clone());
        self.load_devices(inputs, outputs);
        if self.input.is_none() || self.send.is_none() {
            self.ctl.stop();
        } else if !self.stopped_by_user && (route != (self.input.clone(), self.send.clone(), self.monitor.clone()) || !self.ctl.status().running) {
            self.start_engine();
        }
    }

    pub fn start_engine(&mut self) {
        let (Some(input), Some(send)) = (self.input.clone(), self.send.clone()) else {
            eprintln!("not starting: mic {:?}, send to {:?}", self.input, self.send);
            return;
        };
        // The virtual mic may have gone if the sound server restarted.
        if audio::is_send_device(&send) {
            self.virtual_mic_error = platform::ensure_virtual_mic().err();
        }
        if !self.ctl.start(&input, &send, self.monitor.as_deref()) {
            eprintln!("audio engine: {}", self.ctl.error().unwrap_or("failed to start"));
        }
        if self.prefs.default_mic {
            self.default_mic_error = platform::default_mic_apply(&mut self.prefs.prev_default_mic).err();
            self.prefs.save();
        }
    }

    pub fn toggle_engine(&mut self) {
        self.stopped_by_user = self.ctl.status().running;
        if self.stopped_by_user {
            self.ctl.stop();
        } else {
            self.start_engine();
        }
    }

    pub fn pick_input(&mut self, name: String) {
        self.prefs.input_device = Some(name.clone());
        self.input = Some(name);
        self.picked();
    }

    pub fn pick_send(&mut self, name: String) {
        self.prefs.send_device = Some(name.clone());
        self.send = Some(name);
        self.picked();
    }

    pub fn pick_monitor(&mut self, name: Option<String>) {
        self.prefs.monitor_device = Some(name.clone().unwrap_or_else(|| crate::ui::MONITOR_OFF.into()));
        self.monitor = name;
        self.picked();
    }

    fn picked(&mut self) {
        self.prefs.save();
        self.devices_gen += 1;
        self.start_engine();
    }

    pub fn set_monitor_on(&mut self, on: bool) {
        self.ctl.set_monitor(on);
        self.prefs.monitor_on = on;
        self.prefs.save();
    }

    pub fn send_is_virtual(&self) -> bool {
        self.send.as_deref().is_some_and(audio::is_send_device)
    }

    // ------------------------------------------------------------------ status

    /// One sentence answering "are people hearing me?", worst problem first.
    pub fn status_line(&self) -> (String, &'static str, Kind) {
        let s = self.ctl.status();
        if !s.running {
            if self.input.is_none() {
                return ("Choose a microphone under Devices".into(), "Stopped", Kind::Bad);
            }
            let text = if self.ctl.error().is_none() { "Nothing is being sent" } else { "Audio problem: see the message below" };
            return (text.into(), "Stopped", Kind::Bad);
        }
        if self.muted() {
            return ("Nothing is being sent while muted".into(), "Muted", Kind::Warn);
        }
        if self.meters.blocks >= 100 && self.hold < SILENT_DB {
            return ("No sound from your mic".into(), "No sound", Kind::Bad);
        }
        if !self.send_is_virtual() {
            return (format!("Sending to {}", self.send.as_deref().unwrap_or("nothing")), "Running", Kind::Warn);
        }
        if !self.enhanced() {
            return ("Sending your raw mic, without enhancement".into(), "Raw mic", Kind::Warn);
        }
        let text = if self.prefs.default_mic && self.default_mic_error.is_none() {
            "Apps on the default mic hear your enhanced voice"
        } else {
            "Apps using MicWright Virtual Mic hear your enhanced voice"
        };
        (text.into(), "Running", Kind::Good)
    }

    /// Below the good range only counts while talking: a quiet room isn't a problem.
    pub fn mic_verdict(&self) -> (&'static str, Kind) {
        let talking = self.talking();
        let h = self.hold;
        if h > CLIP_DB {
            ("Too loud: turn the gain down", Kind::Bad)
        } else if h > HOT_DB {
            ("Nearly too loud", Kind::Warn)
        } else if h >= QUIET_DB {
            ("Good level", Kind::Good)
        } else if h < SILENT_DB {
            ("No sound", Kind::Off)
        } else if talking {
            ("Too quiet: move closer", Kind::Warn)
        } else {
            ("Listening…", Kind::Off)
        }
    }

    /// Auto level can boost near-silence enough to fool the voice detector.
    pub fn talking(&self) -> bool {
        self.meters.vad > 0.6 && self.hold >= SILENT_DB
    }

    pub fn enhanced(&self) -> bool {
        self.ctl.params().enabled != 0
    }

    pub fn muted(&self) -> bool {
        self.ctl.params().mute != 0
    }

    pub fn on(&self, key: &str) -> bool {
        self.ctl.param(key).unwrap_or(0.0) >= 0.5
    }

    // ------------------------------------------------------------------ sound

    pub fn toggle_ab(&mut self) {
        self.ctl.toggle_ab();
    }

    pub fn toggle_mute(&mut self) {
        self.ctl.toggle_mute();
    }

    pub fn toggle_param(&mut self, key: &str) {
        let v = if self.on(key) { 0.0 } else { 1.0 };
        self.set_param(key, v);
        self.params_gen += 1;
    }

    /// The user changed a setting by hand.
    pub fn set_param(&mut self, key: &str, value: f32) {
        self.ctl.set_param(key, value);
        self.undo = None;
    }

    pub fn apply_preset(&mut self, key: &str) {
        self.ctl.apply_key(key);
        self.undo = None;
        self.params_gen += 1;
    }

    pub fn revert(&mut self) {
        self.ctl.revert();
        self.undo = None;
        self.params_gen += 1;
    }

    /// Resets a settings page, or undoes the last reset.
    pub fn reset_or_undo(&mut self, page: usize) {
        if let Some(saved) = self.undo.take() {
            for (key, value) in saved {
                self.ctl.set_param(&key, value);
            }
        } else {
            let p = &settings::PAGES[page];
            let keys = p.groups.iter().flat_map(|g| g.knobs.iter().map(|k| k.key).chain(g.toggle.as_ref().map(|t| t.key)));
            self.undo = Some(keys.map(|k| (k.to_string(), self.ctl.param(k).unwrap_or(0.0))).collect());
            self.ctl.reset_page(page);
        }
        self.params_gen += 1;
    }

    pub fn preset_name(&self, key: &str) -> Option<String> {
        match key.strip_prefix("custom:") {
            Some(name) => Some(name.to_string()),
            None => Preset::ALL.iter().find(|p| p.1 == key).map(|p| p.2.to_string()),
        }
    }

    /// The name on the Preset tile: the preset, or that it was edited since.
    pub fn preset_label(&self) -> String {
        let current = self.ctl.current_key();
        match self.preset_name(&current) {
            Some(name) if !current.is_empty() => name,
            _ => match self.preset_name(&self.ctl.picked_key()) {
                Some(name) => format!("{name} (edited)"),
                None => "Custom sound".into(),
            },
        }
    }

    pub fn save_preset(&mut self, name: &str) -> Result<(), String> {
        self.ctl.save_custom(name).map(|_| ())
    }

    pub fn delete_preset(&mut self, name: &str) {
        let _ = self.ctl.delete_custom(name);
    }

    // ------------------------------------------------------------------ soundboard

    pub fn apply_board_levels(&self) {
        self.ctl.board_levels(self.prefs.board_volume, self.prefs.board_monitor_volume);
    }

    pub fn set_board_levels(&mut self, send: f32, monitor: f32) {
        self.prefs.board_volume = send;
        self.prefs.board_monitor_volume = monitor;
        self.prefs.save();
        self.apply_board_levels();
    }

    pub fn toggle_clip(&mut self, id: &str, preview: bool) {
        self.board_problem = self.ctl.board_toggle(id, preview).err();
    }

    pub fn stop_all_clips(&mut self) {
        if self.playing_count > 0 {
            self.stopped_until = Instant::now() + Duration::from_secs(2);
        }
        self.ctl.board_stop();
    }

    pub fn remove_clip(&mut self, id: &str) {
        self.board_problem = self.ctl.board_remove(id).err();
        self.prefs.board_keys.remove(id);
        self.prefs.save();
        self.register_hotkeys();
    }

    /// Decoding and levelling take a moment per file: done off the UI thread, and added
    /// on a later tick.
    pub fn add_sounds(&mut self, paths: Vec<PathBuf>) {
        if self.adding.is_empty() {
            self.adding_problems.clear();
        }
        for path in paths {
            let (tx, rx) = std::sync::mpsc::channel();
            self.adding.push(rx);
            std::thread::spawn(move || {
                let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let _ = tx.send((file, mw_engine::soundboard::prepare(&path).map_err(|e| e.to_string())));
            });
        }
    }

    pub fn board_busy(&self) -> bool {
        !self.adding.is_empty()
    }

    fn finish_adding(&mut self) {
        let mut done = Vec::new();
        self.adding.retain(|rx| match rx.try_recv() {
            Ok(result) => {
                done.push(result);
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        });
        if done.is_empty() {
            return;
        }
        for (file, clip) in done {
            if let Err(e) = clip.and_then(|clip| self.ctl.board_add(&clip.id, &clip.name)) {
                self.adding_problems.push(format!("{file} {e}."));
            }
        }
        if self.adding.is_empty() {
            self.board_problem = (!self.adding_problems.is_empty()).then(|| self.adding_problems.join(" "));
            self.register_hotkeys();
        }
    }

    /// Plays a sound by name or id, as asked from the command line.
    fn play_named(&mut self, name: &str) {
        let id = self.ctl.board_clips().iter().find(|c| c.id == name || c.name.eq_ignore_ascii_case(name)).map(|c| c.id.clone());
        self.board_problem = match id {
            Some(id) => self.ctl.board_play(&id, false).err(),
            None => Some(format!("There's no sound called \"{name}\".")),
        };
    }

    /// (id, preview, fraction played) for each sound playing now.
    pub fn playing(&self) -> Vec<(String, bool, f32)> {
        self.ctl
            .board_playing()
            .into_iter()
            .map(|(id, preview, left)| {
                let total = self.ctl.board_clips().iter().find(|c| c.id == id).map(|c| c.seconds()).unwrap_or(1.0);
                (id, preview, (1.0 - left / total.max(0.01)).clamp(0.0, 1.0))
            })
            .collect()
    }

    pub fn playing_pill(&self) -> (String, Kind) {
        if self.playing_count > 0 {
            (format!("{} playing", self.playing_count), Kind::Good)
        } else if Instant::now() < self.stopped_until {
            ("Stopped".into(), Kind::Warn)
        } else {
            ("Nothing playing".into(), Kind::Off)
        }
    }

    /// What a press of Play does right now, so it never fails in silence.
    pub fn board_status(&self) -> String {
        if !self.ctl.status().running {
            "The audio engine is stopped. Start it on the Dashboard to play sounds.".into()
        } else if self.muted() {
            "You're muted, so sounds won't play.".into()
        } else if !self.send_is_virtual() {
            format!("Only {} gets these sounds: other apps need the virtual mic. See Devices.", self.send.as_deref().unwrap_or("nothing"))
        } else if self.monitor.is_none() {
            "Other people hear your sounds. Choose a Monitor device under Devices to hear them yourself.".into()
        } else {
            "Other people hear these sounds alongside your voice.".into()
        }
    }

    pub fn set_hotkeys_on(&mut self, on: bool) {
        self.prefs.board_hotkeys = on;
        self.prefs.save();
        self.register_hotkeys();
    }

    pub fn set_stop_key_on(&mut self, on: bool) {
        self.prefs.board_stop_key_on = on;
        self.prefs.save();
        self.register_hotkeys();
    }

    /// One key, one sound: taking it from another sound beats two sounds at once.
    pub fn set_clip_key(&mut self, id: &str, key: Option<Accel>) -> Result<(), String> {
        match key {
            None => {
                self.prefs.board_keys.remove(id);
            }
            Some(key) => {
                if key.text() == self.prefs.board_stop_key {
                    return Err(format!("{} already stops all sounds.", key.label()));
                }
                self.prefs.board_keys.retain(|_, v| *v != key.text());
                self.prefs.board_keys.insert(id.to_string(), key.text());
            }
        }
        self.prefs.save();
        self.register_hotkeys();
        Ok(())
    }

    pub fn set_stop_key(&mut self, key: Accel) -> Result<(), String> {
        if self.prefs.board_keys.values().any(|v| *v == key.text()) {
            return Err(format!("{} already plays a sound.", key.label()));
        }
        self.prefs.board_stop_key = key.text();
        self.prefs.save();
        self.register_hotkeys();
        Ok(())
    }

    pub fn register_hotkeys(&mut self) {
        let mut keys = Vec::new();
        if self.prefs.board_hotkeys && !self.picking_hotkey {
            if self.prefs.board_stop_key_on {
                if let Some(a) = Accel::parse(&self.prefs.board_stop_key) {
                    keys.push((STOP_KEY_ID.to_string(), "Stop all sounds".to_string(), a));
                }
            }
            for clip in self.ctl.board_clips() {
                if let Some(a) = self.prefs.board_keys.get(&clip.id).and_then(|k| Accel::parse(k)) {
                    keys.push((clip.id.clone(), format!("Play {}", clip.name), a));
                }
            }
        }
        self.hotkeys.set(keys);
        self.key_states.clear();
    }

    pub fn key_state(&self, id: &str) -> Option<&KeyState> {
        self.key_states.get(id)
    }

    // ------------------------------------------------------------------ recorder

    pub fn toggle_recording(&mut self) {
        if self.ctl.recording_left().is_some() {
            self.ctl.finish_recording();
        } else {
            self.ctl.start_recording(RECORD_SECONDS);
        }
    }

    pub fn play_recording(&mut self, enhanced: bool) {
        let same = self.player.as_ref().is_some_and(|(_, e)| *e == enhanced);
        self.player = None;
        self.play_error = None;
        if same {
            return;
        }
        let Some((raw, enh)) = self.latest.clone() else { return };
        match Player::play(if enhanced { &enh } else { &raw }, self.monitor.as_deref()) {
            Ok(p) => self.player = Some((p, enhanced)),
            Err(e) => self.play_error = Some(format!("Couldn't play the recording: {e:#}")),
        }
    }

    pub fn playing_recording(&self) -> Option<bool> {
        self.player.as_ref().map(|(_, enhanced)| *enhanced)
    }

    // ------------------------------------------------------------------ app settings

    pub fn set_default_mic(&mut self, on: bool) {
        self.prefs.default_mic = on;
        if on {
            self.default_mic_error = platform::default_mic_apply(&mut self.prefs.prev_default_mic).err();
            // Apps now depend on us being up, so come back after a restart too.
            if self.default_mic_error.is_none() && !platform::start_with_system() {
                let _ = platform::set_start_with_system(true);
            }
        } else {
            self.default_mic_error = None;
            platform::default_mic_restore(&mut self.prefs.prev_default_mic);
        }
        self.prefs.save();
    }

    pub fn set_show_tips(&mut self, on: bool) {
        self.prefs.show_tips = on;
        self.prefs.save();
    }

    pub fn set_show_all(&mut self, on: bool) {
        self.prefs.show_all_settings = on;
        self.prefs.save();
    }

    /// Hands the apps their raw mic back and removes the virtual mic: without us it is silent.
    pub fn shutdown(&mut self) {
        let _ = self.ctl.save_state();
        self.ctl.stop();
        self.hotkeys.clear();
        if self.prefs.default_mic {
            platform::default_mic_restore(&mut self.prefs.prev_default_mic);
            self.prefs.save();
        }
        platform::remove_virtual_mic();
    }

    // ------------------------------------------------------------------ alerts

    fn refresh_alerts(&mut self) {
        let now = Instant::now();
        let mut current: Vec<(String, Severity, String, String)> = Vec::new();
        if let Some(err) = self.ctl.error() {
            current.push(("error".into(), Severity::Error, "Audio problem".into(), err.to_string()));
        }
        if let Some(err) = &self.virtual_mic_error {
            current.push(("virtual-mic".into(), Severity::Error, "No virtual mic".into(), err.clone()));
        }
        if self.prefs.show_tips {
            for t in self.ctl.tips() {
                current.push((t.key.into(), t.severity, t.title.into(), t.message.into()));
            }
        }
        for (key, severity, title, message) in current {
            match self.alerts.iter_mut().find(|a| a.key == key) {
                Some(a) => {
                    a.last_seen = now;
                    (a.severity, a.title, a.message) = (severity, title, message);
                }
                None => self.alerts.push(Alert { key: key.clone(), severity, title, message, first_seen: now, last_seen: now }),
            }
            // Alternatives of the same problem replace each other at once (no lingering).
            if EXCLUSIVE.contains(&key.as_str()) {
                self.alerts.retain(|a| a.key == key || !EXCLUSIVE.contains(&a.key.as_str()));
            }
        }
        self.alerts.retain(|a| now - a.last_seen <= HIDE_AFTER);
    }

    pub fn visible_alerts(&self) -> Vec<Alert> {
        let now = Instant::now();
        let rank = |s: Severity| match s {
            Severity::Error => 0,
            Severity::Warning => 1,
            Severity::Info => 2,
        };
        let mut v: Vec<Alert> = self
            .alerts
            .iter()
            .filter(|a| !self.dismissed.contains(&a.key) && (now - a.first_seen >= SHOW_AFTER || a.key == "error" || a.key == "virtual-mic"))
            .cloned()
            .collect();
        v.sort_by_key(|a| rank(a.severity));
        v.truncate(MAX_TOASTS);
        v
    }

    // ------------------------------------------------------------------ tick

    /// ~30×/s. Returns what the app itself must do (show the window, quit).
    pub fn tick(&mut self) -> Vec<AppAction> {
        let mut actions = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            if std::env::var_os("MW_DEBUG").is_some() {
                eprintln!("event: {event:?}");
            }
            match event {
                Event::Show => actions.push(AppAction::ShowWindow),
                Event::Quit => actions.push(AppAction::Quit),
                Event::ToggleMute => self.toggle_mute(),
                Event::ToggleAb => self.toggle_ab(),
                Event::Play(name) => self.play_named(&name),
                Event::AddSound(path) => self.add_sounds(vec![PathBuf::from(path)]),
                Event::StopSounds => self.stop_all_clips(),
                Event::Hotkey(id) if id == STOP_KEY_ID => self.stop_all_clips(),
                Event::Hotkey(id) => self.board_problem = self.ctl.board_play(&id, false).err(),
            }
        }

        self.meters = self.ctl.poll();
        self.hold = self.ctl.peak_hold();
        self.ticks += 1;
        if self.ticks % 2 == 0 {
            self.wave.pop_front();
            self.wave.push_back(fraction(self.meters.in_peak_db));
            self.playing_count = self.ctl.board_playing().len();
        }
        if self.player.as_ref().is_some_and(|(p, _)| p.finished()) {
            self.player = None;
        }
        self.finish_adding();
        if self.ticks % 6 != 0 {
            return actions;
        }

        // ~5×/s: remember the sound (writes only on change), alerts, recorder, tray, devices.
        let _ = self.ctl.save_state();
        if self.ticks % 150 == 0 && std::env::var_os("MW_DEBUG").is_some() {
            let s = self.ctl.status();
            eprintln!("running {} · latency {:.0} ms · dropouts {} · window {}", s.running, s.latency_ms, s.dropouts, self.window_open);
            eprintln!("hotkeys ({}): {:?} {}", self.hotkeys.kind(), self.key_states, self.hotkeys.problem().unwrap_or_default());
        }
        self.refresh_alerts();
        let recording = self.ctl.recording_left().is_some();
        if self.was_recording && !recording {
            self.latest = latest_recording();
        }
        self.was_recording = recording;
        self.key_states = self.hotkeys.states();
        if let Some(tray) = &self.tray {
            if self.tray_muted != self.muted() {
                self.tray_muted = self.muted();
                tray.set_muted(self.tray_muted);
            }
        }
        self.scan_devices();
        actions
    }

    /// Lists devices every few seconds on a worker thread: asking the sound server takes
    /// a round trip that the UI shouldn't wait on.
    fn scan_devices(&mut self) {
        if let Some(rx) = &self.device_scan {
            if let Ok((inputs, outputs)) = rx.try_recv() {
                self.device_scan = None;
                self.devices_scanned(inputs, outputs);
            }
            return;
        }
        if self.last_scan.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.last_scan = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        self.device_scan = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send((audio::input_names(), audio::output_names()));
        });
    }
}

pub fn fraction(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Newest raw + enhanced pair. File names start with a timestamp, so the last sorts newest.
fn latest_recording() -> Option<(PathBuf, PathBuf)> {
    let raw = std::fs::read_dir(recordings_dir())
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with("-A-raw.wav"))
        .max()?;
    let enhanced = PathBuf::from(raw.to_string_lossy().replace("-A-raw.wav", "-B-enhanced.wav"));
    enhanced.exists().then_some((raw, enhanced))
}

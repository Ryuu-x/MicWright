//! Desktop integration that differs per OS. Linux gets the full app; on other systems
//! this binary is only a test front-end, so these do nothing.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(not(target_os = "linux"))]
mod other;
#[cfg(not(target_os = "linux"))]
pub use other::*;

/// Something that happened outside the window: tray menu, hotkeys, or a second launch.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Show,
    Quit,
    ToggleMute,
    ToggleAb,
    /// Play a soundboard sound, by name or id (from the command line).
    Play(String),
    /// Add a sound file to the soundboard (an absolute path).
    AddSound(String),
    StopSounds,
    /// A registered hotkey: `stop`, or the id of a sound.
    Hotkey(String),
}

impl Event {
    /// Command-line flags that a running copy acts on, e.g. `--play "Airhorn"`.
    pub fn from_args(args: &[String]) -> Option<Event> {
        let flag = |name: &str| args.iter().any(|a| a == name);
        if let Some(i) = args.iter().position(|a| a == "--play") {
            return Some(Event::Play(args.get(i + 1)?.clone()));
        }
        // Works where the desktop has no file picker portal.
        if let Some(i) = args.iter().position(|a| a == "--add-sound") {
            let path = std::path::absolute(args.get(i + 1)?).ok()?;
            return Some(Event::AddSound(path.to_string_lossy().into_owned()));
        }
        Some(if flag("--toggle-mute") {
            Event::ToggleMute
        } else if flag("--toggle-ab") {
            Event::ToggleAb
        } else if flag("--stop-sounds") {
            Event::StopSounds
        } else if flag("--quit") {
            Event::Quit
        } else {
            return None;
        })
    }

    pub fn to_line(&self) -> String {
        match self {
            Event::Show => "show".into(),
            Event::Quit => "quit".into(),
            Event::ToggleMute => "toggle-mute".into(),
            Event::ToggleAb => "toggle-ab".into(),
            Event::Play(name) => format!("play {name}"),
            Event::AddSound(path) => format!("add-sound {path}"),
            Event::StopSounds => "stop-sounds".into(),
            Event::Hotkey(id) => format!("hotkey {id}"),
        }
    }

    pub fn from_line(line: &str) -> Option<Event> {
        Some(match line.split_once(' ') {
            Some(("play", name)) => Event::Play(name.to_string()),
            Some(("add-sound", path)) => Event::AddSound(path.to_string()),
            Some(("hotkey", id)) => Event::Hotkey(id.to_string()),
            _ => match line {
                "show" => Event::Show,
                "quit" => Event::Quit,
                "toggle-mute" => Event::ToggleMute,
                "toggle-ab" => Event::ToggleAb,
                "stop-sounds" => Event::StopSounds,
                _ => return None,
            },
        })
    }
}

/// How a hotkey fared when it was registered.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyState {
    /// Works; the text is what the desktop says the trigger is.
    Active(String),
    /// Another app already has this combination.
    Taken,
    /// The desktop didn't assign it (the user may have declined or changed it).
    NotAssigned,
}

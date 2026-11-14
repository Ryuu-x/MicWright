//! App behaviour (not sound) settings, in `app.json` next to the engine's own files.
//! Field names match the Windows app's file.

use mw_engine::control::config_dir;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Prefs {
    /// Live pop-up hints (level, clipping, noise). Audio errors always show.
    pub show_tips: bool,
    /// Make the virtual mic the default mic while the app runs.
    pub default_mic: bool,
    /// The default mic before we replaced it, to put back.
    pub prev_default_mic: Option<String>,
    /// Show every sound setting instead of the few that matter most.
    pub show_all_settings: bool,
    /// Devices the user picked; None means use the best guess.
    pub input_device: Option<String>,
    pub send_device: Option<String>,
    pub monitor_device: Option<String>,
    /// Hear yourself while you talk.
    pub monitor_on: bool,
    /// Soundboard slider positions (0..1): what others hear, what you hear.
    pub board_volume: f32,
    pub board_monitor_volume: f32,
    pub board_hotkeys: bool,
    pub board_stop_key_on: bool,
    /// Hotkey per sound id, and the one that stops everything (see [`crate::hotkey::Accel`]).
    pub board_keys: BTreeMap<String, String>,
    pub board_stop_key: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            show_tips: true,
            default_mic: false,
            prev_default_mic: None,
            show_all_settings: false,
            input_device: None,
            send_device: None,
            monitor_device: None,
            monitor_on: true,
            board_volume: 0.7,
            board_monitor_volume: 0.7,
            board_hotkeys: true,
            board_stop_key_on: true,
            board_keys: BTreeMap::new(),
            board_stop_key: "ctrl-alt-end".into(),
        }
    }
}

fn file() -> PathBuf {
    config_dir().join("app.json")
}

/// The first-run walkthrough was finished or skipped.
fn setup_marker() -> PathBuf {
    config_dir().join("setup-done")
}

impl Prefs {
    pub fn load() -> Prefs {
        std::fs::read_to_string(file()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    /// Losing a preference is not worth a crash: errors are ignored.
    pub fn save(&self) {
        let _ = std::fs::create_dir_all(config_dir());
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(file(), text);
        }
    }

    pub fn setup_done() -> bool {
        setup_marker().exists()
    }

    pub fn mark_setup_done() {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(setup_marker(), "");
    }
}

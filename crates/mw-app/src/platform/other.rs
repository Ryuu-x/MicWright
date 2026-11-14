//! Non-Linux stand-ins: the Windows app (ui-winui) owns these features there.

use super::{Event, KeyState};
use crate::hotkey::Accel;
use std::collections::HashMap;
use std::sync::mpsc::Sender;

pub const SEND_HINT: &str = "Choose CABLE Input, then use CABLE Output as the mic in Discord or OBS.";

/// Always the first copy: no single-instance handling here.
pub fn claim_instance(_events: Sender<Event>) -> bool {
    true
}

pub fn forward_to_running(_event: &Event) -> bool {
    false
}

pub fn quit_on_signals(_events: Sender<Event>) {}

pub struct Tray;

impl Tray {
    pub fn spawn(_events: Sender<Event>) -> Option<Tray> {
        None
    }
    pub fn set_muted(&self, _muted: bool) {}
}

pub struct Hotkeys;

impl Hotkeys {
    pub fn new(_events: Sender<Event>) -> Self {
        Hotkeys
    }
    pub fn kind(&self) -> &'static str {
        "unavailable"
    }
    pub fn set(&mut self, _keys: Vec<(String, String, Accel)>) {}
    pub fn clear(&mut self) {}
    pub fn states(&self) -> HashMap<String, KeyState> {
        HashMap::new()
    }
    pub fn problem(&self) -> Option<String> {
        Some("Global hotkeys are part of the Windows app.".into())
    }
}

pub fn start_with_system() -> bool {
    false
}

pub fn set_start_with_system(_on: bool) -> Result<(), String> {
    Err("Not available in this test app.".into())
}

pub fn ensure_virtual_mic() -> Result<(), String> {
    Ok(())
}

pub fn remove_virtual_mic() {}

pub fn default_mic_apply(_prev: &mut Option<String>) -> Result<(), String> {
    Err("Use the Windows app for this.".into())
}

pub fn default_mic_restore(_prev: &mut Option<String>) {}

pub fn open_path(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

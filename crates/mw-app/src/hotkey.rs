//! A key combination for the soundboard, kept in app.json as text like `ctrl-alt-end`
//! (GPUI's keystroke spelling), and translated for X11 and the desktop portal.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accel {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// GPUI's lowercase key name: `a`, `5`, `f5`, `end`, `pageup`, `-` …
    pub key: String,
}

/// Keys a hotkey can use: GPUI's name, the label to show, and the X11 keysym (whose
/// name the desktop portal also takes).
const KEYS: &[(&str, &str, u32, &str)] = &[
    ("space", "Space", 0x20, "space"),
    ("end", "End", 0xff57, "End"),
    ("home", "Home", 0xff50, "Home"),
    ("pageup", "Page Up", 0xff55, "Prior"),
    ("pagedown", "Page Down", 0xff56, "Next"),
    ("insert", "Insert", 0xff63, "Insert"),
    ("delete", "Delete", 0xffff, "Delete"),
    ("left", "Left", 0xff51, "Left"),
    ("up", "Up", 0xff52, "Up"),
    ("right", "Right", 0xff53, "Right"),
    ("down", "Down", 0xff54, "Down"),
    ("pause", "Pause", 0xff13, "Pause"),
    ("-", "-", 0x2d, "minus"),
    ("=", "=", 0x3d, "equal"),
    ("[", "[", 0x5b, "bracketleft"),
    ("]", "]", 0x5d, "bracketright"),
    (";", ";", 0x3b, "semicolon"),
    ("'", "'", 0x27, "apostrophe"),
    (",", ",", 0x2c, "comma"),
    (".", ".", 0x2e, "period"),
    ("/", "/", 0x2f, "slash"),
    ("`", "`", 0x60, "grave"),
    ("\\", "\\", 0x5c, "backslash"),
];

impl Accel {
    pub fn parse(text: &str) -> Option<Accel> {
        let mut accel = Accel { ctrl: false, alt: false, shift: false, key: String::new() };
        // "ctrl--" is Ctrl plus the minus key: the key is whatever follows the last modifier.
        let mut rest = text;
        loop {
            if let Some(r) = rest.strip_prefix("ctrl-") {
                accel.ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("alt-") {
                accel.alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("shift-") {
                accel.shift = true;
                rest = r;
            } else {
                break;
            }
        }
        accel.key = rest.to_string();
        accel.keysym().is_some().then_some(accel)
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        for (on, name) in [(self.ctrl, "ctrl-"), (self.alt, "alt-"), (self.shift, "shift-")] {
            if on {
                s.push_str(name);
            }
        }
        s + &self.key
    }

    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (on, name) in [(self.ctrl, "Ctrl"), (self.alt, "Alt"), (self.shift, "Shift")] {
            if on {
                parts.push(name.into());
            }
        }
        let key = match KEYS.iter().find(|k| k.0 == self.key) {
            Some(k) => k.1.to_string(),
            None if self.key.starts_with('f') && self.key.len() > 1 => self.key.to_uppercase(),
            None => self.key.to_uppercase(),
        };
        parts.push(key);
        parts.join("+")
    }

    /// X11 keysym of the key (without modifiers).
    pub fn keysym(&self) -> Option<u32> {
        let k = self.key.as_str();
        if let Some(entry) = KEYS.iter().find(|e| e.0 == k) {
            return Some(entry.2);
        }
        let mut chars = k.chars();
        match (chars.next(), chars.next()) {
            (Some(c @ ('a'..='z' | '0'..='9')), None) => Some(c as u32),
            (Some('f'), Some(_)) => k[1..].parse::<u32>().ok().filter(|n| (1..=24).contains(n)).map(|n| 0xffbe + n - 1),
            _ => None,
        }
    }

    /// The trigger in the portal's spelling, e.g. `CTRL+ALT+End`.
    pub fn portal_trigger(&self) -> String {
        let key = match KEYS.iter().find(|e| e.0 == self.key) {
            Some(e) => e.3.to_string(),
            None if self.key.len() > 1 => self.key.to_uppercase(), // F5
            None => self.key.clone(),
        };
        let mut s = String::new();
        for (on, name) in [(self.ctrl, "CTRL+"), (self.alt, "ALT+"), (self.shift, "SHIFT+")] {
            if on {
                s.push_str(name);
            }
        }
        s + &key
    }

    /// Why this combination can't be a hotkey, if it can't.
    pub fn problem(&self) -> Option<&'static str> {
        if self.keysym().is_none() {
            return Some("That key can't be a hotkey. Try a letter, number, F-key or End.");
        }
        let function_key = self.key.starts_with('f') && self.key.len() > 1;
        if !self.ctrl && !self.alt && !function_key {
            return Some("Add Ctrl or Alt, so typing doesn't play sounds.");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_translates() {
        let a = Accel::parse("ctrl-alt-end").unwrap();
        assert_eq!(a.label(), "Ctrl+Alt+End");
        assert_eq!(a.text(), "ctrl-alt-end");
        assert_eq!(a.keysym(), Some(0xff57));
        assert_eq!(a.portal_trigger(), "CTRL+ALT+End");
        assert_eq!(Accel::parse("ctrl--").unwrap().keysym(), Some(0x2d));
        assert_eq!(Accel::parse("f5").unwrap().keysym(), Some(0xffc2));
        assert_eq!(Accel::parse("f5").unwrap().problem(), None);
        assert!(Accel::parse("a").unwrap().problem().is_some());
        assert!(Accel::parse("ctrl-capslock").is_none());
    }
}

//! The "Advanced settings" model: every user-facing control, grouped into pages.
//!
//! Each knob maps straight onto one `Params` field, with a plain-language label,
//! a one-line hint, a safe range and a friendly value format. The UI is generated
//! from this table, so adding a control is a one-line change here.

use mw_dsp::Params;

#[derive(Clone, Copy)]
pub enum Fmt {
    Db,
    Hz,
    Percent,
    Ratio,
    Semitones,
    /// A reduction: shown with a minus, so it can't be read as a boost.
    Cut,
    /// A plain 0-100% position, where 0 is a real setting rather than "off".
    Scale,
}

impl Fmt {
    pub fn format(self, v: f32) -> String {
        match self {
            Fmt::Db if v.abs() < 0.05 => "0 dB".into(),
            Fmt::Db => format!("{v:+.1} dB"),
            Fmt::Hz => format!("{v:.0} Hz"),
            Fmt::Percent if v <= 0.001 => "off".into(),
            Fmt::Percent => format!("{:.0}%", v * 100.0),
            Fmt::Ratio => format!("{v:.1} : 1"),
            Fmt::Semitones if v.abs() < 0.05 => "natural".into(),
            Fmt::Semitones => format!("{v:+.1} st"),
            Fmt::Cut if v < 0.05 => "off".into(),
            Fmt::Cut => format!("-{v:.1} dB"),
            Fmt::Scale => format!("{:.0}%", v * 100.0),
        }
    }
}

pub struct Knob {
    /// `Params` field name; the stable id front-ends use (see [`param_get`]).
    pub key: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub fmt: Fmt,
    pub get: fn(&Params) -> f32,
    pub set: fn(&mut Params, f32),
    /// Hidden when this returns false (e.g. "Fixed boost" only matters with Auto level off).
    pub visible: fn(&Params) -> bool,
}

pub struct Toggle {
    pub key: &'static str,
    pub get: fn(&Params) -> u32,
    pub set: fn(&mut Params, u32),
}

pub struct Group {
    pub title: &'static str,
    pub hint: &'static str,
    /// Groups without a switch are always on.
    pub toggle: Option<Toggle>,
    pub knobs: &'static [Knob],
}

pub struct Page {
    pub name: &'static str,
    pub blurb: &'static str,
    pub groups: &'static [Group],
}

macro_rules! knob {
    ($label:expr, $hint:expr, $min:expr, $max:expr, $step:expr, $fmt:ident, $field:ident) => {
        knob!($label, $hint, $min, $max, $step, $fmt, $field, |_| true)
    };
    ($label:expr, $hint:expr, $min:expr, $max:expr, $step:expr, $fmt:ident, $field:ident, $visible:expr) => {
        Knob {
            key: stringify!($field),
            label: $label,
            hint: $hint,
            min: $min,
            max: $max,
            step: $step,
            fmt: Fmt::$fmt,
            get: |p| p.$field,
            set: |p, v| p.$field = v,
            visible: $visible,
        }
    };
}

macro_rules! toggle {
    ($field:ident) => {
        Some(Toggle { key: stringify!($field), get: |p: &Params| p.$field, set: |p: &mut Params, v: u32| p.$field = v })
    };
}

/// The knobs shown when "Advanced settings" is off: the ones that change the most for the least.
pub const SIMPLE_KNOBS: &[&str] = &["ns_strength", "exp_range_db", "agc_target_db", "eq_bass_db", "eq_presence_db"];

pub const TONE_PAGE: usize = 2;
pub const EFFECTS_PAGE: usize = 3;

pub static PAGES: &[Page] = &[
    Page {
        name: "Cleanup",
        blurb: "Remove everything that isn't your voice.",
        groups: &[
            Group {
                title: "Noise removal",
                hint: "Removes fans, PC hum and room noise.",
                toggle: toggle!(ns_enabled),
                knobs: &[knob!("Strength", "Lower it if your voice starts to sound watery or robotic.", 0.0, 1.0, 0.05, Percent, ns_strength)],
            },
            Group {
                title: "High-quality noise removal",
                hint: "Removes more noise and some room echo, but uses more of your CPU. Off uses a lighter method.",
                toggle: toggle!(ns_high_quality),
                knobs: &[],
            },
            Group {
                title: "Quieter pauses",
                hint: "Fades the background down while you're not talking.",
                toggle: toggle!(exp_enabled),
                knobs: &[
                    knob!("Amount", "How far the background drops between sentences.", 0.0, 30.0, 1.0, Cut, exp_range_db),
                    knob!(
                        "Opens at",
                        "Lower lets softer words through. Raise it if keyboard clicks get through.",
                        -70.0,
                        -30.0,
                        1.0,
                        Db,
                        exp_threshold_db
                    ),
                ],
            },
            Group {
                title: "Rumble filter",
                hint: "Cuts desk bumps, AC hum and low room rumble.",
                toggle: None,
                knobs: &[knob!("Cutoff", "Higher removes more rumble; too high thins your voice.", 40.0, 160.0, 5.0, Hz, hpf_hz)],
            },
        ],
    },
    Page {
        name: "Level",
        blurb: "Keep your volume steady and loud enough, and catch peaks before they clip the output.",
        groups: &[
            Group {
                title: "Auto level",
                hint: "Brings a quiet or distant voice up to a steady level.",
                toggle: toggle!(agc_enabled),
                knobs: &[
                    knob!("Loudness", "How loud you come out. Around -22 dB suits Discord and most streams.", -30.0, -14.0, 0.5, Db, agc_target_db),
                    knob!(
                        "Max boost",
                        "Cap for quiet voices. More boost also lifts room noise; moving the mic closer works better.",
                        0.0,
                        30.0,
                        1.0,
                        Db,
                        agc_max_gain_db
                    ),
                    knob!("Fixed boost", "Constant gain used while Auto level is off.", 0.0, 30.0, 1.0, Db, pre_gain_db, |p| p.agc_enabled == 0),
                ],
            },
            Group {
                title: "Smoothing",
                hint: "Evens out loud and soft syllables (compressor).",
                toggle: toggle!(comp_enabled),
                knobs: &[
                    knob!("Amount", "Higher sounds more even but less natural.", 1.0, 6.0, 0.1, Ratio, comp_ratio),
                    knob!("Kicks in at", "Lower means more of your speech gets smoothed.", -36.0, -6.0, 0.5, Db, comp_threshold_db),
                    knob!("Extra loudness", "Volume added back after smoothing.", 0.0, 12.0, 0.5, Db, comp_makeup_db),
                ],
            },
            Group {
                title: "Output",
                hint: "Final volume, with a ceiling that catches peaks. It can't fix a mic that's clipping: turn its gain down for that.",
                toggle: None,
                knobs: &[
                    knob!("Volume", "Overall output volume.", -12.0, 12.0, 0.5, Db, output_gain_db),
                    knob!("Peak ceiling", "Enhanced audio never goes above this level.", -6.0, 0.0, 0.5, Db, limiter_ceiling_db),
                ],
            },
        ],
    },
    Page {
        name: "Tone",
        blurb: "Shape how your voice sounds.",
        groups: &[
            Group {
                title: "Voice EQ",
                hint: "Bass, body and brightness.",
                toggle: toggle!(eq_enabled),
                knobs: &[
                    knob!("Bass", "Adds weight and depth (120 Hz).", -6.0, 9.0, 0.5, Db, eq_bass_db),
                    knob!("Mud", "Turn down to clean up a boxy, muffled sound (250 Hz).", -9.0, 3.0, 0.5, Db, eq_mud_db),
                    knob!("Clarity", "Helps you cut through game audio (4 kHz).", -3.0, 9.0, 0.5, Db, eq_presence_db),
                    knob!("Treble", "Brightness and air (10 kHz).", -6.0, 6.0, 0.5, Db, eq_air_db),
                ],
            },
            Group {
                title: "Ess control",
                hint: "Tames harsh 's' and 'sh' sounds (de-esser).",
                toggle: toggle!(deess_enabled),
                knobs: &[
                    knob!("Strength", "How much harsh s-sounds are turned down.", 0.0, 12.0, 0.5, Cut, deess_max_db),
                    knob!("Kicks in at", "Lower catches softer s-sounds too.", -40.0, -10.0, 1.0, Db, deess_threshold_db),
                ],
            },
        ],
    },
    Page {
        name: "Effects",
        blurb: "Creative effects for character. All off by default.",
        groups: &[
            Group {
                title: "Voice character",
                hint: "Change the pitch of your voice or give it analog warmth.",
                toggle: None,
                knobs: &[
                    knob!("Pitch", "Shift your voice up or down in semitones.", -12.0, 12.0, 0.5, Semitones, pitch_semitones),
                    knob!("Warmth", "Gentle saturation for a fuller, richer sound.", 0.0, 1.0, 0.05, Percent, warmth),
                ],
            },
            Group {
                title: "Reverb",
                hint: "Adds a sense of space around your voice.",
                toggle: None,
                knobs: &[
                    knob!("Amount", "How much room sound is mixed in.", 0.0, 1.0, 0.05, Percent, reverb_mix),
                    knob!("Room size", "Small room up to a large hall.", 0.0, 1.0, 0.05, Scale, reverb_size),
                    knob!("Darkness", "Higher gives softer, darker echoes.", 0.0, 1.0, 0.05, Scale, reverb_damping),
                ],
            },
        ],
    },
];

/// Every knob in display order (page → group → knob).
pub fn all_knobs() -> impl Iterator<Item = &'static Knob> {
    PAGES.iter().flat_map(|p| p.groups.iter()).flat_map(|g| g.knobs.iter())
}

/// Index of the first knob of `page` in `all_knobs()` order.
pub fn page_offset(page: usize) -> usize {
    PAGES[..page].iter().flat_map(|p| p.groups.iter()).map(|g| g.knobs.len()).sum()
}

/// Copies every value shown on `page` from `defaults` into `params`.
pub fn reset_page(page: usize, params: &mut Params, defaults: &Params) {
    for g in PAGES[page].groups {
        if let Some(t) = &g.toggle {
            (t.set)(params, (t.get)(defaults));
        }
        for k in g.knobs {
            (k.set)(params, (k.get)(defaults));
        }
    }
}

/// Generates key-based access to every `Params` field (u32 switches read as 0.0 / 1.0).
macro_rules! param_access {
    (f32: [$($f:ident),* $(,)?], u32: [$($u:ident),* $(,)?]) => {
        /// Every `Params` field name, i.e. every valid key.
        pub const PARAM_KEYS: &[&str] = &[$(stringify!($f),)* $(stringify!($u),)*];

        pub fn param_get(p: &Params, key: &str) -> Option<f32> {
            match key {
                $(stringify!($f) => Some(p.$f),)*
                $(stringify!($u) => Some(p.$u as f32),)*
                _ => None,
            }
        }

        /// Returns false for unknown keys. Values are clamped to the knob's range when it has one.
        pub fn param_set(p: &mut Params, key: &str, value: f32) -> bool {
            if !value.is_finite() {
                return false;
            }
            let value = match all_knobs().find(|k| k.key == key) {
                Some(k) => value.clamp(k.min, k.max),
                None => value,
            };
            match key {
                $(stringify!($f) => p.$f = value,)*
                $(stringify!($u) => p.$u = (value >= 0.5) as u32,)*
                _ => return false,
            }
            true
        }
    };
}

param_access!(
    f32: [
        hpf_hz, ns_strength, exp_threshold_db, exp_ratio, exp_range_db, agc_target_db, agc_max_gain_db,
        agc_min_gain_db, pre_gain_db, comp_threshold_db, comp_ratio, comp_knee_db, comp_attack_ms,
        comp_release_ms, comp_makeup_db, eq_mud_hz, eq_mud_db, eq_presence_hz, eq_presence_db, eq_air_db,
        deess_threshold_db, deess_max_db, limiter_ceiling_db, output_gain_db, eq_bass_db, pitch_semitones,
        warmth, reverb_mix, reverb_size, reverb_damping,
    ],
    u32: [enabled, mute, ns_enabled, ns_high_quality, exp_enabled, agc_enabled, comp_enabled, eq_enabled, deess_enabled, fx_enabled]
);

fn fmt_name(f: Fmt) -> &'static str {
    match f {
        Fmt::Db => "db",
        Fmt::Hz => "hz",
        Fmt::Percent => "percent",
        Fmt::Ratio => "ratio",
        Fmt::Semitones => "semitones",
        Fmt::Cut => "cut",
        Fmt::Scale => "scale",
    }
}

/// Knob keys shown only while Auto level is off. Front-ends hide them otherwise.
pub fn visible_when_agc_off(key: &str) -> bool {
    key == "pre_gain_db"
}

/// The whole settings table as JSON, for non-Rust front-ends.
pub fn settings_json() -> String {
    let pages: Vec<serde_json::Value> = PAGES
        .iter()
        .map(|page| {
            serde_json::json!({
                "name": page.name,
                "blurb": page.blurb,
                "groups": page.groups.iter().map(|g| serde_json::json!({
                    "title": g.title,
                    "hint": g.hint,
                    "toggle": g.toggle.as_ref().map(|t| t.key),
                    "knobs": g.knobs.iter().map(|k| serde_json::json!({
                        "key": k.key,
                        "label": k.label,
                        "hint": k.hint,
                        "min": k.min,
                        "max": k.max,
                        "step": k.step,
                        "format": fmt_name(k.fmt),
                        "onlyWhenAgcOff": visible_when_agc_off(k.key),
                        "simple": SIMPLE_KNOBS.contains(&k.key),
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::Value::Array(pages).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fit_slider_ranges() {
        for p in [Params::far_mic(), Params::quiet_room(), Params::noisy_room(), Params::beatbox()] {
            for k in all_knobs() {
                let v = (k.get)(&p);
                assert!(v >= k.min && v <= k.max, "{} = {v} outside {}..{}", k.label, k.min, k.max);
            }
        }
    }

    #[test]
    fn simple_knobs_exist() {
        for key in SIMPLE_KNOBS {
            assert!(all_knobs().any(|k| k.key == *key), "{key}");
        }
    }

    #[test]
    fn every_knob_and_toggle_key_is_accessible() {
        let mut p = Params::far_mic();
        for k in all_knobs() {
            assert!(param_get(&p, k.key).is_some(), "{}", k.key);
            assert!(param_set(&mut p, k.key, k.min), "{}", k.key);
            assert_eq!(param_get(&p, k.key), Some(k.min), "{}", k.key);
        }
        for t in PAGES.iter().flat_map(|pg| pg.groups.iter()).filter_map(|g| g.toggle.as_ref()) {
            assert!(param_set(&mut p, t.key, 0.0));
            assert_eq!((t.get)(&p), 0);
        }
        assert!(param_set(&mut p, "pitch_semitones", 99.0));
        assert_eq!(p.pitch_semitones, 12.0, "clamped to the knob range");
        assert!(serde_json::from_str::<serde_json::Value>(&settings_json()).is_ok());
    }

    #[test]
    fn reset_page_only_touches_that_page() {
        let defaults = Params::far_mic();
        let mut p = defaults;
        p.pitch_semitones = 5.0;
        p.eq_bass_db = 4.0;
        reset_page(3, &mut p, &defaults); // Effects
        assert_eq!(p.pitch_semitones, 0.0);
        assert_eq!(p.eq_bass_db, 4.0);
    }
}

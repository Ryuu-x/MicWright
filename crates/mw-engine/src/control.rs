//! UI-independent app state: the running engine, parameters, presets, tips and the
//! A/B recorder. Front-ends call into this and only draw.

use crate::audio::{self, Engine, Recording, Shared};
use crate::settings;
use crate::soundboard::{self, Clip, Command};
use mw_dsp::{Meters, Params, SAMPLE_RATE};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    Far,
    Quiet,
    Noisy,
    Beatbox,
}

impl Preset {
    pub const ALL: [(Preset, &'static str, &'static str, &'static str); 4] = [
        (Preset::Far, "far", "Far mic", "Mic is some distance away: strong noise removal and auto level"),
        (Preset::Quiet, "quiet", "Quiet room", "Lighter processing for a quiet room or a close mic"),
        (Preset::Noisy, "noisy", "Noisy room", "Loud fans or keyboard: fades the background further between words"),
        (Preset::Beatbox, "beatbox", "Beatbox", "Keeps every kick, hi-hat and bass sound: no noise removal, gating or de-essing, so room noise stays in"),
    ];

    pub fn params(self) -> Params {
        match self {
            Preset::Far => Params::far_mic(),
            Preset::Quiet => Params::quiet_room(),
            Preset::Noisy => Params::noisy_room(),
            Preset::Beatbox => Params::beatbox(),
        }
    }

    /// False for room presets, which leave the user's tone alone. The de-esser and EQ
    /// live on the Tone page, and beatboxing can't work with a voice tone.
    fn sets_tone(self) -> bool {
        self == Preset::Beatbox
    }

    pub fn key(self) -> &'static str {
        Self::ALL.iter().find(|p| p.0 == self).map(|p| p.1).unwrap_or("far")
    }

    pub fn from_key(key: &str) -> Option<Preset> {
        Self::ALL.iter().find(|p| p.1 == key).map(|p| p.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

pub struct Tip {
    pub key: &'static str,
    pub severity: Severity,
    pub title: &'static str,
    pub message: &'static str,
}

pub struct Status {
    pub running: bool,
    pub latency_ms: f32,
    pub input: String,
    pub output: String,
    pub dropouts: u32,
}

pub struct Controller {
    shared: Arc<Shared>,
    params: Params,
    /// Built-in preset currently applied (None once anything is edited).
    preset: Option<Preset>,
    /// Custom preset currently applied (None once anything is edited).
    custom: Option<String>,
    /// Values that "Reset page" restores: the last preset (built-in or custom) picked.
    base: Params,
    /// The preset picked last and the sound right after picking it: what "Revert" goes back to.
    picked: Option<(String, Params)>,
    /// The sound from before a tone-setting preset, whose Tone page comes back on leaving it.
    tone_before: Option<Params>,
    engine: Option<Engine>,
    error: Option<String>,
    meters: Meters,
    in_peak_hold: f32,
    last_poll: Instant,
    recording_until: Option<Instant>,
    last_saved: Option<PathBuf>,
    /// What `last.json` holds now, so `save_state` only writes real changes.
    saved_state: String,
    board: Vec<Clip>,
    /// Removed clips the mixer may still be playing: freed here, not on the DSP thread.
    retired: Vec<Arc<[f32]>>,
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

impl Controller {
    pub fn new() -> Self {
        let params = Params::far_mic();
        Self {
            shared: Shared::new(params),
            params,
            preset: Some(Preset::Far),
            custom: None,
            base: params,
            picked: Some((Preset::Far.key().to_string(), params)),
            tone_before: None,
            engine: None,
            error: None,
            meters: Meters::default(),
            in_peak_hold: -120.0,
            last_poll: Instant::now(),
            recording_until: None,
            last_saved: None,
            saved_state: String::new(),
            board: Vec::new(),
            retired: Vec::new(),
        }
    }

    // ------------------------------------------------------------ engine

    pub fn start(&mut self, input: &str, send: &str, monitor: Option<&str>) -> bool {
        self.engine = None;
        match Engine::start(input, send, monitor, self.shared.clone()) {
            Ok(e) => {
                self.engine = Some(e);
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                false
            }
        }
    }

    pub fn stop(&mut self) {
        self.engine = None;
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn status(&self) -> Status {
        let dropouts = self.shared.underruns.load(Ordering::Relaxed) + self.shared.overflows.load(Ordering::Relaxed);
        match &self.engine {
            Some(e) => Status {
                running: true,
                latency_ms: e.chain_latency_ms + self.shared.buffer_ms.load(Ordering::Relaxed) as f32,
                input: e.input_desc.clone(),
                output: e.output_desc.clone(),
                dropouts,
            },
            None => Status { running: false, latency_ms: 0.0, input: String::new(), output: String::new(), dropouts },
        }
    }

    pub fn monitor(&self) -> bool {
        self.shared.monitor.load(Ordering::Relaxed)
    }

    pub fn set_monitor(&self, on: bool) {
        self.shared.monitor.store(on, Ordering::Relaxed);
    }

    // ------------------------------------------------------------ parameters

    pub fn params(&self) -> &Params {
        &self.params
    }

    fn push(&self) {
        self.shared.set_params(self.params);
    }

    pub fn param(&self, key: &str) -> Option<f32> {
        settings::param_get(&self.params, key)
    }

    /// Sets a parameter by key; returns false for unknown keys.
    pub fn set_param(&mut self, key: &str, value: f32) -> bool {
        if !settings::param_set(&mut self.params, key, value) {
            return false;
        }
        if key != "enabled" && key != "mute" {
            self.preset = None;
            self.custom = None;
            self.relabel();
        }
        self.push();
        true
    }

    /// Puts the picked preset's name back once the sound matches it again.
    fn relabel(&mut self) {
        let Some((key, mut p)) = self.picked.clone() else { return };
        (p.enabled, p.mute) = (self.params.enabled, self.params.mute);
        if p == self.params {
            self.preset = Preset::from_key(&key);
            self.custom = key.strip_prefix("custom:").map(str::to_string);
        }
    }

    pub fn toggle_ab(&mut self) {
        self.params.enabled ^= 1;
        self.push();
    }

    pub fn toggle_mute(&mut self) {
        self.params.mute ^= 1;
        self.push();
    }

    pub fn preset(&self) -> Option<Preset> {
        self.preset
    }

    /// Room presets change cleanup and level; the user's tone and effects are kept.
    /// A preset that sets its own tone keeps the effects and hands the tone back when left.
    pub fn apply_preset(&mut self, preset: Preset) {
        let old = self.params;
        let mut p = preset.params();
        (p.enabled, p.mute) = (old.enabled, old.mute);
        if preset.sets_tone() {
            self.tone_before.get_or_insert(old);
        } else {
            let tone = self.tone_before.take().unwrap_or(old);
            settings::reset_page(settings::TONE_PAGE, &mut p, &tone);
        }
        settings::reset_page(settings::EFFECTS_PAGE, &mut p, &old);
        self.params = p;
        self.preset = Some(preset);
        self.custom = None;
        self.base = preset.params();
        self.picked = Some((preset.key().to_string(), p));
        self.push();
    }

    /// Key of the preset picked last, still known after edits; "" when there is none.
    pub fn picked_key(&self) -> String {
        self.picked.as_ref().map(|(key, _)| key.clone()).unwrap_or_default()
    }

    /// Undoes every edit since the last preset was picked.
    pub fn revert(&mut self) {
        let Some((_, mut p)) = self.picked.clone() else { return };
        (p.enabled, p.mute) = (self.params.enabled, self.params.mute);
        self.params = p;
        self.relabel();
        self.push();
    }

    /// Restores one settings page to the last picked preset's values.
    pub fn reset_page(&mut self, page: usize) {
        if page < settings::PAGES.len() {
            let base = self.base;
            settings::reset_page(page, &mut self.params, &base);
            self.relabel();
            self.push();
        }
    }

    // ------------------------------------------------------------ custom presets & saved state

    /// "far" / "quiet" / "noisy" / "beatbox", "custom:<name>", or "" when values were edited since.
    pub fn current_key(&self) -> String {
        match (&self.preset, &self.custom) {
            (Some(p), _) => p.key().to_string(),
            (None, Some(name)) => format!("custom:{name}"),
            _ => String::new(),
        }
    }

    /// Names of saved custom presets, sorted.
    pub fn custom_presets(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(presets_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension()? != "json" {
                    return None;
                }
                Some(p.file_stem()?.to_string_lossy().into_owned())
            })
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    /// Saves the current sound as a custom preset (overwrites one with the same name).
    pub fn save_custom(&mut self, name: &str) -> Result<String, String> {
        let name = clean_preset_name(name).ok_or("Use 1–40 letters, digits, spaces, - or _.")?;
        std::fs::create_dir_all(presets_dir()).map_err(|e| e.to_string())?;
        std::fs::write(preset_path(&name), params_to_json(&self.params)).map_err(|e| e.to_string())?;
        self.preset = None;
        self.custom = Some(name.clone());
        self.base = self.params;
        self.picked = Some((format!("custom:{name}"), self.params));
        Ok(name)
    }

    pub fn apply_custom(&mut self, name: &str) -> Result<(), String> {
        let name = clean_preset_name(name).ok_or("bad preset name")?;
        let text = std::fs::read_to_string(preset_path(&name)).map_err(|e| e.to_string())?;
        let mut p = self.params;
        params_from_json(&text, &mut p)?;
        (p.enabled, p.mute) = (self.params.enabled, self.params.mute);
        self.params = p;
        self.preset = None;
        self.picked = Some((format!("custom:{name}"), p));
        self.custom = Some(name);
        self.base = p;
        self.tone_before = None;
        self.push();
        Ok(())
    }

    pub fn delete_custom(&mut self, name: &str) -> Result<(), String> {
        let name = clean_preset_name(name).ok_or("bad preset name")?;
        std::fs::remove_file(preset_path(&name)).map_err(|e| e.to_string())?;
        if self.custom.as_deref() == Some(name.as_str()) {
            self.custom = None;
        }
        if self.picked_key() == format!("custom:{name}") {
            self.picked = None;
        }
        Ok(())
    }

    /// Applies any preset key accepted by [`current_key`](Self::current_key).
    pub fn apply_key(&mut self, key: &str) -> bool {
        if let Some(name) = key.strip_prefix("custom:") {
            self.apply_custom(name).is_ok()
        } else if let Some(p) = Preset::from_key(key) {
            self.apply_preset(p);
            true
        } else {
            false
        }
    }

    /// Remembers the current sound (and which preset it came from) for the next launch.
    /// Cheap to call often: it only touches the disk when something changed.
    pub fn save_state(&mut self) -> Result<(), String> {
        let state = serde_json::json!({
            "preset": self.current_key(),
            "params": serde_json::from_str::<serde_json::Value>(&params_to_json(&self.params)).unwrap_or_default(),
            "tone_before": self.tone_before.map(|t| serde_json::from_str::<serde_json::Value>(&params_to_json(&t)).unwrap_or_default()),
            "picked_key": self.picked_key(),
            "picked_params": self.picked.as_ref().map(|(_, p)| serde_json::from_str::<serde_json::Value>(&params_to_json(p)).unwrap_or_default()),
        })
        .to_string();
        if state == self.saved_state {
            return Ok(());
        }
        std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
        std::fs::write(config_dir().join("last.json"), &state).map_err(|e| e.to_string())?;
        self.saved_state = state;
        Ok(())
    }

    /// Restores what `save_state` wrote. Missing or unreadable state keeps the defaults.
    pub fn load_state(&mut self) {
        let Ok(text) = std::fs::read_to_string(config_dir().join("last.json")) else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        let mut p = self.params;
        if params_from_json(&v["params"].to_string(), &mut p).is_err() {
            return;
        }
        (p.enabled, p.mute) = (1, 0);
        self.params = p;
        let key = v["preset"].as_str().unwrap_or("");
        self.preset = Preset::from_key(key);
        // A preset deleted since then is no longer "applied"; its values still load.
        self.custom = key.strip_prefix("custom:").filter(|name| preset_path(name).exists()).map(str::to_string);
        self.base = match (self.preset, &self.custom) {
            (Some(pr), _) => pr.params(),
            _ => p,
        };
        let mut tone = p;
        self.tone_before = params_from_json(&v["tone_before"].to_string(), &mut tone).ok().map(|_| tone);
        // Saved before Revert existed, or the preset was deleted since: nothing to go back to.
        let picked = v["picked_key"].as_str().filter(|k| !k.is_empty()).unwrap_or(key).to_string();
        let known = Preset::from_key(&picked).is_some() || picked.strip_prefix("custom:").is_some_and(|name| preset_path(name).exists());
        let mut sound = p;
        let saved = params_from_json(&v["picked_params"].to_string(), &mut sound).is_ok();
        self.picked = (known && (saved || !key.is_empty())).then_some((picked, sound));
        self.push();
    }

    // ------------------------------------------------------------ soundboard

    pub fn board_load(&mut self) {
        self.board = soundboard::load_library();
    }

    pub fn board_clips(&self) -> &[Clip] {
        &self.board
    }

    /// Adds a clip saved by [`soundboard::prepare`]. A refused clip's file is removed.
    pub fn board_add(&mut self, id: &str, name: &str) -> Result<(), String> {
        let added = soundboard::read_clip(id, name.to_string()).map_err(|e| e.to_string()).and_then(|clip| {
            let total = self.board.iter().map(|c| c.samples.len()).sum::<usize>() + clip.samples.len();
            if self.board.len() >= soundboard::MAX_CLIPS {
                return Err(format!("wasn't added: the list is full ({} sounds)", soundboard::MAX_CLIPS));
            }
            if total > soundboard::MAX_TOTAL_SECONDS * SAMPLE_RATE as usize {
                return Err(format!("wasn't added: all your sounds together can't pass {} minutes", soundboard::MAX_TOTAL_SECONDS / 60));
            }
            self.board.push(clip);
            soundboard::save_library(&self.board).map_err(|e| e.to_string())
        });
        if added.is_err() {
            soundboard::delete_clip(id);
        }
        added
    }

    pub fn board_remove(&mut self, id: &str) -> Result<(), String> {
        let at = self.board.iter().position(|c| c.id == id).ok_or("that sound is gone")?;
        let clip = self.board.remove(at);
        self.retired.push(clip.samples);
        soundboard::delete_clip(id);
        soundboard::save_library(&self.board).map_err(|e| e.to_string())
    }

    /// `preview` plays for the user alone.
    pub fn board_play(&mut self, id: &str, preview: bool) -> Result<(), String> {
        let clip = self.board.iter().find(|c| c.id == id).ok_or("that sound is gone")?.samples.clone();
        let engine = self.engine.as_mut().ok_or("The audio engine isn't running.")?;
        if preview && !engine.has_monitor {
            return Err("Preview needs a Monitor device. Choose one under Devices.".into());
        }
        engine.board.push(Command::Play { clip, preview }).map_err(|_| "Too many sounds at once.".to_string())
    }

    /// `(id, preview, seconds left)` for each sound playing now.
    pub fn board_playing(&self) -> Vec<(String, bool, f32)> {
        if self.engine.is_none() {
            return Vec::new();
        }
        let list = *self.shared.board_playing.lock().unwrap();
        list.iter()
            .flatten()
            .filter_map(|p| {
                let clip = self.board.iter().find(|c| soundboard::clip_key(&c.samples) == p.clip)?;
                Some((clip.id.clone(), p.preview, p.samples_left as f32 / SAMPLE_RATE as f32))
            })
            .collect()
    }

    /// Stops the sound if it's playing, otherwise starts it.
    pub fn board_toggle(&mut self, id: &str, preview: bool) -> Result<(), String> {
        if !self.board_playing().iter().any(|(playing, _, _)| playing == id) {
            return self.board_play(id, preview);
        }
        let clip = self.board.iter().find(|c| c.id == id).ok_or("that sound is gone")?.samples.clone();
        let engine = self.engine.as_mut().ok_or("The audio engine isn't running.")?;
        engine.board.push(Command::Stop(clip)).map_err(|_| "Too many sounds at once.".to_string())
    }

    pub fn board_stop(&mut self) {
        if let Some(engine) = self.engine.as_mut() {
            let _ = engine.board.push(Command::StopAll);
        }
    }

    /// Both 0..=1 as slider positions. Ears hear gain on a curve, so squaring makes the
    /// slider feel even.
    pub fn board_levels(&self, send: f32, monitor: f32) {
        let gain = |v: f32| v.clamp(0.0, 1.0).powi(2).to_bits();
        self.shared.board_send.store(gain(send), Ordering::Relaxed);
        self.shared.board_monitor.store(gain(monitor), Ordering::Relaxed);
    }

    // ------------------------------------------------------------ metering & tips

    /// Call regularly (e.g. 30×/s) from the UI: refreshes meters, peak hold and the recorder.
    pub fn poll(&mut self) -> Meters {
        if let Ok(m) = self.shared.meters.try_lock() {
            self.meters = *m;
        }
        let dt = self.last_poll.elapsed().as_secs_f32().min(0.5);
        self.last_poll = Instant::now();
        // Peak hold falls back at 15 dB/s.
        self.in_peak_hold = (self.in_peak_hold - 15.0 * dt).max(self.meters.in_peak_db);
        if self.recording_until.is_some_and(|t| Instant::now() >= t) {
            self.finish_recording();
        }
        self.retired.retain(|clip| Arc::strong_count(clip) > 1);
        self.meters
    }

    pub fn peak_hold(&self) -> f32 {
        self.in_peak_hold
    }

    pub fn tips(&self) -> Vec<Tip> {
        let m = self.meters;
        let mut tips = Vec::new();
        if self.engine.is_none() || m.blocks < 100 {
            return tips;
        }
        if self.in_peak_hold < -60.0 {
            tips.push(Tip {
                key: "no-signal",
                severity: Severity::Error,
                title: "No signal from the mic",
                message: "Is it muted (red LED off)? Tap the top of the QuadCast to unmute, and check the gain knob underneath isn't at zero.",
            });
        } else if self.in_peak_hold < -35.0 && m.vad > 0.6 {
            tips.push(Tip {
                key: "quiet",
                severity: Severity::Warning,
                title: "Your voice is very quiet at the mic",
                message: "Speak into the side with the HyperX logo (not the top), turn the gain knob up, and set the Windows mic level to 100.",
            });
        }
        if self.in_peak_hold > -1.0 {
            tips.push(Tip {
                key: "clipping",
                severity: Severity::Error,
                title: "The mic is clipping",
                message: "Turn the gain knob underneath the mic down a little.",
            });
        }
        if self.params.agc_enabled != 0 && m.agc_gain_db > self.params.agc_max_gain_db - 2.0 {
            tips.push(Tip {
                key: "agc-limit",
                severity: Severity::Warning,
                title: "Auto level is at its limit",
                message: "Your voice needs more boost than allowed. Moving the mic closer sounds far better than more gain.",
            });
        }
        if self.shared.light_forced.load(Ordering::Relaxed) {
            tips.push(Tip {
                key: "overload",
                severity: Severity::Warning,
                title: "PC is busy: using lighter noise removal",
                message: "High-quality noise removal was paused so your audio doesn't crackle. It switches back automatically when the load drops.",
            });
        }
        if m.noise_floor_db > -45.0 {
            tips.push(Tip {
                key: "noisy",
                severity: Severity::Info,
                title: "Noisy room",
                message: "Try the Noisy room preset, and set the dial on the back of the mic to cardioid (heart icon).",
            });
        }
        tips
    }

    // ------------------------------------------------------------ A/B recorder

    pub fn start_recording(&mut self, seconds: u32) {
        let n = seconds as usize * SAMPLE_RATE as usize;
        *self.shared.recording.lock().unwrap() = Some(Recording { raw: Vec::with_capacity(n), processed: Vec::with_capacity(n) });
        self.recording_until = Some(Instant::now() + Duration::from_secs(seconds as u64));
    }

    /// Seconds left, or `None` when not recording.
    pub fn recording_left(&self) -> Option<f32> {
        self.recording_until.map(|t| t.saturating_duration_since(Instant::now()).as_secs_f32())
    }

    pub fn finish_recording(&mut self) {
        self.recording_until = None;
        let Some(rec) = self.shared.recording.lock().unwrap().take() else { return };
        let dir = recordings_dir();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let raw = dir.join(format!("{stamp}-A-raw.wav"));
        let enh = dir.join(format!("{stamp}-B-enhanced.wav"));
        match audio::write_wav(&raw, &rec.raw).and_then(|_| audio::write_wav(&enh, &rec.processed)) {
            Ok(()) => self.last_saved = Some(dir),
            Err(e) => self.error = Some(format!("saving recording failed: {e:#}")),
        }
    }

    pub fn last_saved(&self) -> Option<&PathBuf> {
        self.last_saved.as_ref()
    }
}

/// `%APPDATA%\MicWright` (`MW_CONFIG_DIR` overrides it, e.g. for tests).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MW_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("MicWright");
    // The app used to be called QuadCast Enhancer: carry its settings over once.
    let legacy = base.join("QuadCastEnhancer");
    if !dir.exists() && legacy.is_dir() {
        let _ = std::fs::rename(&legacy, &dir);
    }
    dir
}

pub fn presets_dir() -> PathBuf {
    config_dir().join("presets")
}

fn preset_path(name: &str) -> PathBuf {
    presets_dir().join(format!("{name}.json"))
}

/// Preset names become file names: keep them short and filesystem-safe.
fn clean_preset_name(name: &str) -> Option<String> {
    let name = name.trim();
    let ok = (1..=40).contains(&name.chars().count())
        && name.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_');
    ok.then(|| name.to_string())
}

/// `{"key": value, ...}` for every parameter except the A/B and mute switches.
fn params_to_json(p: &Params) -> String {
    let map: serde_json::Map<String, serde_json::Value> = settings::PARAM_KEYS
        .iter()
        .filter(|k| **k != "enabled" && **k != "mute")
        .filter_map(|k| Some((k.to_string(), serde_json::json!(settings::param_get(p, k)?))))
        .collect();
    serde_json::to_string_pretty(&map).unwrap_or_default()
}

/// Applies known keys onto `p`; unknown keys are ignored so older/newer files still load.
fn params_from_json(text: &str, p: &mut Params) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let obj = v.as_object().ok_or("preset file is not a JSON object")?;
    for (k, val) in obj {
        if let Some(x) = val.as_f64() {
            settings::param_set(p, k, x as f32);
        }
    }
    Ok(())
}

pub fn recordings_dir() -> PathBuf {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    base.join("Music").join("MicWright")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MW_CONFIG_DIR` is process-wide, and tests run in parallel.
    static CONFIG_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn revert_undoes_edits_and_keeps_the_presets_kept_tone() {
        let mut c = Controller::new();
        assert!(c.set_param("eq_bass_db", 4.0));
        c.apply_preset(Preset::Noisy);
        assert!(c.set_param("eq_bass_db", 1.0));
        assert!(c.set_param("ns_strength", 0.2));
        assert_eq!(c.current_key(), "");
        assert_eq!(c.picked_key(), "noisy");
        c.toggle_mute();

        c.revert();
        assert_eq!(c.current_key(), "noisy");
        assert_eq!(c.param("eq_bass_db"), Some(4.0));
        assert_eq!(c.param("mute"), Some(1.0));
    }

    #[test]
    fn moving_a_value_back_clears_edited() {
        let mut c = Controller::new();
        let before = c.param("ns_strength").unwrap();
        assert!(c.set_param("ns_strength", 0.2));
        assert_eq!(c.current_key(), "");
        assert!(c.set_param("ns_strength", before));
        assert_eq!(c.current_key(), "far");
    }

    #[test]
    fn preset_keeps_tone_and_effects() {
        let mut c = Controller::new();
        assert!(c.set_param("pitch_semitones", 3.0));
        assert!(c.set_param("eq_bass_db", 4.0));
        c.apply_preset(Preset::Noisy);
        assert_eq!(c.param("pitch_semitones"), Some(3.0));
        assert_eq!(c.param("eq_bass_db"), Some(4.0));
        assert_eq!(c.param("exp_threshold_db"), Some(Params::noisy_room().exp_threshold_db));
        assert_eq!(c.preset(), Some(Preset::Noisy));
    }

    #[test]
    fn beatbox_sets_tone_keeps_effects_and_gives_the_tone_back() {
        let mut c = Controller::new();
        assert!(c.set_param("reverb_mix", 0.3));
        assert!(c.set_param("eq_mud_db", -6.0));
        c.apply_preset(Preset::Beatbox);
        assert_eq!(c.param("deess_enabled"), Some(0.0));
        assert_eq!(c.param("eq_mud_db"), Some(0.0));
        assert_eq!(c.param("reverb_mix"), Some(0.3));

        // Re-picking it must not overwrite the sound to go back to.
        c.apply_preset(Preset::Beatbox);
        c.apply_preset(Preset::Far);
        assert_eq!(c.param("deess_enabled"), Some(1.0));
        assert_eq!(c.param("eq_mud_db"), Some(-6.0));
        assert_eq!(c.param("reverb_mix"), Some(0.3));
        assert_eq!(c.param("ns_enabled"), Some(1.0));
    }

    #[test]
    fn soundboard_keeps_clips_across_a_restart() {
        let _guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("mw-board-{}", std::process::id()));
        std::env::set_var("MW_CONFIG_DIR", &dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("FAKKKK.wav");
        audio::write_wav(&source, &(0..24_000).map(|n| 0.5 * (n as f32 * 0.05).sin()).collect::<Vec<_>>()).unwrap();

        let mut c = Controller::new();
        let clip = soundboard::prepare(&source).unwrap();
        assert_eq!(clip.name, "FAKKKK");
        assert!((clip.seconds - 0.5).abs() < 0.01);
        c.board_add(&clip.id, &clip.name).unwrap();
        std::fs::remove_file(&source).unwrap(); // the app keeps its own copy

        let mut fresh = Controller::new();
        fresh.board_load();
        assert_eq!(fresh.board_clips().len(), 1);
        assert_eq!(fresh.board_clips()[0].name, "FAKKKK");
        assert!(fresh.board_play(&clip.id, false).unwrap_err().contains("isn't running"));
        assert!(fresh.board_play("nope", false).is_err());

        // A tampered list can't reach outside the folder or break loading.
        let index = dir.join("sounds").join("index.json");
        let good = std::fs::read(&index).unwrap();
        std::fs::write(&index, r#"[{"id":"..\\..\\evil","name":"x"},{"id":7},"junk"]"#).unwrap();
        let mut tampered = Controller::new();
        tampered.board_load();
        assert!(tampered.board_clips().is_empty());
        std::fs::write(&index, "not json").unwrap();
        tampered.board_load();
        assert!(tampered.board_clips().is_empty());
        std::fs::write(&index, good).unwrap();

        assert!(c.board_add("deadbeef", "gone").is_err());
        c.board_remove(&clip.id).unwrap();
        assert!(c.board_clips().is_empty());
        assert!(!dir.join("sounds").join(format!("{}.wav", clip.id)).exists());
        assert!(c.board_remove(&clip.id).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn custom_presets_and_state_roundtrip() {
        let _guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("mw-test-{}", std::process::id()));
        std::env::set_var("MW_CONFIG_DIR", &dir);

        let mut c = Controller::new();
        c.set_param("eq_bass_db", 5.0);
        c.set_param("reverb_mix", 0.3);
        assert!(c.save_custom("../evil").is_err(), "path characters are rejected");
        assert_eq!(c.save_custom("  My Stream ").unwrap(), "My Stream");
        assert_eq!(c.current_key(), "custom:My Stream");
        assert_eq!(c.custom_presets(), vec!["My Stream".to_string()]);

        c.apply_preset(Preset::Quiet);
        c.set_param("eq_bass_db", 0.0);
        assert!(c.apply_key("custom:My Stream"));
        assert_eq!(c.param("eq_bass_db"), Some(5.0));
        assert_eq!(c.param("reverb_mix"), Some(0.3));

        // Reset goes back to the custom preset's own values.
        c.set_param("eq_bass_db", -3.0);
        c.reset_page(settings::TONE_PAGE);
        assert_eq!(c.param("eq_bass_db"), Some(5.0));

        c.save_state().unwrap();
        let mut fresh = Controller::new();
        fresh.load_state();
        assert_eq!(fresh.param("eq_bass_db"), Some(5.0));

        // The last preset picked is the one that comes back.
        assert!(c.apply_key("custom:My Stream"));
        c.save_state().unwrap();
        let mut picked = Controller::new();
        picked.load_state();
        assert_eq!(picked.current_key(), "custom:My Stream");

        // The tone to go back to survives a restart.
        c.set_param("eq_bass_db", 2.0);
        c.apply_preset(Preset::Beatbox);
        c.save_state().unwrap();
        let mut beatbox = Controller::new();
        beatbox.load_state();
        assert_eq!(beatbox.current_key(), "beatbox");
        beatbox.apply_preset(Preset::Far);
        assert_eq!(beatbox.param("eq_bass_db"), Some(2.0));
        c.apply_preset(Preset::Far);

        // Editing without picking a preset is remembered as it is.
        c.set_param("reverb_mix", 0.6);
        c.save_state().unwrap();
        let mut edited = Controller::new();
        edited.load_state();
        assert_eq!(edited.current_key(), "");
        assert_eq!(edited.param("reverb_mix"), Some(0.6));

        c.delete_custom("My Stream").unwrap();
        assert!(c.custom_presets().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editing_a_value_clears_the_preset_but_ab_does_not() {
        let mut c = Controller::new();
        c.toggle_ab();
        assert_eq!(c.preset(), Some(Preset::Far));
        c.set_param("ns_strength", 0.5);
        assert_eq!(c.preset(), None);
        assert!(!c.set_param("no_such_param", 1.0));
    }
}

//! Soundboard: short clips mixed into the outgoing voice.
//!
//! * **Import** turns a file the user picked into a private copy: decoded once with
//!   pure-Rust Symphonia, checked against size limits, converted to 48 kHz mono and
//!   levelled, then saved under a generated name. Nothing else reads the original, so
//!   playing a clip never parses an untrusted file.
//! * **Mixing** happens on the DSP thread, after the voice chain, so noise removal and
//!   the gate can't eat a clip. A fast limiter keeps voice + clips under the ceiling.
//!
//! The UI thread talks to the mixer through a lock-free queue. [`Controller`] keeps
//! every clip's `Arc` alive until the mixer has let go, so the DSP thread never frees one.
//!
//! [`Controller`]: crate::control::Controller

use crate::audio::{self, BLOCK};
use crate::control::config_dir;
use anyhow::{anyhow, bail, Context, Result};
use mw_dsp::SAMPLE_RATE;
use rubato::{FftFixedIn, Resampler};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Longest clip, and everything the app is willing to hold in memory (about 170 MB).
pub const MAX_SECONDS: usize = 60;
pub const MAX_TOTAL_SECONDS: usize = 900;
pub const MAX_CLIPS: usize = 64;
/// Read limit: 60 s of 96 kHz / 32-bit stereo fits with room to spare.
const MAX_FILE_BYTES: u64 = 64 << 20;
/// Sample rates outside this range are corrupt or hostile: the resampler sizes its
/// buffers from the rate, and a failed allocation can't be caught.
const RATES: std::ops::RangeInclusive<u32> = 8_000..=192_000;
const MAX_BAD_PACKETS: u32 = 64;
const MAX_NAME_CHARS: usize = 60;

/// A clip's loudest 400 ms lands here (about the level of a levelled voice).
const TARGET_RMS: f32 = 0.1;
const LOUDNESS_WINDOW: usize = 19_200;
const MAX_BOOST: f32 = 8.0;
const CLIP_PEAK: f32 = 0.7;
/// Softens the first and last 2 ms, so a clip cut mid-wave doesn't start or end on a click.
const EDGE_FADE: usize = 96;

/// Clips playing at once. The queue only has to outrun a 10 ms block.
pub const MAX_VOICES: usize = 8;
pub const QUEUE: usize = 32;
/// Stopping takes 10 ms rather than cutting mid-wave, which clicks.
const FADE: usize = BLOCK;
/// -1 dBFS, the level the app promises on stream. The user can set the voice's own
/// ceiling higher, so this only runs around clips.
const CEILING: f32 = 0.891;
/// Limiter recovery per sample: about 100 ms.
const RELEASE: f32 = 0.000_208;

// ---------------------------------------------------------------- import

/// A clip saved by [`prepare`], waiting for [`Controller::board_add`](crate::control::Controller::board_add).
pub struct Prepared {
    pub id: String,
    pub name: String,
    pub seconds: f32,
}

/// Decodes, levels and saves one file. Slow, and touches no shared state, so it can run
/// on a worker thread.
pub fn prepare(path: &Path) -> Result<Prepared> {
    let (mono, rate) = decode(path)?;
    let mut clip = to_48k(mono, rate)?;
    level(&mut clip)?;
    let id = new_id();
    audio::write_wav(&clip_path(&id), &clip)?;
    Ok(Prepared { id, name: clean_name(path), seconds: clip.len() as f32 / SAMPLE_RATE as f32 })
}

fn decode(path: &Path) -> Result<(Vec<f32>, u32)> {
    let meta = std::fs::metadata(path).context("couldn't be opened")?;
    if !meta.is_file() {
        bail!("isn't a file");
    }
    if meta.len() > MAX_FILE_BYTES {
        bail!("is larger than {} MB", MAX_FILE_BYTES >> 20);
    }
    let file = std::fs::File::open(path).context("couldn't be opened")?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, source, FormatOptions::default(), MetadataOptions::default())
        .map_err(|_| anyhow!("isn't a sound file this app can read"))?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| anyhow!("has no audio in it"))?;
    let track_id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or_else(|| anyhow!("uses an unknown audio format"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|_| anyhow!("uses an audio format this app can't read"))?;

    let mut mono = Vec::new();
    let mut frame = Vec::new();
    let mut rate = None;
    let mut bad_packets = 0;
    // A file that ends early still plays: only a file with nothing usable is an error.
    while let Ok(Some(packet)) = format.next_packet() {
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(DecodeError::DecodeError(_)) => {
                bad_packets += 1;
                if bad_packets > MAX_BAD_PACKETS {
                    bail!("is damaged");
                }
                continue;
            }
            Err(_) => break,
        };
        let spec = decoded.spec();
        let this_rate = spec.rate();
        if !RATES.contains(&this_rate) {
            bail!("has an unsupported sample rate ({this_rate} Hz)");
        }
        if *rate.get_or_insert(this_rate) != this_rate {
            bail!("changes sample rate part way through");
        }
        let channels = spec.channels().count().max(1);
        frame.resize(decoded.samples_interleaved(), 0.0f32);
        decoded.copy_to_slice_interleaved(&mut frame);
        mono.extend(frame.chunks_exact(channels).map(|f| clean_sample(f.iter().sum::<f32>() / channels as f32)));
        // Checked while decoding: a tiny file can claim hours of audio.
        if mono.len() > MAX_SECONDS * this_rate as usize {
            bail!("is longer than {MAX_SECONDS} seconds");
        }
    }
    match rate {
        Some(rate) if !mono.is_empty() => Ok((mono, rate)),
        _ => bail!("has no audio in it"),
    }
}

/// NaN or infinity from a float file would poison everything mixed after it.
fn clean_sample(s: f32) -> f32 {
    if s.is_finite() {
        s.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn to_48k(x: Vec<f32>, rate: u32) -> Result<Vec<f32>> {
    if rate == SAMPLE_RATE {
        return Ok(x);
    }
    let mut resampler = FftFixedIn::<f32>::new(rate as usize, SAMPLE_RATE as usize, 1024, 2, 1)?;
    let wanted = (x.len() as u64 * SAMPLE_RATE as u64 / rate as u64) as usize;
    let delay = resampler.output_delay();
    let mut out = Vec::with_capacity(wanted + delay + 4096);
    let mut pos = 0;
    while out.len() < wanted + delay {
        let n = resampler.input_frames_next();
        let end = (pos + n).min(x.len());
        let chunk = [&x[pos..end]];
        let block = if end - pos == n {
            resampler.process(&chunk, None)?
        } else if end > pos {
            resampler.process_partial(Some(&chunk), None)?
        } else {
            resampler.process_partial::<&[f32]>(None, None)?
        };
        out.extend_from_slice(&block[0]);
        pos = end;
    }
    out.drain(..delay);
    out.truncate(wanted);
    out.iter_mut().for_each(|s| *s = clean_sample(*s));
    Ok(out)
}

/// Brings every clip to a similar loudness, so one quiet meme doesn't mean the next
/// one is painfully loud. Quiet clips are only raised so far, to avoid lifting hiss.
fn level(x: &mut [f32]) -> Result<()> {
    let peak = x.iter().fold(0.0f32, |p, s| p.max(s.abs()));
    if peak < 1e-4 {
        bail!("is silent");
    }
    let mut sum = 0.0f64;
    let mut loudest = 0.0f64;
    for i in 0..x.len() {
        sum += (x[i] as f64).powi(2);
        if i >= LOUDNESS_WINDOW {
            sum -= (x[i - LOUDNESS_WINDOW] as f64).powi(2);
        }
        loudest = loudest.max(sum);
    }
    let rms = (loudest / x.len().min(LOUDNESS_WINDOW) as f64).sqrt() as f32;
    let gain = (TARGET_RMS / rms).min(MAX_BOOST).min(CLIP_PEAK / peak);
    x.iter_mut().for_each(|s| *s *= gain);

    let edge = EDGE_FADE.min(x.len() / 2);
    for i in 0..edge {
        let g = i as f32 / edge as f32;
        let end = x.len() - 1 - i;
        x[i] *= g;
        x[end] *= g;
    }
    Ok(())
}

/// The name shown in the list. It is only ever displayed: files are named by id.
fn clean_name(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    let name: String = stem.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
    match name.trim() {
        "" => "Sound".to_string(),
        name => name.to_string(),
    }
}

// ---------------------------------------------------------------- library

pub struct Clip {
    pub id: String,
    pub name: String,
    pub samples: Arc<[f32]>,
}

impl Clip {
    pub fn seconds(&self) -> f32 {
        self.samples.len() as f32 / SAMPLE_RATE as f32
    }
}

fn sounds_dir() -> PathBuf {
    config_dir().join("sounds")
}

fn index_path() -> PathBuf {
    sounds_dir().join("index.json")
}

fn clip_path(id: &str) -> PathBuf {
    sounds_dir().join(format!("{id}.wav"))
}

fn new_id() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0);
    format!("{now:x}{:04x}", NEXT.fetch_add(1, Ordering::Relaxed) & 0xFFFF)
}

/// Ids come back from `index.json`, which anyone can edit: they must stay file names.
fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// Reads a clip saved by [`prepare`], rejecting anything that isn't exactly that.
pub fn read_clip(id: &str, name: String) -> Result<Clip> {
    if !valid_id(id) {
        bail!("bad clip id");
    }
    let reader = hound::WavReader::open(clip_path(id))?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != SAMPLE_RATE || spec.sample_format != hound::SampleFormat::Float || spec.bits_per_sample != 32 {
        bail!("not a saved clip");
    }
    if reader.duration() as usize > MAX_SECONDS * SAMPLE_RATE as usize {
        bail!("clip too long");
    }
    let samples: Vec<f32> = reader.into_samples::<f32>().map(|s| s.map(clean_sample)).collect::<Result<_, _>>()?;
    Ok(Clip { id: id.to_string(), name, samples: samples.into() })
}

/// The saved list, in order. Entries that are missing or don't check out are skipped.
pub fn load_library() -> Vec<Clip> {
    let Ok(text) = std::fs::read_to_string(index_path()) else { return Vec::new() };
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str(&text) else { return Vec::new() };
    let mut total = 0;
    let mut clips = Vec::new();
    for e in entries.iter().take(MAX_CLIPS) {
        let (Some(id), Some(name)) = (e["id"].as_str(), e["name"].as_str()) else { continue };
        let name: String = name.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
        let Ok(clip) = read_clip(id, name) else { continue };
        total += clip.samples.len();
        if total > MAX_TOTAL_SECONDS * SAMPLE_RATE as usize {
            break;
        }
        clips.push(clip);
    }
    clips
}

/// Written beside the real file and swapped in, so a crash can't lose the whole list.
pub fn save_library(clips: &[Clip]) -> Result<()> {
    let list: Vec<_> = clips.iter().map(|c| serde_json::json!({ "id": c.id, "name": c.name })).collect();
    let dir = sounds_dir();
    std::fs::create_dir_all(&dir)?;
    let temp = dir.join("index.json.tmp");
    std::fs::write(&temp, serde_json::to_string_pretty(&list)?)?;
    std::fs::rename(&temp, index_path())?;
    Ok(())
}

pub fn delete_clip(id: &str) {
    if valid_id(id) {
        let _ = std::fs::remove_file(clip_path(id));
    }
}

// ---------------------------------------------------------------- real-time mixer

pub enum Command {
    /// `preview` plays for the user alone.
    Play { clip: Arc<[f32]>, preview: bool },
    Stop(Arc<[f32]>),
    StopAll,
}

/// One sound the mixer is playing, published for the UI.
#[derive(Clone, Copy)]
pub struct Playing {
    /// See [`clip_key`].
    pub clip: usize,
    pub samples_left: usize,
    pub preview: bool,
}

pub type PlayingList = [Option<Playing>; MAX_VOICES];

/// Identifies a clip across threads without touching its reference count.
pub fn clip_key(clip: &Arc<[f32]>) -> usize {
    Arc::as_ptr(clip) as *const f32 as usize
}

struct Voice {
    clip: Arc<[f32]>,
    pos: usize,
    preview: bool,
    /// Samples left of the fade-out, once stopping.
    fade: Option<usize>,
}

impl Voice {
    fn stop(&mut self) {
        self.fade.get_or_insert(FADE);
    }

    fn done(&self) -> bool {
        self.pos >= self.clip.len()
    }

    fn render(&mut self, bus: &mut [f32; BLOCK]) {
        for out in bus.iter_mut() {
            let Some(&sample) = self.clip.get(self.pos) else { break };
            let gain = match &mut self.fade {
                Some(0) => {
                    self.pos = self.clip.len();
                    break;
                }
                Some(left) => {
                    *left -= 1;
                    *left as f32 / FADE as f32
                }
                None => 1.0,
            };
            *out += sample * gain;
            self.pos += 1;
        }
    }
}

/// Instant attack, smooth release and no look-ahead, so the voice gains no latency.
/// While nothing goes over the ceiling it doesn't touch the signal.
struct Ceiling {
    env: f32,
}

impl Ceiling {
    fn process(&mut self, bus: &mut [f32; BLOCK]) {
        for s in bus.iter_mut() {
            let level = s.abs();
            let wanted = if level > CEILING { CEILING / level } else { 1.0 };
            self.env = if wanted < self.env { wanted } else { self.env + (wanted - self.env) * RELEASE };
            *s = (*s * self.env).clamp(-CEILING, CEILING);
        }
        // The last steps are too small for an f32 near 1.0, so it would never get there.
        if self.env > 0.999 {
            self.env = 1.0;
        }
    }
}

/// Lives on the DSP thread: allocates once, in `new`.
pub struct Mixer {
    voices: Vec<Voice>,
    send_level: f32,
    monitor_level: f32,
    send_ceiling: Ceiling,
    monitor_ceiling: Ceiling,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    pub fn new() -> Self {
        Self {
            voices: Vec::with_capacity(MAX_VOICES),
            send_level: 0.0,
            monitor_level: 0.0,
            send_ceiling: Ceiling { env: 1.0 },
            monitor_ceiling: Ceiling { env: 1.0 },
        }
    }

    pub fn command(&mut self, command: Command) {
        match command {
            Command::Play { clip, preview } => {
                // Pressing a sound again restarts it: mashing a key must not stack it up.
                self.voices.iter_mut().filter(|v| Arc::ptr_eq(&v.clip, &clip)).for_each(Voice::stop);
                if self.voices.len() == MAX_VOICES {
                    self.voices.remove(0);
                }
                self.voices.push(Voice { clip, pos: 0, preview, fade: None });
            }
            Command::Stop(clip) => self.voices.iter_mut().filter(|v| Arc::ptr_eq(&v.clip, &clip)).for_each(Voice::stop),
            Command::StopAll => self.voices.iter_mut().for_each(Voice::stop),
        }
    }

    /// What's playing now. A sound on its way out already counts as stopped.
    pub fn playing(&self, list: &mut PlayingList) {
        list.fill(None);
        let voices = self.voices.iter().filter(|v| v.fade.is_none());
        for (slot, v) in list.iter_mut().zip(voices) {
            *slot = Some(Playing { clip: clip_key(&v.clip), samples_left: v.clip.len() - v.pos, preview: v.preview });
        }
    }

    /// Writes one block for each output: `send` for other apps, `monitor` for the user.
    /// Muting stops everything except previews, so nothing can reach other apps then.
    #[allow(clippy::too_many_arguments)]
    pub fn mix(
        &mut self,
        voice: &[f32; BLOCK],
        muted: bool,
        voice_to_monitor: bool,
        send_level: f32,
        monitor_level: f32,
        send: &mut [f32; BLOCK],
        monitor: &mut [f32; BLOCK],
    ) {
        let active = !self.voices.is_empty();
        *send = *voice;
        if voice_to_monitor {
            *monitor = *voice;
        } else {
            monitor.fill(0.0);
        }
        if !active {
            (self.send_level, self.monitor_level) = (send_level, monitor_level);
        } else {
            let mut public = [0.0f32; BLOCK];
            let mut private = [0.0f32; BLOCK];
            for v in self.voices.iter_mut() {
                if muted && !v.preview {
                    v.stop();
                }
                v.render(if v.preview { &mut private } else { &mut public });
            }
            self.voices.retain(|v| !v.done());
            // Moves smoothly from the last block's level, so dragging a slider doesn't crackle.
            let step = 1.0 / BLOCK as f32;
            for i in 0..BLOCK {
                let t = i as f32 * step;
                let to_send = self.send_level + (send_level - self.send_level) * t;
                let to_monitor = self.monitor_level + (monitor_level - self.monitor_level) * t;
                // No fade out here: not one sample may reach other apps while muted.
                if !muted {
                    send[i] += public[i] * to_send;
                }
                monitor[i] += (public[i] + private[i]) * to_monitor;
            }
            (self.send_level, self.monitor_level) = (send_level, monitor_level);
        }
        if active || self.send_ceiling.env < 1.0 {
            self.send_ceiling.process(send);
        }
        if active || self.monitor_ceiling.env < 1.0 {
            self.monitor_ceiling.process(monitor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOUD: f32 = 1.0;

    fn clip(value: f32, seconds: f32) -> Arc<[f32]> {
        vec![value; (seconds * SAMPLE_RATE as f32) as usize].into()
    }

    fn mix_once(m: &mut Mixer, voice: f32, muted: bool, voice_to_monitor: bool) -> ([f32; BLOCK], [f32; BLOCK]) {
        let (mut send, mut monitor) = ([0.0; BLOCK], [0.0; BLOCK]);
        m.mix(&[voice; BLOCK], muted, voice_to_monitor, LOUD, LOUD, &mut send, &mut monitor);
        (send, monitor)
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0, |p, s| p.max(s.abs()))
    }

    #[test]
    fn voice_alone_passes_untouched() {
        let mut m = Mixer::new();
        // Above the clip ceiling: a voice ceiling of 0 dB is the user's call.
        let (send, _) = mix_once(&mut m, 0.98, false, true);
        assert_eq!(send, [0.98; BLOCK]);
        let (send, monitor) = mix_once(&mut m, 0.5, false, true);
        assert_eq!(send, [0.5; BLOCK]);
        assert_eq!(monitor, [0.5; BLOCK]);
        let (send, monitor) = mix_once(&mut m, 0.5, false, false);
        assert_eq!(send, [0.5; BLOCK]);
        assert_eq!(monitor, [0.0; BLOCK]);
    }

    #[test]
    fn clip_reaches_others_and_the_user_even_with_hear_myself_off() {
        let mut m = Mixer::new();
        m.command(Command::Play { clip: clip(0.2, 1.0), preview: false });
        mix_once(&mut m, 0.0, false, false); // the level ramps up over the first block
        let (send, monitor) = mix_once(&mut m, 0.1, false, false);
        assert!((send[100] - 0.3).abs() < 1e-5, "{}", send[100]);
        assert!((monitor[100] - 0.2).abs() < 1e-5, "{}", monitor[100]);
    }

    #[test]
    fn preview_is_never_sent() {
        let mut m = Mixer::new();
        m.command(Command::Play { clip: clip(0.2, 1.0), preview: true });
        for _ in 0..10 {
            let (send, _) = mix_once(&mut m, 0.0, false, false);
            assert_eq!(send, [0.0; BLOCK]);
        }
        let (_, monitor) = mix_once(&mut m, 0.0, false, false);
        assert!(monitor[100] > 0.1);
    }

    #[test]
    fn muting_stops_clips_for_good_but_not_previews() {
        let mut m = Mixer::new();
        m.command(Command::Play { clip: clip(0.2, 1.0), preview: false });
        mix_once(&mut m, 0.0, false, false);
        let (send, _) = mix_once(&mut m, 0.0, true, false);
        assert_eq!(send, [0.0; BLOCK], "cut at once, even mid-clip");
        mix_once(&mut m, 0.0, true, false);
        assert!(m.voices.is_empty());
        // Unmuting must not bring the rest of the clip back.
        let (send, _) = mix_once(&mut m, 0.0, false, false);
        assert_eq!(send, [0.0; BLOCK]);

        m.command(Command::Play { clip: clip(0.2, 1.0), preview: true });
        mix_once(&mut m, 0.0, true, false);
        let (_, monitor) = mix_once(&mut m, 0.0, true, false);
        assert!(monitor[100] > 0.1);
    }

    #[test]
    fn a_sound_pressed_while_muted_never_goes_out() {
        let mut m = Mixer::new();
        mix_once(&mut m, 0.0, true, false);
        m.command(Command::Play { clip: clip(0.9, 1.0), preview: false });
        for _ in 0..5 {
            let (send, _) = mix_once(&mut m, 0.0, true, false);
            assert_eq!(send, [0.0; BLOCK]);
        }
    }

    #[test]
    fn replaying_a_sound_restarts_it_instead_of_stacking() {
        let mut m = Mixer::new();
        let same = clip(0.2, 1.0);
        for _ in 0..20 {
            m.command(Command::Play { clip: same.clone(), preview: false });
        }
        for _ in 0..3 {
            mix_once(&mut m, 0.0, false, false);
        }
        assert_eq!(m.voices.len(), 1);
        let (send, _) = mix_once(&mut m, 0.0, false, false);
        assert!(peak(&send) < 0.21, "{}", peak(&send));
    }

    #[test]
    fn many_sounds_at_once_stay_under_the_ceiling() {
        let mut m = Mixer::new();
        for _ in 0..MAX_VOICES * 3 {
            m.command(Command::Play { clip: clip(0.9, 0.5), preview: false });
        }
        assert!(m.voices.len() <= MAX_VOICES);
        assert!(m.voices.capacity() == MAX_VOICES, "the DSP thread must not grow this");
        for _ in 0..40 {
            let (send, monitor) = mix_once(&mut m, 0.9, false, true);
            assert!(peak(&send) <= CEILING, "{}", peak(&send));
            assert!(peak(&monitor) <= CEILING);
        }
    }

    #[test]
    fn limiter_lets_go_after_a_clip() {
        let mut m = Mixer::new();
        m.command(Command::Play { clip: clip(0.9, 0.05), preview: false });
        for _ in 0..8 {
            mix_once(&mut m, 0.8, false, true);
        }
        for _ in 0..200 {
            mix_once(&mut m, 0.3, false, true);
        }
        let (send, _) = mix_once(&mut m, 0.3, false, true);
        assert_eq!(send, [0.3; BLOCK], "voice is back to being untouched");
    }

    #[test]
    fn stopping_one_sound_leaves_the_others_playing() {
        let mut m = Mixer::new();
        let (a, b) = (clip(0.2, 1.0), clip(0.2, 1.0));
        m.command(Command::Play { clip: a.clone(), preview: false });
        m.command(Command::Play { clip: b.clone(), preview: true });
        mix_once(&mut m, 0.0, false, false);

        let mut list: PlayingList = [None; MAX_VOICES];
        m.playing(&mut list);
        let now: Vec<_> = list.iter().flatten().collect();
        assert_eq!(now.len(), 2);
        assert_eq!(now[0].clip, clip_key(&a));
        assert_eq!(now[0].samples_left, 48_000 - BLOCK);
        assert!(now[1].preview);

        m.command(Command::Stop(a));
        m.playing(&mut list);
        let now: Vec<_> = list.iter().flatten().collect();
        assert_eq!(now.len(), 1, "gone from the list at once, not after the fade");
        assert_eq!(now[0].clip, clip_key(&b));

        mix_once(&mut m, 0.0, false, false);
        mix_once(&mut m, 0.0, false, false);
        assert_eq!(m.voices.len(), 1);
        m.command(Command::StopAll);
        m.playing(&mut list);
        assert!(list.iter().flatten().next().is_none());
    }

    #[test]
    fn stop_all_fades_without_a_click() {
        let mut m = Mixer::new();
        m.command(Command::Play { clip: clip(0.5, 1.0), preview: false });
        mix_once(&mut m, 0.0, false, false);
        m.command(Command::StopAll);
        let (send, _) = mix_once(&mut m, 0.0, false, false);
        assert!(send.windows(2).all(|w| (w[0] - w[1]).abs() < 0.01), "jumped");
        mix_once(&mut m, 0.0, false, false);
        assert!(m.voices.is_empty());
    }

    #[test]
    fn levelling_evens_out_loud_and_quiet_clips() {
        let rms = |x: &[f32]| (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt();
        let sine = |amp: f32| -> Vec<f32> { (0..48_000).map(|n| amp * (n as f32 * 0.05).sin()).collect() };
        let (mut loud, mut quiet) = (sine(1.0), sine(0.1));
        level(&mut loud).unwrap();
        level(&mut quiet).unwrap();
        assert!((rms(&loud) - rms(&quiet)).abs() < 0.01);
        assert!(peak(&loud) <= CLIP_PEAK + 1e-3);
        assert_eq!(loud[0], 0.0, "starts from silence");
        assert!(level(&mut [0.0; 480]).is_err(), "silence is refused");
    }

    #[test]
    fn bad_samples_never_get_through() {
        assert_eq!(clean_sample(f32::NAN), 0.0);
        assert_eq!(clean_sample(f32::INFINITY), 0.0);
        assert_eq!(clean_sample(7.0), 1.0);
    }

    #[test]
    fn names_and_ids_cannot_escape_the_folder() {
        assert_eq!(clean_name(Path::new(r"C:\clips\FAKKKK (1).mp3")), "FAKKKK (1)");
        assert_eq!(clean_name(Path::new("a\u{7}b.wav")), "ab");
        assert_eq!(clean_name(Path::new(".wav")), ".wav");
        assert!(clean_name(Path::new(&format!("{}.mp3", "x".repeat(300)))).chars().count() <= MAX_NAME_CHARS);
        assert!(valid_id(&new_id()));
        assert_ne!(new_id(), new_id());
        for bad in ["", "..\\..\\evil", "../evil", "abc.wav", "C:\\x"] {
            assert!(!valid_id(bad), "{bad}");
            assert!(read_clip(bad, String::new()).is_err());
        }
    }

    fn write_test_wav(path: &Path, rate: u32, channels: u16, seconds: f32, value: f32) {
        let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for n in 0..(seconds * rate as f32) as usize {
            for _ in 0..channels {
                w.write_sample(value * (n as f32 * 0.05).sin()).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    #[test]
    fn decoding_a_file_gives_48k_mono_of_the_same_length() {
        let dir = std::env::temp_dir().join(format!("mw-decode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("clip.wav");
        for (rate, channels) in [(44_100, 2), (48_000, 1), (22_050, 1), (96_000, 2)] {
            write_test_wav(&wav, rate, channels, 1.5, 0.5);
            let (mono, got) = decode(&wav).unwrap();
            assert_eq!(got, rate);
            assert!((mono.len() as f32 - 1.5 * rate as f32).abs() < 2_000.0, "{rate}: {}", mono.len());
            let out = to_48k(mono, rate).unwrap();
            assert!((out.len() as f32 - 72_000.0).abs() < 3_000.0, "{rate}: {}", out.len());
            // The middle stays a full-level signal: resampling didn't lose it.
            assert!(peak(&out[20_000..50_000]) > 0.4, "{rate}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostile_files_are_refused() {
        let dir = std::env::temp_dir().join(format!("mw-hostile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let garbage = dir.join("garbage.mp3");
        std::fs::write(&garbage, vec![0x5Au8; 100_000]).unwrap();
        assert!(decode(&garbage).is_err());
        let empty = dir.join("empty.wav");
        std::fs::write(&empty, b"").unwrap();
        assert!(decode(&empty).is_err());
        assert!(decode(&dir).is_err(), "a folder");
        assert!(decode(&dir.join("missing.mp3")).is_err());

        let long = dir.join("long.wav");
        write_test_wav(&long, 8_000, 1, MAX_SECONDS as f32 + 2.0, 0.5);
        assert!(decode(&long).unwrap_err().to_string().contains("longer"));

        let big = dir.join("big.wav");
        std::fs::File::create(&big).unwrap().set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(decode(&big).unwrap_err().to_string().contains("larger"));

        // A real header followed by noise, and a file cut off half way.
        let cut = dir.join("cut.wav");
        write_test_wav(&cut, 48_000, 2, 1.0, 0.5);
        let bytes = std::fs::read(&cut).unwrap();
        std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
        if let Ok((mono, _)) = decode(&cut) {
            assert!(mono.iter().all(|s| s.is_finite()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn float_wav_with_nan_is_cleaned_on_import() {
        let dir = std::env::temp_dir().join(format!("mw-nan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nan.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for n in 0..4_800 {
            w.write_sample(if n % 7 == 0 { f32::NAN } else if n % 11 == 0 { f32::INFINITY } else { 0.3 }).unwrap();
        }
        w.finalize().unwrap();
        let (mono, _) = decode(&path).unwrap();
        assert!(mono.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

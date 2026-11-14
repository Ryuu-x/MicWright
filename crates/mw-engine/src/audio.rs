//! Test-mode audio engine: capture device → mw-dsp chain → up to two outputs.
//!
//! * **Send** — where other apps pick the voice up (e.g. VB-Audio "CABLE Input",
//!   with Discord listening on "CABLE Output").
//! * **Monitor** — optional headphones so you can hear yourself at the same time.
//!
//! Runs entirely in user mode (no APO, no admin). The input callback runs the chain
//! and pushes into one lock-free ring per output. Each output runs on its own device
//! clock, so it drains its ring through an adaptive resampler that keeps the fill level
//! steady instead of dropping or repeating samples (which is audible as clicks/static).
//! Parameters and meters cross threads via `try_lock`, which never blocks audio threads.

use crate::soundboard::{self, Command, Mixer, PlayingList};
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use mw_dsp::{Chain, Meters, Params, SAMPLE_RATE};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Target buffer fill per output (30 ms): enough headroom for WASAPI jitter.
const TARGET_FILL: usize = 1_440;
const RING: usize = 48_000; // 1 s
/// Processing block (10 ms) — the noise removers' native frame size.
pub(crate) const BLOCK: usize = 480;
/// Upper bound for the adaptive output buffer (150 ms).
const MAX_TARGET: usize = 7_200;
/// Max resampling correction (±0.5 %, inaudible) used to track clock drift.
const MAX_CORRECTION: f64 = 0.005;

pub struct Shared {
    pub params: Mutex<Params>,
    pub generation: AtomicU32,
    pub meters: Mutex<Meters>,
    pub underruns: AtomicU32,
    pub overflows: AtomicU32,
    pub monitor: AtomicBool,
    /// Current output buffering in ms (adapts upward if the system can't keep up).
    pub buffer_ms: AtomicU32,
    /// Set while the overload guard has swapped DeepFilterNet3 for RNNoise.
    pub light_forced: AtomicBool,
    /// Raw-vs-processed capture for A/B listening later (mono, 48 kHz).
    pub recording: Mutex<Option<Recording>>,
    /// Soundboard gains for other apps and for the user (`f32` bits).
    pub board_send: AtomicU32,
    pub board_monitor: AtomicU32,
    /// What the mixer is playing, for the UI.
    pub board_playing: Mutex<PlayingList>,
}

pub struct Recording {
    pub raw: Vec<f32>,
    pub processed: Vec<f32>,
}

impl Shared {
    pub fn new(params: Params) -> Arc<Self> {
        Arc::new(Self {
            params: Mutex::new(params),
            generation: AtomicU32::new(1),
            meters: Mutex::new(Meters::default()),
            underruns: AtomicU32::new(0),
            overflows: AtomicU32::new(0),
            monitor: AtomicBool::new(true),
            buffer_ms: AtomicU32::new((TARGET_FILL / 48) as u32),
            light_forced: AtomicBool::new(false),
            recording: Mutex::new(None),
            board_send: AtomicU32::new(0),
            board_monitor: AtomicU32::new(0),
            board_playing: Mutex::new([None; soundboard::MAX_VOICES]),
        })
    }

    pub fn set_params(&self, p: Params) {
        *self.params.lock().unwrap() = p;
        self.generation.fetch_add(1, Ordering::Release);
    }
}

pub fn device_name(d: &cpal::Device) -> String {
    d.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "<unknown>".into())
}

/// What a sound-server connection is for.
#[derive(Clone, Copy)]
pub enum HostRole {
    /// Listing devices, and the mic.
    Main,
    Send,
    Monitor,
    /// Playing A/B recordings back.
    Player,
}

/// The connection for `role`. On Linux each role keeps one for the life of the app:
/// cpal's PulseAudio backend never closes a connection it opened (each leaks a socket and
/// a thread, and the server stops accepting clients at 64), and it can't run two playback
/// streams on one connection. Elsewhere a host is cheap, so every call makes a new one.
pub fn host(role: HostRole) -> Arc<cpal::Host> {
    #[cfg(target_os = "linux")]
    {
        let mut hosts = HOSTS.lock().unwrap();
        hosts[role as usize].get_or_insert_with(|| Arc::new(cpal::default_host())).clone()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = role;
        Arc::new(cpal::default_host())
    }
}

#[cfg(target_os = "linux")]
static HOSTS: Mutex<[Option<Arc<cpal::Host>>; 4]> = Mutex::new([None, None, None, None]);

/// Drops the kept connections, so the next use reconnects (e.g. after the sound server
/// restarted). Each dropped one leaks as described in [`host`]; this is rare.
pub fn reset_hosts() {
    #[cfg(target_os = "linux")]
    {
        *HOSTS.lock().unwrap() = [None, None, None, None];
    }
}

fn names(list: Result<impl Iterator<Item = cpal::Device>, cpal::Error>) -> Vec<String> {
    match list {
        Ok(devices) => devices.map(|d| device_name(&d)).collect(),
        Err(_) => {
            reset_hosts();
            Vec::new()
        }
    }
}

/// Mics to offer. On Linux the sound server also lists our own virtual mic, which would
/// feed back, and "Monitor of …" sources, which go last: they record speakers.
pub fn input_names() -> Vec<String> {
    let names = names(host(HostRole::Main).input_devices());
    #[cfg(target_os = "linux")]
    let names = {
        let (own, monitors) = crate::linux::special_inputs();
        let mut names: Vec<String> = names.into_iter().filter(|n| !own.contains(n)).collect();
        names.sort_by_key(|n| monitors.contains(n));
        names
    };
    names
}

/// Sources that record a speaker rather than a mic (Linux only).
fn monitor_sources() -> Vec<String> {
    #[cfg(target_os = "linux")]
    return crate::linux::special_inputs().1;
    #[cfg(not(target_os = "linux"))]
    return Vec::new();
}

pub fn output_names() -> Vec<String> {
    names(host(HostRole::Main).output_devices())
}

pub fn is_virtual_cable(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("cable") || n.contains("voicemeeter") || n.contains("virtual")
}

/// The output that carries the voice to other apps, by platform: VB-CABLE on Windows,
/// MicWright's own virtual mic on Linux.
pub fn is_send_device(name: &str) -> bool {
    #[cfg(target_os = "linux")]
    return name == crate::linux::SINK_DESC;
    #[cfg(not(target_os = "linux"))]
    return is_virtual_cable(name);
}

/// Picks the QuadCast mic; sends to a virtual cable if one exists, and monitors on
/// the QuadCast's own headphone jack (or the default output).
pub fn default_devices() -> (Option<String>, Option<String>, Option<String>) {
    let host = host(HostRole::Main);
    let outputs = output_names();
    let quadcast_out = outputs.iter().find(|n| n.to_lowercase().contains("quadcast")).cloned();
    // Never monitor on the output that feeds other apps: you'd hear nothing.
    let default_out = host
        .default_output_device()
        .map(|d| device_name(&d))
        .filter(|n| !is_send_device(n))
        .or_else(|| outputs.iter().find(|n| !is_send_device(n)).cloned());
    let inputs = input_names();
    let monitors = monitor_sources();
    let is_monitor = |n: &String| monitors.contains(n);
    // Never fall back to a virtual cable: once it is the Windows default mic (the
    // "every app" mode), capturing it would feed our own output back in.
    let default_in = host.default_input_device().map(|d| device_name(&d)).filter(|n| !is_virtual_cable(n));
    let input = inputs
        .iter()
        .find(|n| n.to_lowercase().contains("quadcast"))
        .cloned()
        .or_else(|| default_in.clone().filter(|n| !is_monitor(n)))
        .or_else(|| inputs.iter().find(|n| !is_virtual_cable(n) && !is_monitor(n)).cloned())
        .or(default_in)
        // Setups whose only mic is a monitor source: better than none, and the user can change it.
        .or_else(|| inputs.iter().find(|n| !is_virtual_cable(n)).cloned());
    let send = outputs
        .iter()
        .find(|n| is_send_device(n) || n.to_lowercase().starts_with("cable input"))
        .or_else(|| outputs.iter().find(|n| is_virtual_cable(n)))
        .cloned();
    let monitor = quadcast_out.or(default_out);
    match send {
        Some(send) => (input, Some(send), monitor),
        // No virtual cable: the headphones are the only output.
        None => (input, monitor, None),
    }
}

pub struct Engine {
    _input: cpal::Stream,
    _outputs: Vec<cpal::Stream>,
    dsp: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    /// Soundboard commands for the DSP thread.
    pub board: rtrb::Producer<Command>,
    /// False when only the send output exists, so a preview has nowhere to go.
    pub has_monitor: bool,
    /// Algorithmic latency of the chain alone (ms); output buffering is in `Shared::buffer_ms`.
    pub chain_latency_ms: f32,
    pub input_desc: String,
    pub output_desc: String,
}

impl Engine {
    /// `send` always receives the processed voice; `monitor` (if any) gets it only while
    /// `Shared::monitor` is on.
    pub fn start(input: &str, send: &str, monitor: Option<&str>, shared: Arc<Shared>) -> Result<Self> {
        let main = host(HostRole::Main);
        let in_dev = main.input_devices().inspect_err(|_| reset_hosts())?.find(|d| device_name(d) == input);
        let in_dev = in_dev.with_context(|| format!("input device '{input}' not found"))?;

        let in_def = in_dev.default_input_config()?;
        // The Linux sound server converts rate and format for us.
        if in_def.sample_rate() != SAMPLE_RATE && !cfg!(target_os = "linux") {
            bail!("input runs at {} Hz; set the mic to 48000 Hz in the Windows sound settings", in_def.sample_rate());
        }
        let in_ch = in_def.channels() as usize;
        let mut in_cfg = in_def.config();
        in_cfg.sample_rate = SAMPLE_RATE;
        in_cfg.buffer_size = stream_buffer();
        let in_format = if cfg!(target_os = "linux") { cpal::SampleFormat::F32 } else { in_def.sample_format() };

        // Each start is a fresh session for the dropout counter and buffer size.
        shared.underruns.store(0, Ordering::Relaxed);
        shared.overflows.store(0, Ordering::Relaxed);
        shared.buffer_ms.store((TARGET_FILL / 48) as u32, Ordering::Relaxed);

        // ---------- outputs ----------
        let mut producers = Vec::new();
        let mut outputs = Vec::new();
        let mut descs = Vec::new();
        let monitor = monitor.filter(|m| *m != send);
        for (name, is_monitor) in std::iter::once((send, false)).chain(monitor.map(|m| (m, true))) {
            let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING);
            // One playback stream per connection (see `host`).
            let out_host = host(if is_monitor { HostRole::Monitor } else { HostRole::Send });
            let (stream, ch) = build_output(&out_host, name, consumer, is_monitor, shared.clone())?;
            producers.push((producer, is_monitor));
            outputs.push(stream);
            descs.push(format!("{}{name} · {ch} ch", if is_monitor { "monitor: " } else { "" }));
        }

        // ---------- DSP thread: raw ring → chain → output rings ----------
        // The capture callback only copies samples; all processing (incl. the
        // DeepFilterNet model) runs here, on an MMCSS "Pro Audio" thread, so a slow
        // block can never make the capture device glitch.
        let (mut raw_tx, mut raw_rx) = rtrb::RingBuffer::<f32>::new(RING);
        let (board, mut board_rx) = rtrb::RingBuffer::<Command>::new(soundboard::QUEUE);
        let has_monitor = monitor.is_some();
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<f32>();
        let sh = shared.clone();
        let stop_dsp = stop.clone();
        let dsp = std::thread::Builder::new().name("mw-dsp".into()).spawn(move || {
            let _mmcss = boost_thread();
            if std::env::var_os("MW_DEBUG").is_some() {
                eprintln!("dsp thread MMCSS boost: {}", if _mmcss.is_some() { "ok" } else { "FAILED" });
            }
            // The chain (and its noise removers) is built on this thread: they needn't be Send.
            let mut chain = Chain::new(SAMPLE_RATE);
            chain.set_params(&sh.params.lock().unwrap());
            let _ = ready_tx.send(chain.latency() as f32 / 48.0);
            let mut seen_gen = 0u32;
            let mut block = [0.0f32; BLOCK];
            let mut raw = [0.0f32; BLOCK];
            let mut mixer = Mixer::new();
            let mut muted = false;
            let mut send_block = [0.0f32; BLOCK];
            let mut monitor_block = [0.0f32; BLOCK];
            // Overload guard: smoothed processing time per 10 ms block.
            let mut avg_ms = 0.0f32;
            let mut blocks_since_switch = 0u32;
            while !stop_dsp.load(Ordering::Acquire) {
                if raw_rx.slots() < BLOCK {
                    std::thread::park_timeout(Duration::from_millis(5));
                    continue;
                }
                let started = std::time::Instant::now();
                for s in block.iter_mut() {
                    *s = raw_rx.pop().unwrap_or(0.0);
                }
                let g = sh.generation.load(Ordering::Acquire);
                if g != seen_gen {
                    if let Ok(p) = sh.params.try_lock() {
                        chain.set_params(&p);
                        muted = p.mute != 0;
                        seen_gen = g;
                    }
                }
                raw.copy_from_slice(&block);
                chain.process_mono(&mut block);

                // If the PC is so busy that the high-quality model eats most of the 10 ms
                // budget, fall back to RNNoise rather than glitch; retry 30 s later.
                avg_ms += (started.elapsed().as_secs_f32() * 1000.0 - avg_ms) * 0.02;
                blocks_since_switch += 1;
                let forced = sh.light_forced.load(Ordering::Relaxed);
                if !forced && avg_ms > 6.0 && blocks_since_switch > 200 {
                    chain.force_light_model(true);
                    sh.light_forced.store(true, Ordering::Relaxed);
                    blocks_since_switch = 0;
                } else if forced && blocks_since_switch > 3_000 {
                    chain.force_light_model(false);
                    sh.light_forced.store(false, Ordering::Relaxed);
                    blocks_since_switch = 0;
                }

                if let Ok(mut m) = sh.meters.try_lock() {
                    *m = *chain.meters();
                }
                if let Ok(mut rec) = sh.recording.try_lock() {
                    if let Some(r) = rec.as_mut() {
                        // Only reserved capacity is used (see Controller::start_recording).
                        let room = (r.raw.capacity() - r.raw.len()).min(BLOCK);
                        r.raw.extend_from_slice(&raw[..room]);
                        r.processed.extend_from_slice(&block[..room]);
                    }
                }
                // After the recorder, so an A/B sample stays a comparison of the voice alone.
                while let Ok(command) = board_rx.pop() {
                    mixer.command(command);
                }
                let monitor_on = sh.monitor.load(Ordering::Relaxed);
                let clip_send = f32::from_bits(sh.board_send.load(Ordering::Relaxed));
                let clip_monitor = f32::from_bits(sh.board_monitor.load(Ordering::Relaxed));
                mixer.mix(&block, muted, monitor_on, clip_send, clip_monitor, &mut send_block, &mut monitor_block);
                if let Ok(mut playing) = sh.board_playing.try_lock() {
                    mixer.playing(&mut playing);
                }
                // Stays fed with Hear Myself off, so clips start at once and stopping
                // them doesn't count as a dropout that grows the monitor's buffer.
                let feed_monitor = monitor_on || clip_monitor > 0.0;
                for (producer, is_monitor) in producers.iter_mut() {
                    if *is_monitor && !feed_monitor {
                        continue;
                    }
                    if producer.slots() < BLOCK {
                        sh.overflows.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    for &s in if *is_monitor { &monitor_block } else { &send_block } {
                        let _ = producer.push(s);
                    }
                }
            }
        })?;
        let chain_latency_ms = ready_rx.recv_timeout(Duration::from_secs(10)).context("audio processing thread failed to start")?;
        let dsp_thread = dsp.thread().clone();

        // ---------- capture: downmix to mono → raw ring ----------
        let sh = shared.clone();
        let mut on_input = move |frames: &mut dyn Iterator<Item = f32>| {
            let mut acc = 0.0;
            let mut dropped = false;
            for (i, s) in frames.enumerate() {
                acc += s;
                if i % in_ch == in_ch - 1 {
                    dropped |= raw_tx.push(acc / in_ch as f32).is_err();
                    acc = 0.0;
                }
            }
            if dropped {
                // One glitch per callback, not one per lost sample.
                sh.overflows.fetch_add(1, Ordering::Relaxed);
            }
            dsp_thread.unpark();
        };
        let err_in = |e| eprintln!("input stream error: {e}");
        let input_stream = match in_format {
            cpal::SampleFormat::F32 => in_dev.build_input_stream(
                in_cfg,
                move |d: &[f32], _: &_| on_input(&mut d.iter().copied()),
                err_in,
                None,
            ),
            cpal::SampleFormat::I16 => in_dev.build_input_stream(
                in_cfg,
                move |d: &[i16], _: &_| on_input(&mut d.iter().map(|&s| s as f32 / 32768.0)),
                err_in,
                None,
            ),
            f => bail!("unsupported input sample format {f:?}"),
        }
        .with_context(|| format!("can't open the mic '{input}'"))?;

        input_stream.play().with_context(|| format!("can't start the mic '{input}'"))?;
        for o in &outputs {
            o.play().context("can't start an output")?;
        }
        Ok(Self {
            _input: input_stream,
            _outputs: outputs,
            dsp: Some(dsp),
            stop,
            board,
            has_monitor,
            chain_latency_ms,
            input_desc: format!("{input} · {in_ch} ch · {in_format:?}"),
            output_desc: descs.join(" + "),
        })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.dsp.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

/// Registers the current thread with MMCSS ("Pro Audio") so Windows schedules it like
/// other real-time audio work. The registration lasts until the returned guard drops.
#[cfg(windows)]
fn boost_thread() -> Option<impl Drop> {
    use windows::core::w;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW};
    struct Mmcss(HANDLE);
    impl Drop for Mmcss {
        fn drop(&mut self) {
            unsafe {
                let _ = AvRevertMmThreadCharacteristics(self.0);
            }
        }
    }
    let mut index = 0u32;
    unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) }.ok().map(Mmcss)
}

#[cfg(not(windows))]
fn boost_thread() -> Option<()> {
    None
}

/// The Linux sound server's default buffer is up to 2 s, far too much for a live voice;
/// ask for one 10 ms block. WASAPI's default is already short.
fn stream_buffer() -> cpal::BufferSize {
    if cfg!(target_os = "linux") {
        cpal::BufferSize::Fixed(BLOCK as u32)
    } else {
        cpal::BufferSize::Default
    }
}

fn build_output(
    host: &cpal::Host,
    name: &str,
    consumer: rtrb::Consumer<f32>,
    is_monitor: bool,
    shared: Arc<Shared>,
) -> Result<(cpal::Stream, usize)> {
    let dev = host
        .output_devices()?
        .find(|d| device_name(d) == name)
        .with_context(|| format!("output device '{name}' not found"))?;
    let def = dev.default_output_config()?;
    let ch = def.channels() as usize;
    let mut cfg = def.config();
    // WASAPI shared mode and the Linux sound server convert rates for us.
    cfg.sample_rate = SAMPLE_RATE;
    cfg.buffer_size = stream_buffer();
    let mut drain = DriftDrain::new(consumer);
    let stream = dev.build_output_stream(
        cfg,
        move |out: &mut [f32], _: &_| {
            let underrun = drain.fill(out, ch);
            // A monitor that was switched off runs dry on purpose: not a dropout.
            if underrun && (!is_monitor || shared.monitor.load(Ordering::Relaxed)) {
                shared.underruns.fetch_add(1, Ordering::Relaxed);
            }
            shared.buffer_ms.fetch_max((drain.target / 48) as u32, Ordering::Relaxed);
        },
        |e| eprintln!("output stream error: {e}"),
        None,
    )
    .with_context(|| format!("can't open the output '{name}'"))?;
    Ok((stream, ch))
}

/// Reads a ring written on another device's clock, resampling by up to ±0.5 % so the
/// ring stays near its target fill. Linear interpolation is transparent for voice.
///
/// The target adapts: it never sits below 1.25× the largest chunk the device asks for,
/// and grows by one block after every underrun (up to `MAX_TARGET`), so a busy system
/// trades a few ms of latency for glitch-free audio instead of crackling.
struct DriftDrain {
    ring: rtrb::Consumer<f32>,
    playing: bool,
    a: f32,
    b: f32,
    frac: f64,
    avg_fill: f64,
    target: usize,
}

impl DriftDrain {
    fn new(ring: rtrb::Consumer<f32>) -> Self {
        Self { ring, playing: false, a: 0.0, b: 0.0, frac: 0.0, avg_fill: TARGET_FILL as f64, target: TARGET_FILL }
    }

    /// Fills interleaved `out`; returns true if it ran dry (underrun).
    fn fill(&mut self, out: &mut [f32], channels: usize) -> bool {
        let request = out.len() / channels.max(1);
        self.target = self.target.max((request * 5 / 4 + BLOCK).min(MAX_TARGET));
        let target = self.target;

        let fill = self.ring.slots();
        if !self.playing {
            if fill < target {
                out.fill(0.0);
                return false;
            }
            self.playing = true;
            self.avg_fill = fill as f64;
            self.a = self.ring.pop().unwrap_or(0.0);
            self.b = self.ring.pop().unwrap_or(0.0);
            self.frac = 0.0;
        }
        // Way too much buffered (e.g. the output stalled): jump back to the target.
        if fill > target * 8 {
            for _ in 0..fill - target {
                let _ = self.ring.pop();
            }
        }

        // Smoothed fill error → playback speed. More buffered than wanted = read faster.
        self.avg_fill += (self.ring.slots() as f64 - self.avg_fill) * 0.05;
        let error = (self.avg_fill - target as f64) / target as f64;
        let step = 1.0 + (error * 0.01).clamp(-MAX_CORRECTION, MAX_CORRECTION);

        let mut underrun = false;
        for frame in out.chunks_mut(channels) {
            let s = if self.playing { self.a + (self.b - self.a) * self.frac as f32 } else { 0.0 };
            frame.fill(s);
            if !self.playing {
                continue;
            }
            self.frac += step;
            while self.frac >= 1.0 {
                self.frac -= 1.0;
                self.a = self.b;
                match self.ring.pop() {
                    Ok(v) => self.b = v,
                    Err(_) => {
                        // Ran dry: go silent, buffer a little more, and restart.
                        self.playing = false;
                        underrun = true;
                        self.target = (self.target + BLOCK).min(MAX_TARGET);
                        break;
                    }
                }
            }
        }
        underrun
    }
}

pub fn write_wav(path: &std::path::Path, x: &[f32]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in x {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Producer slightly faster than the consumer (clock drift): the drain must keep
    /// playing without underruns and hold the ring near its target.
    #[test]
    fn drift_drain_tracks_clock_mismatch() {
        let (mut p, c) = rtrb::RingBuffer::<f32>::new(RING);
        let mut d = DriftDrain::new(c);
        let mut out = vec![0.0f32; 480 * 2];
        let mut underruns = 0;
        let mut t = 0.0f64;
        // Input delivers 480.6 samples per 480 output frames (+0.125 % drift).
        let mut owed = 0.0f64;
        for _ in 0..6_000 {
            owed += 480.6;
            while owed >= 1.0 {
                owed -= 1.0;
                let _ = p.push((t * 0.05).sin() as f32 * 0.1);
                t += 1.0;
            }
            if d.fill(&mut out, 2) {
                underruns += 1;
            }
        }
        assert!(underruns <= 1, "underruns: {underruns}");
        let fill = d.ring.slots() as i64;
        let target = d.target as i64;
        assert!((fill - target).abs() < target, "fill drifted to {fill} (target {target})");
    }

    /// After an underrun the buffer grows, so the same stall doesn't glitch twice.
    #[test]
    fn drift_drain_grows_after_underrun() {
        let (mut p, c) = rtrb::RingBuffer::<f32>::new(RING);
        let mut d = DriftDrain::new(c);
        let mut out = vec![0.0f32; 480 * 2];
        for _ in 0..TARGET_FILL {
            let _ = p.push(0.1);
        }
        let before = d.target;
        let mut underruns = 0;
        for _ in 0..10 {
            underruns += d.fill(&mut out, 2) as u32;
        }
        assert!(underruns >= 1);
        assert!(d.target > before, "target stayed at {before}");
        assert!(d.target <= MAX_TARGET);
    }
}

//! Baseline measurement + offline processing.
//!
//! ```text
//! analyze record  -o recordings/baseline.wav -s 30
//! analyze stats   recordings/baseline.wav
//! analyze process recordings/baseline.wav recordings/processed.wav --preset far
//! ```

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use mw_dsp::{Chain, Params, SAMPLE_RATE};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(about = "MicWright measurement tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List input devices.
    Devices,
    /// Record the QuadCast (raw, mono, 48 kHz float) to a WAV file.
    Record {
        #[arg(short, long, default_value = "recordings/baseline.wav")]
        out: PathBuf,
        #[arg(short, long, default_value_t = 30)]
        seconds: u64,
        /// Substring of the input device name.
        #[arg(short, long, default_value = "Quadcast")]
        device: String,
    },
    /// Print noise floor / speech level / SNR / loudness / peaks.
    Stats { input: PathBuf },
    /// Run the chain over a WAV and print before/after stats.
    Process {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, value_enum, default_value_t = Preset::Far)]
        preset: Preset,
        /// Noise removal model.
        #[arg(long, value_enum, default_value_t = Model::Dfn)]
        model: Model,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Model {
    Rnnoise,
    Dfn,
}

#[derive(Clone, Copy, ValueEnum)]
enum Preset {
    Far,
    Quiet,
    Noisy,
    Beatbox,
    Bypass,
}

impl Preset {
    fn params(self) -> Params {
        match self {
            Preset::Far => Params::far_mic(),
            Preset::Quiet => Params::quiet_room(),
            Preset::Noisy => Params::noisy_room(),
            Preset::Beatbox => Params::beatbox(),
            Preset::Bypass => Params { enabled: 0, ..Params::far_mic() },
        }
    }
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Devices => devices(),
        Cmd::Record { out, seconds, device } => record(&out, seconds, &device),
        Cmd::Stats { input } => {
            let (x, sr) = read_mono(&input)?;
            print_stats(&input.display().to_string(), &stats(&x, sr)?);
            Ok(())
        }
        Cmd::Process { input, output, preset, model } => process(&input, &output, preset, model),
    }
}

fn device_name(d: &cpal::Device) -> String {
    d.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "<unknown>".into())
}

fn devices() -> Result<()> {
    let host = cpal::default_host();
    for d in host.input_devices()? {
        let cfg = d.default_input_config().map(|c| format!("{c:?}")).unwrap_or_default();
        println!("{}  {cfg}", device_name(&d));
    }
    Ok(())
}

fn record(out: &Path, seconds: u64, needle: &str) -> Result<()> {
    let host = cpal::default_host();
    let dev = host
        .input_devices()?
        .find(|d| device_name(d).to_lowercase().contains(&needle.to_lowercase()))
        .with_context(|| format!("no input device matching '{needle}' (try `analyze devices`)"))?;
    let cfg = dev.default_input_config()?;
    let channels = cfg.channels() as usize;
    let sr = cfg.sample_rate();
    println!("Recording {} ({} ch, {} Hz, {:?}) for {seconds}s → {}", device_name(&dev), channels, sr, cfg.sample_format(), out.display());
    println!("Script: 5s silence · 10s normal talking · 5s loud · 5s typing · 5s silence");

    let samples = Arc::new(Mutex::new(Vec::<f32>::with_capacity(sr as usize * seconds as usize)));
    let sink = samples.clone();
    let err = |e| eprintln!("stream error: {e}");
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev.build_input_stream(
            cfg.config(),
            move |data: &[f32], _: &_| push_mono(&sink, data.iter().copied(), channels),
            err,
            None,
        )?,
        cpal::SampleFormat::I16 => dev.build_input_stream(
            cfg.config(),
            move |data: &[i16], _: &_| push_mono(&sink, data.iter().map(|&s| s as f32 / 32768.0), channels),
            err,
            None,
        )?,
        f => bail!("unsupported sample format {f:?}"),
    };
    stream.play()?;
    for s in (1..=seconds).rev() {
        print!("\r  {s:3}s left ");
        use std::io::Write;
        std::io::stdout().flush().ok();
        std::thread::sleep(Duration::from_secs(1));
    }
    drop(stream);
    println!();

    let x = samples.lock().unwrap().clone();
    write_mono(out, &x, sr)?;
    print_stats(&out.display().to_string(), &stats(&x, sr)?);
    Ok(())
}

fn push_mono(sink: &Mutex<Vec<f32>>, data: impl Iterator<Item = f32>, channels: usize) {
    let v: Vec<f32> = data.collect();
    let mut s = sink.lock().unwrap();
    for f in v.chunks_exact(channels) {
        s.push(f.iter().sum::<f32>() / channels as f32);
    }
}

fn process(input: &Path, output: &Path, preset: Preset, model: Model) -> Result<()> {
    let (mut x, sr) = read_mono(input)?;
    if sr != SAMPLE_RATE {
        bail!("input is {sr} Hz; the chain runs at 48 kHz");
    }
    let before = stats(&x, sr)?;
    let mut chain = Chain::new(SAMPLE_RATE);
    let mut params = preset.params();
    params.ns_high_quality = matches!(model, Model::Dfn) as u32;
    chain.set_params(&params);
    if matches!(model, Model::Dfn) && !chain.high_quality_available() {
        bail!("DeepFilterNet3 model not available");
    }
    let lat = chain.latency();
    let t = std::time::Instant::now();
    for block in x.chunks_mut(480) {
        chain.process_mono(block);
    }
    let elapsed = t.elapsed();
    // Remove the algorithmic delay so before/after line up.
    x.drain(..lat.min(x.len()));
    write_mono(output, &x, sr)?;
    let after = stats(&x, sr)?;
    print_stats("before", &before);
    print_stats("after", &after);
    let audio_s = x.len() as f64 / sr as f64;
    println!(
        "\nlatency {:.1} ms · CPU {:.2}% of one core ({:.0}x real-time)",
        lat as f64 / sr as f64 * 1000.0,
        elapsed.as_secs_f64() / audio_s * 100.0,
        audio_s / elapsed.as_secs_f64()
    );
    println!("SNR change: {:+.1} dB · noise floor change: {:+.1} dB", after.snr_db - before.snr_db, after.noise_floor_db - before.noise_floor_db);
    Ok(())
}

struct Stats {
    seconds: f64,
    noise_floor_db: f64,
    speech_db: f64,
    snr_db: f64,
    lufs: f64,
    true_peak_db: f64,
    sample_peak_db: f64,
    clipped: usize,
}

fn stats(x: &[f32], sr: u32) -> Result<Stats> {
    // 50 ms window RMS levels.
    let win = (sr / 20) as usize;
    let mut levels: Vec<f64> = x
        .chunks(win)
        .filter(|c| c.len() == win)
        .map(|c| 10.0 * (c.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / win as f64).max(1e-12).log10())
        .collect();
    if levels.is_empty() {
        bail!("recording too short");
    }
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| levels[((levels.len() - 1) as f64 * p) as usize];
    let noise_floor_db = pct(0.10);
    let speech_db = pct(0.90);

    let mut meter = ebur128::EbuR128::new(1, sr, ebur128::Mode::I | ebur128::Mode::TRUE_PEAK)?;
    meter.add_frames_f32(x)?;
    let lufs = meter.loudness_global()?;
    let tp = meter.true_peak(0)?;
    let peak = x.iter().fold(0f32, |m, s| m.max(s.abs())) as f64;
    Ok(Stats {
        seconds: x.len() as f64 / sr as f64,
        noise_floor_db,
        speech_db,
        snr_db: speech_db - noise_floor_db,
        lufs,
        true_peak_db: 20.0 * tp.max(1e-12).log10(),
        sample_peak_db: 20.0 * peak.max(1e-12).log10(),
        clipped: x.iter().filter(|s| s.abs() >= 0.999).count(),
    })
}

fn print_stats(label: &str, s: &Stats) {
    println!("\n── {label} ({:.1}s)", s.seconds);
    println!("  noise floor (p10, 50ms RMS) {:7.1} dBFS", s.noise_floor_db);
    println!("  speech level (p90)          {:7.1} dBFS", s.speech_db);
    println!("  SNR                         {:7.1} dB", s.snr_db);
    println!("  integrated loudness         {:7.1} LUFS", s.lufs);
    println!("  true peak                   {:7.1} dBTP", s.true_peak_db);
    println!("  sample peak                 {:7.1} dBFS  ({} clipped samples)", s.sample_peak_db, s.clipped);
    let mut tips = Vec::new();
    if s.sample_peak_db < -20.0 {
        tips.push("Very quiet input: raise the gain knob (bottom of the mic) and set the Windows level to 100.");
    }
    if s.clipped > 0 {
        tips.push("Clipping: lower the gain knob a little.");
    }
    if s.snr_db < 25.0 {
        tips.push("Low SNR: speak into the SIDE (logo) of the mic, set the pattern to cardioid, move it closer.");
    }
    for t in tips {
        println!("  ⚠ {t}");
    }
}

fn read_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = r.spec();
    let ch = spec.channels as usize;
    let all: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    Ok((all.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect(), spec.sample_rate))
}

fn write_mono(path: &Path, x: &[f32], sr: u32) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let spec = hound::WavSpec { channels: 1, sample_rate: sr, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in x {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(())
}

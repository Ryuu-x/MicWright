//! Plays an A/B recording back on the user's headphones.

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

pub struct Player {
    _stream: cpal::Stream,
    done: Arc<AtomicBool>,
}

impl Player {
    /// Starts playing a mono 48 kHz WAV on `device` (or the default output).
    pub fn play(path: &Path, device: Option<&str>) -> Result<Player> {
        let samples: Vec<f32> = hound::WavReader::open(path)
            .with_context(|| format!("can't open {}", path.display()))?
            .into_samples::<f32>()
            .collect::<Result<_, _>>()?;
        let host = mw_engine::audio::host(mw_engine::audio::HostRole::Player);
        let dev = device
            .and_then(|name| host.output_devices().ok()?.find(|d| mw_engine::audio::device_name(d) == name))
            .or_else(|| host.default_output_device())
            .context("no speakers or headphones found")?;
        let mut cfg = dev.default_output_config()?.config();
        cfg.sample_rate = mw_dsp::SAMPLE_RATE;
        let ch = cfg.channels as usize;
        let done = Arc::new(AtomicBool::new(false));
        let pos = AtomicUsize::new(0);
        let finished = done.clone();
        let stream = dev.build_output_stream(
            cfg,
            move |out: &mut [f32], _: &_| {
                let mut at = pos.load(Ordering::Relaxed);
                for frame in out.chunks_mut(ch) {
                    frame.fill(samples.get(at).copied().unwrap_or(0.0));
                    at += 1;
                }
                pos.store(at, Ordering::Relaxed);
                if at >= samples.len() {
                    finished.store(true, Ordering::Relaxed);
                }
            },
            |e| eprintln!("playback error: {e}"),
            None,
        )?;
        stream.play()?;
        Ok(Player { _stream: stream, done })
    }

    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

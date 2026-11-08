//! AI noise suppression, wrapped for per-sample processing.
//!
//! * **RNNoise** (pure-Rust `nnnoiseless`) always runs: it's very cheap and its
//!   voice-activity probability drives the AGC and noise-floor tracker.
//! * **DeepFilterNet3** (feature `deepfilter`) is the high-quality path: it removes far
//!   more noise and some room echo, at a few % of one CPU core.
//!
//! Both work on 480-sample frames (10 ms @ 48 kHz). Each path has its own algorithmic
//! delay, so both — and the dry signal — are delayed to one common latency. Switching
//! models therefore never shifts timing, and wet/dry mixing never comb-filters.

use nnnoiseless::DenoiseState;

pub const FRAME: usize = DenoiseState::FRAME_SIZE;
const SCALE: f32 = 32_767.0;
/// RNNoise path: one frame of buffering plus one frame inside RNNoise's overlap-add.
const RNN_LATENCY: usize = 2 * FRAME;

/// Fixed-size delay line (no allocation after construction).
struct Delay {
    buf: Vec<f32>,
    pos: usize,
}

impl Delay {
    fn new(samples: usize) -> Self {
        Self { buf: vec![0.0; samples.max(1)], pos: 0 }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        if self.buf.len() == 1 {
            return x;
        }
        let y = self.buf[self.pos];
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }
}

struct RnNoise {
    state: Box<DenoiseState<'static>>,
    input: [f32; FRAME],
    prev_input: [f32; FRAME],
    wet: [f32; FRAME],
    dry: [f32; FRAME],
    idx: usize,
    first: bool,
    vad: f32,
}

impl RnNoise {
    fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            input: [0.0; FRAME],
            prev_input: [0.0; FRAME],
            wet: [0.0; FRAME],
            dry: [0.0; FRAME],
            idx: 0,
            first: true,
            vad: 0.0,
        }
    }

    /// `(denoised, dry)`, both delayed by exactly `RNN_LATENCY`.
    #[inline]
    fn process(&mut self, x: f32) -> (f32, f32) {
        let out = (self.wet[self.idx], self.dry[self.idx]);
        self.input[self.idx] = x * SCALE;
        self.idx += 1;
        if self.idx == FRAME {
            self.idx = 0;
            let mut frame = [0.0f32; FRAME];
            self.vad = self.state.process_frame(&mut frame, &self.input);
            if self.first {
                // RNNoise's very first frame is a fade-in artefact.
                self.first = false;
                frame.fill(0.0);
            }
            for i in 0..FRAME {
                self.wet[i] = frame[i] / SCALE;
                self.dry[i] = self.prev_input[i] / SCALE;
            }
            self.prev_input = self.input;
        }
        out
    }
}

#[cfg(feature = "deepfilter")]
mod dfn {
    use super::FRAME;
    use df::tract::{DfParams, DfTract, RuntimeParams};
    use ndarray::Array2;

    pub struct DeepFilter {
        model: DfTract,
        input: Array2<f32>,
        output: Array2<f32>,
        ready: [f32; FRAME],
        idx: usize,
        ok: bool,
        /// Model latency in samples, on top of our one frame of buffering.
        pub model_latency: usize,
    }

    impl DeepFilter {
        pub fn new() -> Option<Self> {
            let model = DfTract::new(DfParams::default(), &RuntimeParams::default_with_ch(1)).ok()?;
            if model.hop_size != FRAME || model.sr != 48_000 {
                return None;
            }
            // STFT synthesis delay (fft - hop) plus the model's look-ahead frames.
            let model_latency = (model.fft_size - model.hop_size) + model.lookahead * model.hop_size;
            let mut model = model;
            // Warm up: the first inferences are much slower (allocations, caches). Do them
            // now, off the audio path. Low-level noise, because silence is skipped.
            let mut seed = 1u32;
            let warm = Array2::from_shape_fn((1, FRAME), |_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / 16_777_216.0 - 0.5) * 0.01
            });
            let mut out = Array2::zeros((1, FRAME));
            for _ in 0..20 {
                let _ = model.process(warm.view(), out.view_mut());
            }
            Some(Self {
                model,
                input: Array2::zeros((1, FRAME)),
                output: Array2::zeros((1, FRAME)),
                ready: [0.0; FRAME],
                idx: 0,
                ok: true,
                model_latency,
            })
        }

        pub fn latency(&self) -> usize {
            FRAME + self.model_latency
        }

        /// Runs the model only while `active`, so it costs nothing when switched off.
        #[inline]
        pub fn process(&mut self, x: f32, active: bool) -> f32 {
            let y = self.ready[self.idx];
            self.input[[0, self.idx]] = x;
            self.idx += 1;
            if self.idx == FRAME {
                self.idx = 0;
                if active && self.ok {
                    match self.model.process(self.input.view(), self.output.view_mut()) {
                        Ok(_) => {
                            for (r, o) in self.ready.iter_mut().zip(self.output.iter()) {
                                *r = if o.is_finite() { *o } else { 0.0 };
                            }
                        }
                        Err(_) => {
                            // Never take the audio down: fall back to RNNoise for good.
                            self.ok = false;
                            self.ready.fill(0.0);
                        }
                    }
                } else {
                    self.ready.fill(0.0);
                }
            }
            y
        }

        pub fn healthy(&self) -> bool {
            self.ok
        }

        /// Caps how far DeepFilterNet may attenuate (dB). Used by tests to keep a
        /// synthetic signal flowing through the model's real delay path.
        #[cfg(test)]
        pub fn set_atten_lim(&mut self, db: f32) {
            self.model.set_atten_lim(db);
        }
    }
}

pub struct Denoiser {
    rnn: RnNoise,
    #[cfg(feature = "deepfilter")]
    dfn: Option<dfn::DeepFilter>,
    /// Pads the RNNoise path (wet + dry) up to the common latency.
    rnn_wet_pad: Delay,
    dry_pad: Delay,
    /// Pads the DeepFilterNet path up to the common latency.
    #[cfg(feature = "deepfilter")]
    dfn_pad: Delay,
    latency: usize,
    /// Use DeepFilterNet3 when available.
    pub high_quality: bool,
    pub vad: f32,
}

impl Denoiser {
    pub fn new() -> Self {
        #[cfg(feature = "deepfilter")]
        let dfn = dfn::DeepFilter::new();
        #[cfg(feature = "deepfilter")]
        let dfn_latency = dfn.as_ref().map(|d| d.latency()).unwrap_or(0);
        #[cfg(not(feature = "deepfilter"))]
        let dfn_latency = 0;

        let latency = RNN_LATENCY.max(dfn_latency);
        Self {
            rnn: RnNoise::new(),
            rnn_wet_pad: Delay::new(latency - RNN_LATENCY),
            dry_pad: Delay::new(latency - RNN_LATENCY),
            #[cfg(feature = "deepfilter")]
            dfn_pad: Delay::new(latency - dfn_latency.min(latency)),
            #[cfg(feature = "deepfilter")]
            dfn,
            latency,
            high_quality: true,
            vad: 0.0,
        }
    }

    pub fn latency(&self) -> usize {
        self.latency
    }

    #[cfg(all(test, feature = "deepfilter"))]
    fn limit_dfn(&mut self, db: f32) {
        if let Some(d) = self.dfn.as_mut() {
            d.set_atten_lim(db);
        }
    }

    /// True when the DeepFilterNet3 model is compiled in and loaded.
    pub fn high_quality_available(&self) -> bool {
        #[cfg(feature = "deepfilter")]
        return self.dfn.as_ref().is_some_and(|d| d.healthy());
        #[cfg(not(feature = "deepfilter"))]
        false
    }

    /// Returns `(denoised, dry)` — both delayed by exactly `latency()` and time-aligned.
    #[inline]
    pub fn process(&mut self, x: f32) -> (f32, f32) {
        let (rnn_wet, rnn_dry) = self.rnn.process(x);
        self.vad = self.rnn.vad;
        let dry = self.dry_pad.process(rnn_dry);
        let rnn_wet = self.rnn_wet_pad.process(rnn_wet);

        #[cfg(feature = "deepfilter")]
        if let Some(d) = self.dfn.as_mut() {
            let use_dfn = self.high_quality && d.healthy();
            let wet = self.dfn_pad.process(d.process(x, use_dfn));
            return (if use_dfn { wet } else { rnn_wet }, dry);
        }
        (rnn_wet, dry)
    }
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Voice-like test signal: a harmonic stack whose pitch glides (so it isn't periodic
    /// and cross-correlation has a single, unambiguous peak) with a syllable envelope.
    fn voice(n: usize) -> f32 {
        let t = n as f32 / 48_000.0;
        let env = 0.5 + 0.5 * (2.0 * std::f32::consts::PI * 4.0 * t).sin();
        // Phase of a pitch gliding 120..180 Hz: integral of f0(t) = 150 + 30 sin(0.7 t).
        let phase = 2.0 * std::f32::consts::PI * (150.0 * t - 30.0 / 0.7 * ((0.7 * t).cos() - 1.0));
        (1..12).map(|k| (phase * k as f32).sin() / k as f32).sum::<f32>() * 0.1 * env
    }

    fn best_lag(wet: &[f32], dry: &[f32]) -> i32 {
        let corr = |lag: i32| -> f32 { (20_000..90_000).map(|i| wet[i] * dry[(i as i32 - lag) as usize]).sum() };
        (-600..=600).max_by(|&a, &b| corr(a).partial_cmp(&corr(b)).unwrap()).unwrap()
    }

    /// Wet and dry outputs must be time-aligned for every model, otherwise the
    /// strength mix comb-filters.
    #[test]
    fn wet_and_dry_are_aligned() {
        for hq in [false, true] {
            let mut d = Denoiser::new();
            d.high_quality = hq;
            // Synthetic tones are "noise" to DeepFilterNet; limit its attenuation so the
            // signal still flows through its real delay path.
            #[cfg(feature = "deepfilter")]
            d.limit_dfn(6.0);
            let (mut wet, mut dry) = (Vec::new(), Vec::new());
            for n in 0..96_000 {
                let (w, dr) = d.process(voice(n));
                wet.push(w);
                dry.push(dr);
            }
            let lag = best_lag(&wet, &dry);
            // Band filtering shifts phase by a few samples; a frame (480) is the bug.
            assert!(lag.abs() <= 3, "high_quality={hq}: wet lags dry by {lag} samples");
        }
    }

    #[test]
    fn dry_is_a_pure_delay() {
        let mut d = Denoiser::new();
        let lat = d.latency();
        let input: Vec<f32> = (0..9_600).map(voice).collect();
        let out: Vec<f32> = input.iter().map(|&x| d.process(x).1).collect();
        for n in lat..input.len() {
            assert!((out[n] - input[n - lat]).abs() < 1e-4, "n={n}");
        }
    }
}

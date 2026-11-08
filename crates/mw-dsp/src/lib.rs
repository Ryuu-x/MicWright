//! MicWright voice chain.
//!
//! ```text
//! in ─► HPF ─► AGC ─► noise removal ─► expander ─► compressor ─► EQ ─► de-esser
//!    ─► pitch ─► warmth ─► reverb ─► limiter ─► out
//!              ▲
//!        voice detector (RNNoise VAD, or level well above the tracked room floor)
//! ```
//!
//! The AGC sits *before* noise removal and the gate: a distant or low-gain mic
//! delivers speech so quiet that the noise removers treat it as noise and the gate closes
//! on it, so the level has to be brought up first.
//!
//! Everything is allocated in [`Chain::new`]; `process_*` never allocates or locks,
//! so it is safe to call from the audio engine's real-time thread.

pub mod biquad;
pub mod denoise;
pub mod dynamics;
pub mod fx;
pub mod limiter;
pub mod params;
pub mod util;

use biquad::Biquad;
use denoise::Denoiser;
use dynamics::{Agc, Compressor, DeEsser, Expander};
use fx::{PitchShifter, Reverb};
use limiter::Limiter;
pub use params::{Meters, Params};
use util::{db_to_lin, lin_to_db};

pub const SAMPLE_RATE: u32 = 48_000;

pub struct Chain {
    sr: f32,
    params: Params,

    hpf: [Biquad; 2],
    denoise: Denoiser,
    expander: Expander,
    agc: Agc,
    comp: Compressor,
    eq_bass: Biquad,
    eq_mud: Biquad,
    eq_presence: Biquad,
    eq_air: Biquad,
    deess: DeEsser,
    pitch: PitchShifter,
    reverb: Reverb,
    limiter: Limiter,

    // cached linear gains
    pre_gain: f32,
    makeup: f32,
    out_gain: f32,
    ceiling: f32,

    voice: VoiceDetector,
    /// Overload guard: use RNNoise even if DeepFilterNet3 is selected.
    light_forced: bool,

    // metering
    meter_acc: MeterAcc,
    meters: Meters,
}

/// Decides "someone is talking" for the AGC, per 10 ms block of (high-passed) input.
///
/// RNNoise's VAD is reliable at normal levels but misses very quiet speech, so a
/// level-based check against a minimum-statistics noise floor backs it up.
struct VoiceDetector {
    sq: f32,
    n: u32,
    /// Room noise floor in dBFS: drops instantly to quieter blocks, creeps up slowly.
    floor_db: f32,
    hangover: u32,
    active: bool,
}

impl VoiceDetector {
    const BLOCK: u32 = 480;
    /// Voice must be this far above the room floor to count without RNNoise's help…
    const MARGIN_DB: f32 = 10.0;
    /// …and above this absolute level, so blips in near-digital silence (a muted
    /// mic sits around -110 dBFS) never count as speech.
    const MIN_VOICE_DB: f32 = -70.0;
    /// Floor rise per block (0.5 dB/s), so speech can't drag it up quickly.
    const RISE_DB: f32 = 0.005;
    /// Keep the gate open for 300 ms after the last voiced block.
    const HANGOVER: u32 = 30;

    fn new() -> Self {
        // Start high: the first quiet block pulls the floor straight down to the real value.
        Self { sq: 0.0, n: 0, floor_db: 0.0, hangover: 0, active: false }
    }

    #[inline]
    fn push(&mut self, x: f32, vad: f32) {
        self.sq += x * x;
        self.n += 1;
        if self.n < Self::BLOCK {
            return;
        }
        let level_db = 10.0 * (self.sq / self.n as f32).max(1e-12).log10();
        self.sq = 0.0;
        self.n = 0;
        self.floor_db = if level_db < self.floor_db { level_db } else { self.floor_db + Self::RISE_DB };
        let loud_enough = level_db > Self::MIN_VOICE_DB;
        if loud_enough && (vad > 0.5 || level_db > self.floor_db + Self::MARGIN_DB) {
            self.hangover = Self::HANGOVER;
        } else {
            self.hangover = self.hangover.saturating_sub(1);
        }
        self.active = self.hangover > 0;
    }
}

#[derive(Default)]
struct MeterAcc {
    in_peak: f32,
    in_sq: f32,
    out_peak: f32,
    out_sq: f32,
    n: u32,
    noise_floor_db: f32,
}

impl Chain {
    /// The chain is designed for 48 kHz (RNNoise is trained at 48 kHz).
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut c = Self {
            sr,
            params: Params::default(),
            hpf: [Biquad::identity(); 2],
            denoise: Denoiser::new(),
            expander: Expander::new(sr),
            agc: Agc::new(sr),
            comp: Compressor::new(sr),
            eq_bass: Biquad::identity(),
            eq_mud: Biquad::identity(),
            eq_presence: Biquad::identity(),
            eq_air: Biquad::identity(),
            deess: DeEsser::new(sr, 5_500.0),
            pitch: PitchShifter::new(sr),
            reverb: Reverb::new(sr),
            limiter: Limiter::new(sr, 1.5, 60.0),
            pre_gain: 1.0,
            makeup: 1.0,
            out_gain: 1.0,
            ceiling: 1.0,
            voice: VoiceDetector::new(),
            light_forced: false,
            meter_acc: MeterAcc { noise_floor_db: -90.0, ..Default::default() },
            meters: Meters { noise_floor_db: -90.0, ..Default::default() },
        };
        c.apply_params(&Params::default(), true);
        c
    }

    /// Total algorithmic latency in samples (identical whether or not the chain is bypassed,
    /// so toggling never causes a timing jump).
    pub fn latency(&self) -> usize {
        self.denoise.latency() + self.limiter.latency()
    }

    /// Temporarily forces the lighter RNNoise model regardless of params (overload guard).
    pub fn force_light_model(&mut self, force: bool) {
        self.light_forced = force;
        self.denoise.high_quality = self.params.ns_high_quality != 0 && !force;
    }

    /// True when the DeepFilterNet3 model is available (feature `deepfilter`).
    pub fn high_quality_available(&self) -> bool {
        self.denoise.high_quality_available()
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    pub fn meters(&self) -> &Meters {
        &self.meters
    }

    /// Cheap if nothing changed; recomputes filter coefficients only for changed fields.
    pub fn set_params(&mut self, p: &Params) {
        if *p != self.params {
            self.apply_params(p, false);
        }
    }

    fn apply_params(&mut self, p: &Params, force: bool) {
        let old = self.params;
        let sr = self.sr;
        if force || p.hpf_hz != old.hpf_hz {
            // 4th-order Butterworth = two cascaded 2nd-order sections.
            self.hpf[0].highpass(sr, p.hpf_hz.max(10.0), 0.541_196_1);
            self.hpf[1].highpass(sr, p.hpf_hz.max(10.0), 1.306_563);
        }
        if force || p.eq_bass_db != old.eq_bass_db {
            self.eq_bass.low_shelf(sr, 120.0, p.eq_bass_db);
        }
        if force || p.eq_mud_hz != old.eq_mud_hz || p.eq_mud_db != old.eq_mud_db {
            self.eq_mud.peaking(sr, p.eq_mud_hz, 0.9, p.eq_mud_db);
        }
        if force || p.eq_presence_hz != old.eq_presence_hz || p.eq_presence_db != old.eq_presence_db {
            self.eq_presence.peaking(sr, p.eq_presence_hz, 0.8, p.eq_presence_db);
        }
        if force || p.eq_air_db != old.eq_air_db {
            self.eq_air.high_shelf(sr, 10_000.0, p.eq_air_db);
        }
        if force || p.comp_attack_ms != old.comp_attack_ms || p.comp_release_ms != old.comp_release_ms {
            self.comp.set_times(p.comp_attack_ms, p.comp_release_ms, sr);
        }
        self.denoise.high_quality = p.ns_high_quality != 0 && !self.light_forced;
        if force || p.agc_enabled != old.agc_enabled {
            self.agc.set_gain_db(0.0);
        }
        self.pre_gain = db_to_lin(p.pre_gain_db);
        self.makeup = db_to_lin(p.comp_makeup_db);
        self.out_gain = db_to_lin(p.output_gain_db);
        self.ceiling = db_to_lin(p.limiter_ceiling_db.min(0.0));
        self.params = *p;
    }

    /// Process one mono sample.
    #[inline]
    pub fn process_sample(&mut self, x: f32) -> f32 {
        let p = self.params;
        let x = if x.is_finite() { x } else { 0.0 };

        // Always run the denoiser so bypass toggling keeps identical latency.
        let mut s = x;
        if p.enabled != 0 {
            s = self.hpf[0].process(s);
            s = self.hpf[1].process(s);
            self.voice.push(s, self.denoise.vad);
            s = if p.agc_enabled != 0 {
                self.agc.process(s, self.voice.active, p.agc_target_db, p.agc_max_gain_db, p.agc_min_gain_db)
            } else {
                s * self.pre_gain
            };
        }
        let (wet, dry) = self.denoise.process(s);
        let vad = self.denoise.vad;

        let mut y;
        if p.enabled == 0 {
            y = self.delay_bypass(dry);
        } else {
            y = if p.ns_enabled != 0 { wet * p.ns_strength + dry * (1.0 - p.ns_strength) } else { dry };
            if p.exp_enabled != 0 {
                y = self.expander.process(y, p.exp_threshold_db, p.exp_ratio, p.exp_range_db);
            }
            if p.comp_enabled != 0 {
                y = self.comp.process(y, p.comp_threshold_db, p.comp_ratio, p.comp_knee_db) * self.makeup;
            }
            if p.eq_enabled != 0 {
                y = self.eq_bass.process(y);
                y = self.eq_mud.process(y);
                y = self.eq_presence.process(y);
                y = self.eq_air.process(y);
            }
            if p.deess_enabled != 0 {
                y = self.deess.process(y, p.deess_threshold_db, p.deess_max_db);
            }
            if p.fx_enabled != 0 {
                y = self.pitch.process(y, p.pitch_semitones.clamp(-12.0, 12.0));
                y = fx::warmth(y, p.warmth);
                y = self.reverb.process(y, p.reverb_mix, p.reverb_size, p.reverb_damping);
            }
            y = self.limiter.process(y * self.out_gain, self.ceiling);
        }

        if p.mute != 0 {
            y = 0.0;
        }
        self.meter(x, y, vad);
        y
    }

    /// In bypass the limiter isn't used, but its latency must still be honoured.
    #[inline]
    fn delay_bypass(&mut self, x: f32) -> f32 {
        // Reuse the limiter's delay with a ceiling it can't hit.
        self.limiter.process(x, f32::MAX)
    }

    /// Process mono audio in place.
    pub fn process_mono(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            *s = self.process_sample(*s);
        }
        self.finish_block();
    }

    /// Process interleaved audio in place: channels are averaged to mono, processed,
    /// and the result is written to every channel.
    pub fn process_interleaved(&mut self, buf: &mut [f32], channels: usize) {
        let ch = channels.max(1);
        let inv = 1.0 / ch as f32;
        for frame in buf.chunks_exact_mut(ch) {
            let mono = frame.iter().sum::<f32>() * inv;
            let y = self.process_sample(mono);
            frame.fill(y);
        }
        self.finish_block();
    }

    /// Process from separate input/output buffers (APO-style), interleaved.
    pub fn process_interleaved_into(&mut self, input: &[f32], output: &mut [f32], channels: usize) {
        let ch = channels.max(1);
        let inv = 1.0 / ch as f32;
        for (fi, fo) in input.chunks_exact(ch).zip(output.chunks_exact_mut(ch)) {
            let mono = fi.iter().sum::<f32>() * inv;
            let y = self.process_sample(mono);
            fo.fill(y);
        }
        self.finish_block();
    }

    #[inline]
    fn meter(&mut self, x: f32, y: f32, vad: f32) {
        let m = &mut self.meter_acc;
        m.in_peak = m.in_peak.max(x.abs());
        m.in_sq += x * x;
        m.out_peak = m.out_peak.max(y.abs());
        m.out_sq += y * y;
        m.n += 1;
        let _ = vad;
    }

    fn finish_block(&mut self) {
        let m = &mut self.meter_acc;
        if m.n == 0 {
            return;
        }
        // Report the combined voice decision (RNNoise VAD or level detector).
        let vad = if self.voice.active { self.denoise.vad.max(0.99) } else { self.denoise.vad };
        let in_rms_db = 10.0 * (m.in_sq / m.n as f32).max(1e-12).log10();
        m.noise_floor_db = self.voice.floor_db;
        self.meters = Meters {
            in_peak_db: lin_to_db(m.in_peak),
            in_rms_db,
            out_peak_db: lin_to_db(m.out_peak),
            out_rms_db: 10.0 * (m.out_sq / m.n as f32).max(1e-12).log10(),
            vad,
            noise_floor_db: m.noise_floor_db,
            agc_gain_db: self.agc.gain_db(),
            exp_gain_db: self.expander.gain_db,
            comp_gr_db: self.comp.gain_reduction_db,
            deess_gr_db: self.deess.gain_reduction_db,
            limiter_gr_db: self.limiter.gain_reduction_db,
            blocks: self.meters.blocks.wrapping_add(1),
        };
        m.in_peak = 0.0;
        m.out_peak = 0.0;
        m.in_sq = 0.0;
        m.out_sq = 0.0;
        m.n = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    }

    #[test]
    fn output_never_exceeds_ceiling_and_is_finite() {
        let mut c = Chain::new(SAMPLE_RATE);
        let ceiling = db_to_lin(c.params().limiter_ceiling_db);
        let mut seed = 1;
        let mut buf = vec![0.0f32; 480];
        for block in 0..400 {
            for (i, s) in buf.iter_mut().enumerate() {
                let t = (block * 480 + i) as f32 / 48_000.0;
                *s = 0.9 * (2.0 * std::f32::consts::PI * 220.0 * t).sin() + 0.2 * noise(&mut seed);
            }
            buf[7] = f32::NAN;
            c.process_mono(&mut buf);
            for &s in &buf {
                assert!(s.is_finite());
                assert!(s.abs() <= ceiling + 1e-5);
            }
        }
    }

    #[test]
    fn mute_outputs_silence() {
        let mut c = Chain::new(SAMPLE_RATE);
        let mut p = *c.params();
        p.mute = 1;
        c.set_params(&p);
        let mut buf = vec![0.5f32; 960];
        c.process_mono(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn bypass_is_a_pure_delay() {
        let mut c = Chain::new(SAMPLE_RATE);
        let mut p = *c.params();
        p.enabled = 0;
        c.set_params(&p);
        let lat = c.latency();
        let input: Vec<f32> = (0..9_600).map(|n| 0.25 * (n as f32 * 0.01).sin()).collect();
        let mut out = input.clone();
        c.process_mono(&mut out);
        for n in lat..input.len() {
            assert!((out[n] - input[n - lat]).abs() < 1e-5, "n={n}");
        }
    }

    #[test]
    fn interleaved_writes_all_channels() {
        let mut c = Chain::new(SAMPLE_RATE);
        let mut buf: Vec<f32> = (0..9_600).map(|n| ((n / 2) as f32 * 0.02).sin() * 0.1).collect();
        c.process_interleaved(&mut buf, 2);
        for f in buf.chunks_exact(2) {
            assert_eq!(f[0], f[1]);
        }
    }

    #[test]
    fn noise_floor_meter_tracks_silence() {
        let mut c = Chain::new(SAMPLE_RATE);
        let mut seed = 7;
        let mut buf = vec![0.0f32; 480];
        for _ in 0..500 {
            for s in buf.iter_mut() {
                *s = 0.001 * noise(&mut seed); // ~ -65 dBFS RMS
            }
            c.process_mono(&mut buf);
        }
        let nf = c.meters().noise_floor_db;
        assert!((-72.0..-58.0).contains(&nf), "noise floor {nf}");
    }
    /// Regression: a quiet, distant mic (voice peaks ~-49 dBFS over a -73 dBFS floor,
    /// as measured on the user's QuadCast) must come out at a usable level, not gated to silence.
    #[test]
    fn quiet_distant_voice_is_brought_up() {
        let mut c = Chain::new(SAMPLE_RATE);
        // Gain staging is what's under test; DeepFilterNet (rightly) treats a synthetic
        // tone stack as noise, so use the RNNoise path here.
        let mut p = *c.params();
        p.ns_high_quality = 0;
        c.set_params(&p);
        let mut seed = 3;
        let mut voiced_sq = 0.0f64;
        let mut voiced_n = 0usize;
        let total = 48_000 * 12;
        let mut buf = vec![0.0f32; 480];
        for b in 0..total / 480 {
            for (i, s) in buf.iter_mut().enumerate() {
                let n = b * 480 + i;
                let t = n as f32 / 48_000.0;
                // 1.5 s phrases separated by 0.5 s pauses; 4 Hz syllables.
                let talking = (t % 2.0) < 1.5;
                let env = if talking { 0.5 + 0.5 * (2.0 * std::f32::consts::PI * 4.0 * t).sin() } else { 0.0 };
                let voice: f32 = (1..12).map(|k| (2.0 * std::f32::consts::PI * 140.0 * k as f32 * t).sin() / k as f32).sum();
                *s = 0.0018 * env * voice + 0.0003 * noise(&mut seed);
            }
            c.process_mono(&mut buf);
            // Measure the last 4 s, while talking (output is delayed by the chain latency).
            let lat = c.latency();
            for (i, &y) in buf.iter().enumerate() {
                let n = b * 480 + i;
                if n >= total - 48_000 * 4 && n >= lat {
                    let t = (n - lat) as f32 / 48_000.0;
                    if (t % 2.0) > 0.2 && (t % 2.0) < 1.3 {
                        voiced_sq += (y as f64).powi(2);
                        voiced_n += 1;
                    }
                }
            }
        }
        let rms_db = 10.0 * (voiced_sq / voiced_n as f64).log10();
        assert!(rms_db > -32.0, "voiced output RMS {rms_db:.1} dBFS: quiet voice was not brought up");
    }

    /// Sounds of a 2 s beatbox loop as `(name, start, length)` in samples.
    const KIT: [(&str, usize, usize); 5] = [
        ("kick", 0, 7_200),
        ("hi-hat", 12_000, 2_400),
        ("snare", 24_000, 6_000),
        ("click", 36_000, 720),
        ("lip bass", 48_000, 24_000),
    ];

    /// One sample of the loop: synthetic drum sounds over a quiet room, with the soft
    /// sounds well below the kick.
    fn beatbox_sample(n: usize, seed: &mut u32, hp: &mut f32) -> f32 {
        let i = n % 96_000;
        let hiss = noise(seed);
        let bright = hiss - *hp;
        *hp = hiss;
        let time = |start: usize| (i - start) as f32 / 48_000.0;
        let tau = 2.0 * std::f32::consts::PI;
        let sound = match KIT.iter().find(|s| i >= s.1 && i < s.1 + s.2).map(|s| (s.0, s.1)) {
            Some(("kick", s)) => {
                let t = time(s);
                0.3 * (-t * 18.0).exp() * (tau * (55.0 * t + 4.0 * (1.0 - (-t * 25.0).exp()))).sin()
            }
            Some(("hi-hat", s)) => 0.05 * (-time(s) * 90.0).exp() * bright,
            Some(("snare", s)) => {
                let t = time(s);
                (-t * 28.0).exp() * (0.08 * hiss + 0.08 * (tau * 180.0 * t).sin())
            }
            Some(("click", s)) => 0.03 * (-time(s) * 400.0).exp() * bright,
            Some(("lip bass", s)) => {
                let t = time(s);
                (1..6).map(|k| 0.04 * (tau * 70.0 * k as f32 * t).sin() / k as f32).sum()
            }
            _ => 0.0,
        };
        sound + 0.0003 * hiss
    }

    /// Output vs input level of each kit sound in dB, once the AGC has settled.
    fn kit_gains(params: Params) -> Vec<(&'static str, f32)> {
        let mut c = Chain::new(SAMPLE_RATE);
        c.set_params(&params);
        let (mut seed, mut hp) = (5, 0.0);
        let total = 96_000 * 12;
        let input: Vec<f32> = (0..total).map(|n| beatbox_sample(n, &mut seed, &mut hp)).collect();
        let mut out = input.clone();
        for block in out.chunks_mut(480) {
            c.process_mono(block);
        }
        let lat = c.latency();
        let energy = |x: &[f32], offset: usize, start: usize, len: usize| -> f32 {
            (6..11).flat_map(|bar| (0..len).map(move |i| bar * 96_000 + start + i + offset)).map(|n| x[n] * x[n]).sum()
        };
        KIT.iter().map(|s| (s.0, 10.0 * (energy(&out, lat, s.1, s.2) / energy(&input, 0, s.1, s.2)).log10())).collect()
    }

    /// Every sound, down to the softest click, must come out at about the same gain:
    /// a sound the chain dislikes drops away from the others.
    #[test]
    fn beatbox_preset_keeps_every_sound() {
        for (label, params) in [("far", Params::far_mic()), ("beatbox", Params::beatbox())] {
            let gains = kit_gains(params);
            let top = gains.iter().map(|g| g.1).fold(f32::MIN, f32::max);
            for (name, gain) in &gains {
                println!("{label:>8} {name:>9}: {gain:+.1} dB");
                if label == "beatbox" {
                    assert!(top - gain < 6.0, "{name} lost {:.1} dB against the loudest sound", top - gain);
                }
            }
        }
    }

    /// Regression: a muted mic (near-digital silence with tiny blips) must not make
    /// the auto-gain climb.
    #[test]
    fn muted_mic_does_not_pump_agc() {
        let mut c = Chain::new(SAMPLE_RATE);
        let mut seed = 11;
        let mut buf = vec![0.0f32; 480];
        for b in 0..1_000 {
            for s in buf.iter_mut() {
                // ~-110 dBFS hiss with an occasional -95 dBFS blip.
                *s = 3e-6 * noise(&mut seed) + if b % 37 == 0 { 2e-5 } else { 0.0 };
            }
            c.process_mono(&mut buf);
        }
        assert!(c.meters().agc_gain_db < 1.0, "AGC climbed to {} dB on silence", c.meters().agc_gain_db);
    }
}
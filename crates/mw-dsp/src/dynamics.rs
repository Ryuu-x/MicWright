//! Expander, AGC/leveler, compressor and de-esser.

use crate::biquad::Biquad;
use crate::util::{db_to_lin, lin_to_db, Ballistics};

/// Downward expander: below `threshold_db`, every dB of level drop becomes `ratio` dB of output drop.
pub struct Expander {
    detector: Ballistics,
    gain: Ballistics,
    hold_samples: u32,
    hold_left: u32,
    pub gain_db: f32,
}

impl Expander {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            detector: Ballistics::new(1.0, 60.0, sample_rate, 0.0),
            gain: Ballistics::new(2.0, 150.0, sample_rate, 1.0),
            hold_samples: (0.08 * sample_rate) as u32,
            hold_left: 0,
            gain_db: 0.0,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32, threshold_db: f32, ratio: f32, range_db: f32) -> f32 {
        let env = self.detector.process(x.abs());
        let level_db = lin_to_db(env);
        let target_db = if level_db >= threshold_db {
            self.hold_left = self.hold_samples;
            0.0
        } else if self.hold_left > 0 {
            self.hold_left -= 1;
            0.0
        } else {
            ((level_db - threshold_db) * (ratio - 1.0)).max(-range_db)
        };
        // "Attack" here means opening (gain going up) — we invert: open fast, close slowly.
        let g = self.gain.process(db_to_lin(target_db));
        self.gain_db = lin_to_db(g);
        x * g
    }
}

/// Slow automatic leveler. Only adapts while the voice-activity probability is high,
/// so it never pumps up the room noise during silence.
pub struct Agc {
    power: f32,
    power_coeff: f32,
    gain_db: f32,
    up_rate_db: f32,
    down_rate_db: f32,
}

impl Agc {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            power: 0.0,
            // ~400 ms power window
            power_coeff: crate::util::time_coeff(400.0, sample_rate),
            gain_db: 0.0,
            // dB per sample: up 6 dB/s (15 dB/s while far below target), down 12 dB/s
            up_rate_db: 6.0 / sample_rate,
            down_rate_db: 12.0 / sample_rate,
        }
    }

    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    pub fn set_gain_db(&mut self, g: f32) {
        self.gain_db = g;
    }

    #[inline]
    pub fn process(&mut self, x: f32, voice: bool, target_db: f32, max_gain_db: f32, min_gain_db: f32) -> f32 {
        if voice {
            self.power = x * x + self.power_coeff * (self.power - x * x);
            // RMS of a sine is -3 dB from its peak; target is an RMS (dBFS) level.
            let level_db = 10.0 * self.power.max(1e-12).log10();
            let wanted = (target_db - level_db).clamp(min_gain_db, max_gain_db);
            if wanted > self.gain_db {
                // Catch up quickly at start-up / after a big level drop, then settle gently.
                let rate = if wanted - self.gain_db > 10.0 { self.up_rate_db * 2.5 } else { self.up_rate_db };
                self.gain_db = (self.gain_db + rate).min(wanted);
            } else {
                self.gain_db = (self.gain_db - self.down_rate_db).max(wanted);
            }
        }
        x * db_to_lin(self.gain_db)
    }
}

/// Feed-forward log-domain compressor with soft knee.
pub struct Compressor {
    detector: Ballistics,
    smoothing: Ballistics,
    pub gain_reduction_db: f32,
}

impl Compressor {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            detector: Ballistics::new(0.5, 30.0, sample_rate, 0.0),
            smoothing: Ballistics::new(5.0, 80.0, sample_rate, 0.0),
            gain_reduction_db: 0.0,
        }
    }

    pub fn set_times(&mut self, attack_ms: f32, release_ms: f32, sample_rate: f32) {
        self.smoothing.set_times(attack_ms, release_ms, sample_rate);
    }

    #[inline]
    pub fn static_curve(level_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
        let over = level_db - threshold_db;
        let slope = 1.0 - 1.0 / ratio;
        if knee_db > 0.0 && over.abs() <= knee_db / 2.0 {
            // reduction (positive dB) inside the knee
            slope * (over + knee_db / 2.0).powi(2) / (2.0 * knee_db)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
        let env = self.detector.process(x.abs());
        let reduction = Self::static_curve(lin_to_db(env), threshold_db, ratio, knee_db);
        // Reduction rising = attack.
        let r = self.smoothing.process(reduction);
        self.gain_reduction_db = r;
        x * db_to_lin(-r)
    }
}

/// Split-band de-esser: only the band above `freq` is attenuated, so the
/// rest of the voice is untouched. `x - hp(x)*(1-g)` reconstructs exactly when g = 1.
pub struct DeEsser {
    split: Biquad,
    detect: Biquad,
    env: Ballistics,
    gain: Ballistics,
    pub gain_reduction_db: f32,
}

impl DeEsser {
    pub fn new(sample_rate: f32, freq: f32) -> Self {
        let mut split = Biquad::identity();
        split.highpass(sample_rate, freq, std::f32::consts::FRAC_1_SQRT_2);
        let mut detect = Biquad::identity();
        detect.peaking(sample_rate, 7_000.0, 1.0, 12.0);
        Self {
            split,
            detect,
            env: Ballistics::new(0.5, 40.0, sample_rate, 0.0),
            gain: Ballistics::new(1.0, 60.0, sample_rate, 1.0),
            gain_reduction_db: 0.0,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32, threshold_db: f32, max_reduction_db: f32) -> f32 {
        let high = self.split.process(x);
        let side = self.detect.process(high);
        let level_db = lin_to_db(self.env.process(side.abs()));
        let target_db = -((level_db - threshold_db) * 0.75).clamp(0.0, max_reduction_db);
        let g = self.gain.process_inverted(db_to_lin(target_db));
        self.gain_reduction_db = -lin_to_db(g);
        x - high * (1.0 - g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressor_curve() {
        assert_eq!(Compressor::static_curve(-30.0, -20.0, 3.0, 0.0), 0.0);
        let r = Compressor::static_curve(-8.0, -20.0, 3.0, 0.0);
        assert!((r - 8.0).abs() < 1e-4, "{r}");
        // knee is continuous at its edges
        let a = Compressor::static_curve(-17.0, -20.0, 3.0, 6.0);
        let b = Compressor::static_curve(-17.0001, -20.0, 3.0, 6.0);
        assert!((a - b).abs() < 1e-3);
    }

    #[test]
    fn expander_attenuates_quiet_noise() {
        let sr = 48_000.0;
        let mut e = Expander::new(sr);
        let mut out = 0.0f32;
        for n in 0..48_000 {
            let x = 0.001 * ((n as f32) * 0.37).sin(); // ~ -60 dBFS
            out = e.process(x, -45.0, 3.0, 30.0).abs().max(out * 0.999);
        }
        assert!(e.gain_db < -20.0, "gain {}", e.gain_db);
        let _ = out;
    }

    #[test]
    fn agc_freezes_without_voice() {
        let mut a = Agc::new(48_000.0);
        for _ in 0..48_000 {
            a.process(0.001, false, -20.0, 24.0, -12.0);
        }
        assert_eq!(a.gain_db(), 0.0);
        for _ in 0..(48_000 * 10) {
            a.process(0.01, true, -20.0, 24.0, -12.0);
        }
        // 0.01 DC = -40 dBFS RMS, so it should reach +20 dB.
        assert!((a.gain_db() - 20.0).abs() < 0.5, "{}", a.gain_db());
    }
}

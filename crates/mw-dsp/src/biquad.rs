//! RBJ "Audio EQ Cookbook" biquads, transposed direct form II.

use crate::util::flush;
use std::f32::consts::PI;

#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self::identity()
    }
}

impl Biquad {
    pub const fn identity() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0 }
    }

    fn set(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    pub fn highpass(&mut self, sample_rate: f32, freq: f32, q: f32) {
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q);
        self.set((1.0 + cs) / 2.0, -(1.0 + cs), (1.0 + cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha);
    }

    pub fn lowpass(&mut self, sample_rate: f32, freq: f32, q: f32) {
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q);
        self.set((1.0 - cs) / 2.0, 1.0 - cs, (1.0 - cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha);
    }

    pub fn peaking(&mut self, sample_rate: f32, freq: f32, q: f32, gain_db: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q);
        self.set(1.0 + alpha * a, -2.0 * cs, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cs, 1.0 - alpha / a);
    }

    pub fn low_shelf(&mut self, sample_rate: f32, freq: f32, gain_db: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sn, cs) = w0.sin_cos();
        // Shelf slope S = 1.
        let alpha = sn / 2.0 * 2f32.sqrt();
        let sa = 2.0 * a.sqrt() * alpha;
        self.set(
            a * ((a + 1.0) - (a - 1.0) * cs + sa),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
            a * ((a + 1.0) - (a - 1.0) * cs - sa),
            (a + 1.0) + (a - 1.0) * cs + sa,
            -2.0 * ((a - 1.0) + (a + 1.0) * cs),
            (a + 1.0) + (a - 1.0) * cs - sa,
        );
    }

    pub fn high_shelf(&mut self, sample_rate: f32, freq: f32, gain_db: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sn, cs) = w0.sin_cos();
        // Shelf slope S = 1.
        let alpha = sn / 2.0 * 2f32.sqrt();
        let sa = 2.0 * a.sqrt() * alpha;
        self.set(
            a * ((a + 1.0) + (a - 1.0) * cs + sa),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
            a * ((a + 1.0) + (a - 1.0) * cs - sa),
            (a + 1.0) - (a - 1.0) * cs + sa,
            2.0 * ((a - 1.0) - (a + 1.0) * cs),
            (a + 1.0) - (a - 1.0) * cs - sa,
        );
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = flush(self.b1 * x - self.a1 * y + self.z2);
        self.z2 = flush(self.b2 * x - self.a2 * y);
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady_gain(f: &mut Biquad, freq: f32) -> f32 {
        let sr = 48_000.0;
        let mut peak: f32 = 0.0;
        for n in 0..48_000 {
            let y = f.process((2.0 * PI * freq * n as f32 / sr).sin());
            if n > 24_000 {
                peak = peak.max(y.abs());
            }
        }
        peak
    }

    #[test]
    fn highpass_cuts_low_passes_high() {
        let mut f = Biquad::identity();
        f.highpass(48_000.0, 80.0, std::f32::consts::FRAC_1_SQRT_2);
        assert!(steady_gain(&mut f, 20.0) < 0.1);
        f.reset();
        assert!((steady_gain(&mut f, 1_000.0) - 1.0).abs() < 0.02);
    }

    #[test]
    fn low_shelf_boosts_bass_only() {
        let mut f = Biquad::identity();
        f.low_shelf(48_000.0, 120.0, 6.0);
        let low = steady_gain(&mut f, 30.0);
        f.reset();
        let high = steady_gain(&mut f, 3_000.0);
        assert!((low - 10f32.powf(6.0 / 20.0)).abs() < 0.1, "low {low}");
        assert!((high - 1.0).abs() < 0.05, "high {high}");
    }

    #[test]
    fn peaking_applies_gain_at_center() {
        let mut f = Biquad::identity();
        f.peaking(48_000.0, 3_000.0, 1.0, 6.0);
        let g = steady_gain(&mut f, 3_000.0);
        assert!((g - 10f32.powf(6.0 / 20.0)).abs() < 0.03, "gain {g}");
    }
}

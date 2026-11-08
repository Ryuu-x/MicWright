//! Look-ahead brickwall limiter.
//!
//! Required gain per sample is `min(1, ceiling/|x|)`. A sliding minimum over the
//! look-ahead window followed by a box average of the same length produces a smooth
//! gain curve that is guaranteed to be at or below the required gain when the peak
//! leaves the delay line, so the ceiling is never exceeded.

use crate::util::time_coeff;

pub const MAX_LOOKAHEAD: usize = 256;

pub struct Limiter {
    len: usize,
    delay: [f32; MAX_LOOKAHEAD],
    req: [f32; MAX_LOOKAHEAD],
    mins: [f32; MAX_LOOKAHEAD],
    pos: usize,
    sum: f32,
    release: f32,
    env: f32,
    pub gain_reduction_db: f32,
}

impl Limiter {
    pub fn new(sample_rate: f32, lookahead_ms: f32, release_ms: f32) -> Self {
        let len = ((lookahead_ms * 0.001 * sample_rate) as usize).clamp(1, MAX_LOOKAHEAD);
        Self {
            len,
            delay: [0.0; MAX_LOOKAHEAD],
            req: [1.0; MAX_LOOKAHEAD],
            mins: [1.0; MAX_LOOKAHEAD],
            pos: 0,
            sum: len as f32,
            release: time_coeff(release_ms, sample_rate),
            env: 1.0,
            gain_reduction_db: 0.0,
        }
    }

    /// Latency in samples (the signal delay).
    pub fn latency(&self) -> usize {
        self.len - 1
    }

    #[inline]
    pub fn process(&mut self, x: f32, ceiling: f32) -> f32 {
        let n = self.len;
        let p = self.pos;

        let a = x.abs();
        self.req[p] = if a > ceiling { ceiling / a } else { 1.0 };

        // Sliding minimum of required gain over the last `n` samples.
        let mut m = 1.0f32;
        for i in 0..n {
            m = m.min(self.req[i]);
        }

        // Box average of the minima over the last `n` samples.
        self.sum += m - self.mins[p];
        self.mins[p] = m;
        let smooth = (self.sum / n as f32).min(1.0);

        // Instant attack (already smoothed by the box filter), exponential release.
        self.env = if smooth < self.env { smooth } else { smooth + self.release * (self.env - smooth) };

        // Delay line of n-1 samples: read the oldest sample.
        let read = (p + 1) % n;
        let delayed = if n == 1 { x } else { self.delay[read] };
        self.delay[p] = x;
        self.pos = (p + 1) % n;

        self.gain_reduction_db = -20.0 * self.env.max(1e-6).log10();
        // Floating-point safety net.
        (delayed * self.env).clamp(-ceiling, ceiling)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_exceeds_ceiling() {
        let mut l = Limiter::new(48_000.0, 1.5, 50.0);
        let ceiling = 0.5;
        let mut seed = 12345u32;
        for n in 0..96_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            let burst = if (n / 4800) % 3 == 0 { 3.0 } else { 0.2 };
            let y = l.process(noise * burst, ceiling);
            assert!(y.abs() <= ceiling + 1e-6, "sample {n}: {y}");
        }
    }

    #[test]
    fn transparent_below_ceiling() {
        let mut l = Limiter::new(48_000.0, 1.5, 50.0);
        let lat = l.latency();
        let input: Vec<f32> = (0..4800).map(|n| 0.3 * (n as f32 * 0.05).sin()).collect();
        let out: Vec<f32> = input.iter().map(|&x| l.process(x, 0.9)).collect();
        for n in lat..input.len() {
            assert!((out[n] - input[n - lat]).abs() < 1e-6);
        }
    }
}

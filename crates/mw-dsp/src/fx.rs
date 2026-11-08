//! Creative voice effects: pitch shift, warmth (saturation) and reverb.
//!
//! All buffers are allocated in `new`; `process` never allocates.

use crate::util::flush;
use std::f32::consts::PI;

/// Delay-line pitch shifter: two read taps sweep through a short window at a rate set
/// by the pitch ratio and are cross-faded with complementary Hann windows. Cheap,
/// low-latency (≈ half a window on average) and good enough for voice character.
pub struct PitchShifter {
    buf: Vec<f32>,
    mask: usize,
    write: usize,
    phase: f32,
    window: f32,
}

impl PitchShifter {
    pub fn new(sample_rate: f32) -> Self {
        // ~40 ms window: long enough to avoid a "buzzy" sound, short enough to stay tight.
        let window = (0.040 * sample_rate).round();
        let size = (window as usize * 2).next_power_of_two();
        Self { buf: vec![0.0; size], mask: size - 1, write: 0, phase: 0.0, window }
    }

    #[inline]
    fn read(&self, delay: f32) -> f32 {
        let pos = self.write as f32 - delay;
        let i = pos.floor();
        let frac = pos - i;
        let i = i as isize as usize;
        let a = self.buf[i & self.mask];
        let b = self.buf[(i + 1) & self.mask];
        a + (b - a) * frac
    }

    /// `semitones` in -12..=12. 0 passes the signal through untouched.
    #[inline]
    pub fn process(&mut self, x: f32, semitones: f32) -> f32 {
        self.buf[self.write] = x;
        let y = if semitones.abs() < 0.01 {
            x
        } else {
            let ratio = 2f32.powf(semitones / 12.0);
            // Delay changes by (1 - ratio) samples per sample.
            self.phase = (self.phase + (1.0 - ratio) / self.window).rem_euclid(1.0);
            let p2 = (self.phase + 0.5) % 1.0;
            let g1 = 0.5 - 0.5 * (2.0 * PI * self.phase).cos();
            let g2 = 0.5 - 0.5 * (2.0 * PI * p2).cos();
            self.read(self.phase * self.window) * g1 + self.read(p2 * self.window) * g2
        };
        self.write = (self.write + 1) & self.mask;
        y
    }
}

/// Soft saturation that adds warm low-order harmonics. Normalised so full-scale peaks
/// stay where they were; `amount` 0 = clean, 1 = clearly driven.
#[inline]
pub fn warmth(x: f32, amount: f32) -> f32 {
    if amount <= 0.0 {
        return x;
    }
    let drive = 1.0 + amount * 4.0;
    let wet = (x * drive).tanh() / drive.tanh();
    x + (wet - x) * amount.min(1.0)
}

struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

impl Comb {
    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.buf[self.pos];
        self.store = flush(out * (1.0 - damp) + self.store * damp);
        self.buf[self.pos] = x + self.store * feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.pos];
        self.buf[self.pos] = flush(x + b * 0.5);
        self.pos = (self.pos + 1) % self.buf.len();
        b - x
    }
}

/// Mono Freeverb (Schroeder/Moorer): 8 parallel damped combs into 4 series allpasses.
pub struct Reverb {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl Reverb {
    pub fn new(sample_rate: f32) -> Self {
        // Classic tunings are for 44.1 kHz; scale to the actual rate.
        let scale = sample_rate / 44_100.0;
        let len = |n: usize| ((n as f32 * scale) as usize).max(1);
        Self {
            combs: [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617]
                .iter()
                .map(|&n| Comb { buf: vec![0.0; len(n)], pos: 0, store: 0.0 })
                .collect(),
            allpasses: [556, 441, 341, 225].iter().map(|&n| Allpass { buf: vec![0.0; len(n)], pos: 0 }).collect(),
        }
    }

    /// `mix` 0..1 (0 = off), `size` 0..1 (small room → hall), `damping` 0..1 (bright → dark).
    #[inline]
    pub fn process(&mut self, x: f32, mix: f32, size: f32, damping: f32) -> f32 {
        if mix <= 0.0 {
            return x;
        }
        let feedback = 0.70 + size.clamp(0.0, 1.0) * 0.28;
        let damp = damping.clamp(0.0, 1.0) * 0.4;
        let input = x * 0.015;
        let mut wet = 0.0;
        for c in &mut self.combs {
            wet += c.process(input, feedback, damp);
        }
        for a in &mut self.allpasses {
            wet = a.process(wet);
        }
        let mix = mix.clamp(0.0, 1.0);
        x * (1.0 - 0.5 * mix) + wet * 3.0 * mix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zero_crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count()
    }

    #[test]
    fn pitch_up_one_octave_doubles_frequency() {
        let mut p = PitchShifter::new(48_000.0);
        let x: Vec<f32> = (0..48_000).map(|n| (2.0 * PI * 200.0 * n as f32 / 48_000.0).sin()).collect();
        let y: Vec<f32> = x.iter().map(|&s| p.process(s, 12.0)).collect();
        let zin = zero_crossings(&x[24_000..]);
        let zout = zero_crossings(&y[24_000..]);
        let ratio = zout as f32 / zin as f32;
        assert!((ratio - 2.0).abs() < 0.15, "frequency ratio {ratio}");
    }

    #[test]
    fn pitch_zero_is_transparent() {
        let mut p = PitchShifter::new(48_000.0);
        for n in 0..1000 {
            let x = (n as f32 * 0.1).sin();
            assert_eq!(p.process(x, 0.0), x);
        }
    }

    #[test]
    fn warmth_keeps_peaks_and_is_identity_at_zero() {
        assert_eq!(warmth(0.3, 0.0), 0.3);
        assert!((warmth(1.0, 1.0) - 1.0).abs() < 1e-5);
        assert!(warmth(0.3, 1.0) > 0.3, "quiet parts are lifted");
    }

    #[test]
    fn reverb_has_a_tail_and_stays_bounded() {
        let mut r = Reverb::new(48_000.0);
        let mut tail = 0.0f32;
        for n in 0..48_000 {
            let x = if n < 480 { 0.5 } else { 0.0 };
            let y = r.process(x, 0.5, 0.6, 0.5);
            assert!(y.is_finite() && y.abs() < 2.0);
            if n > 4_800 && n < 9_600 {
                tail = tail.max(y.abs());
            }
        }
        assert!(tail > 1e-3, "no reverb tail");
    }
}

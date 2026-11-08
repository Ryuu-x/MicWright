//! Small numeric helpers shared by every stage.

#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
pub fn lin_to_db(lin: f32) -> f32 {
    20.0 * lin.max(1e-10).log10()
}

/// One-pole smoothing coefficient for a time constant in milliseconds.
#[inline]
pub fn time_coeff(ms: f32, sample_rate: f32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * sample_rate)).exp()
    }
}

/// Envelope follower with separate attack/release (operates in any domain).
#[derive(Clone, Copy, Debug)]
pub struct Ballistics {
    attack: f32,
    release: f32,
    pub value: f32,
}

impl Ballistics {
    pub fn new(attack_ms: f32, release_ms: f32, sample_rate: f32, initial: f32) -> Self {
        Self {
            attack: time_coeff(attack_ms, sample_rate),
            release: time_coeff(release_ms, sample_rate),
            value: initial,
        }
    }

    pub fn set_times(&mut self, attack_ms: f32, release_ms: f32, sample_rate: f32) {
        self.attack = time_coeff(attack_ms, sample_rate);
        self.release = time_coeff(release_ms, sample_rate);
    }

    /// `rising` decides which coefficient applies when the input exceeds the state.
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        let c = if input > self.value { self.attack } else { self.release };
        self.value = input + c * (self.value - input);
        self.value
    }

    /// Like `process`, but "attack" applies when the input *falls* (used for gain curves,
    /// where gain reduction must engage fast and recover slowly).
    #[inline]
    pub fn process_inverted(&mut self, input: f32) -> f32 {
        let c = if input < self.value { self.attack } else { self.release };
        self.value = input + c * (self.value - input);
        self.value
    }
}

/// Flush denormals to zero to keep the real-time thread fast.
#[inline]
pub fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 {
        0.0
    } else {
        x
    }
}

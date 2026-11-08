//! Plain-old-data parameter and meter blocks.
//!
//! Both are `#[repr(C)]` with only 4-byte fields so they can be shared verbatim
//! across the C ABI (APO) and through shared memory (tray app <-> audiodg).

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// 0 = bypass the whole chain (audio passes through untouched, still metered).
    pub enabled: u32,
    /// 1 = output silence.
    pub mute: u32,

    pub hpf_hz: f32,

    pub ns_enabled: u32,
    /// 0..1 wet/dry mix of the noise suppressor.
    pub ns_strength: f32,

    pub exp_enabled: u32,
    pub exp_threshold_db: f32,
    pub exp_ratio: f32,
    pub exp_range_db: f32,

    pub agc_enabled: u32,
    /// Target RMS level of speech in dBFS.
    pub agc_target_db: f32,
    pub agc_max_gain_db: f32,
    pub agc_min_gain_db: f32,
    /// Fixed gain applied before the dynamics when the AGC is off.
    pub pre_gain_db: f32,

    pub comp_enabled: u32,
    pub comp_threshold_db: f32,
    pub comp_ratio: f32,
    pub comp_knee_db: f32,
    pub comp_attack_ms: f32,
    pub comp_release_ms: f32,
    pub comp_makeup_db: f32,

    pub eq_enabled: u32,
    pub eq_mud_hz: f32,
    pub eq_mud_db: f32,
    pub eq_presence_hz: f32,
    pub eq_presence_db: f32,
    pub eq_air_db: f32,

    pub deess_enabled: u32,
    pub deess_threshold_db: f32,
    pub deess_max_db: f32,

    pub limiter_ceiling_db: f32,
    pub output_gain_db: f32,

    /// Low shelf at 120 Hz.
    pub eq_bass_db: f32,

    /// 1 = the creative effects below are active.
    pub fx_enabled: u32,
    /// -12..=12 semitones, 0 = off.
    pub pitch_semitones: f32,
    /// 0..1 soft saturation.
    pub warmth: f32,
    /// 0..1 reverb amount, 0 = off.
    pub reverb_mix: f32,
    /// 0..1 small room → hall.
    pub reverb_size: f32,
    /// 0..1 bright → dark.
    pub reverb_damping: f32,

    /// 1 = DeepFilterNet3 noise removal (when compiled in), 0 = RNNoise.
    pub ns_high_quality: u32,
}

impl Default for Params {
    fn default() -> Self {
        Self::far_mic()
    }
}

impl Params {
    /// Default preset: the mic is some distance away, so lean on denoise + AGC.
    pub const fn far_mic() -> Self {
        Self {
            enabled: 1,
            mute: 0,
            hpf_hz: 80.0,
            ns_enabled: 1,
            ns_strength: 1.0,
            exp_enabled: 1,
            exp_threshold_db: -50.0,
            exp_ratio: 2.5,
            exp_range_db: 18.0,
            agc_enabled: 1,
            agc_target_db: -22.0,
            agc_max_gain_db: 30.0,
            agc_min_gain_db: -6.0,
            pre_gain_db: 0.0,
            comp_enabled: 1,
            comp_threshold_db: -18.0,
            comp_ratio: 3.0,
            comp_knee_db: 6.0,
            comp_attack_ms: 5.0,
            comp_release_ms: 80.0,
            comp_makeup_db: 3.0,
            eq_enabled: 1,
            eq_mud_hz: 250.0,
            eq_mud_db: -3.0,
            eq_presence_hz: 4_000.0,
            eq_presence_db: 3.0,
            eq_air_db: 1.5,
            deess_enabled: 1,
            deess_threshold_db: -26.0,
            deess_max_db: 8.0,
            limiter_ceiling_db: -1.5,
            output_gain_db: 0.0,
            eq_bass_db: 0.0,
            fx_enabled: 1,
            pitch_semitones: 0.0,
            warmth: 0.0,
            reverb_mix: 0.0,
            reverb_size: 0.4,
            reverb_damping: 0.5,
            ns_high_quality: 1,
        }
    }

    /// Quiet room, mic close: light touch.
    pub const fn quiet_room() -> Self {
        let mut p = Self::far_mic();
        p.ns_strength = 0.7;
        p.exp_range_db = 10.0;
        p.agc_max_gain_db = 12.0;
        p.comp_ratio = 2.5;
        p
    }

    /// Loud fans / keyboard: stronger gating.
    pub const fn noisy_room() -> Self {
        let mut p = Self::far_mic();
        p.exp_threshold_db = -42.0;
        p.exp_ratio = 4.0;
        p.exp_range_db = 30.0;
        p.agc_max_gain_db = 18.0;
        p
    }

    /// Beatboxing: nothing may decide a sound is unwanted. The denoiser and de-esser
    /// take kicks, hats and clicks for noise and sibilance, and the gate chops tails,
    /// so room noise is left in. The slow compressor attack keeps each hit's transient.
    pub const fn beatbox() -> Self {
        let mut p = Self::far_mic();
        p.hpf_hz = 40.0;
        p.ns_enabled = 0;
        // With noise removal off, DeepFilterNet would only burn CPU.
        p.ns_high_quality = 0;
        p.exp_enabled = 0;
        p.exp_ratio = 2.0;
        p.exp_range_db = 8.0;
        p.agc_max_gain_db = 24.0;
        p.agc_min_gain_db = -12.0;
        p.comp_threshold_db = -14.0;
        p.comp_ratio = 2.0;
        p.comp_attack_ms = 15.0;
        p.comp_release_ms = 120.0;
        p.comp_makeup_db = 1.5;
        p.eq_mud_db = 0.0;
        p.eq_presence_db = 0.0;
        p.eq_air_db = 0.0;
        p.deess_enabled = 0;
        p.limiter_ceiling_db = -1.0;
        p
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Meters {
    pub in_peak_db: f32,
    pub in_rms_db: f32,
    pub out_peak_db: f32,
    pub out_rms_db: f32,
    pub vad: f32,
    pub noise_floor_db: f32,
    pub agc_gain_db: f32,
    pub exp_gain_db: f32,
    pub comp_gr_db: f32,
    pub deess_gr_db: f32,
    pub limiter_gr_db: f32,
    /// Incremented on every processed block.
    pub blocks: u32,
}

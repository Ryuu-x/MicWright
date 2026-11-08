//! Shared memory between the APO (inside `audiodg.exe`, LocalService) and the
//! tray app / service (user / LocalSystem).
//!
//! * Parameters flow app → APO, meters flow APO → app.
//! * Each direction is guarded by a seqlock, so the real-time reader never blocks:
//!   if a read races a write it simply keeps the previous value for one block.
//! * Whoever comes first creates the mapping (the APO usually does, at audio start-up),
//!   with a DACL that lets SYSTEM, LocalService, Administrators and the interactive user in.

use mw_dsp::{Meters, Params};
use std::ptr;
use std::sync::atomic::{fence, AtomicU32, Ordering};

pub const MAGIC: u32 = 0x4D57_5231; // "MWR1"
pub const VERSION: u32 = 3;
pub const NAME: &str = "Global\\MicWright.Shared.v1";
/// Fallback when the caller lacks SeCreateGlobalPrivilege (plain user session).
pub const LOCAL_NAME: &str = "Local\\MicWright.Shared.v1";

/// APO status flags (`Shared::apo_flags`).
pub mod flags {
    /// A panic was caught; the instance is passing audio through untouched.
    pub const POISONED: u32 = 1 << 0;
    /// Repeated time-budget overruns; the instance switched to pass-through.
    pub const OVERRUN_BYPASS: u32 = 1 << 1;
    /// Stream format is not 48 kHz float; passing through.
    pub const UNSUPPORTED_FORMAT: u32 = 1 << 2;
}

#[repr(C)]
pub struct Shared {
    pub magic: u32,
    pub version: u32,
    pub size: u32,

    params_seq: AtomicU32,
    params: Params,
    /// Bumped by writers; lets the APO notice changes cheaply.
    pub params_generation: AtomicU32,

    meters_seq: AtomicU32,
    meters: Meters,

    /// Incremented by the APO on every processed block.
    pub heartbeat: AtomicU32,
    pub apo_instances: AtomicU32,
    pub apo_flags: AtomicU32,
    pub apo_latency_frames: AtomicU32,
    pub apo_sample_rate: AtomicU32,
    pub apo_channels: AtomicU32,
    pub apo_overruns: AtomicU32,
    /// Worst observed block processing time, in microseconds.
    pub apo_max_process_us: AtomicU32,
}

impl Shared {
    /// Initialise a freshly created (zeroed) block.
    pub fn init(&mut self, params: &Params) {
        self.magic = MAGIC;
        self.version = VERSION;
        self.size = std::mem::size_of::<Shared>() as u32;
        self.params = *params;
        self.params_generation.store(1, Ordering::Release);
    }

    pub fn is_valid(&self) -> bool {
        self.magic == MAGIC && self.version == VERSION && self.size == std::mem::size_of::<Shared>() as u32
    }

    pub fn write_params(&self, p: &Params) {
        seq_write(&self.params_seq, &self.params as *const Params as *mut Params, p);
        self.params_generation.fetch_add(1, Ordering::Release);
    }

    /// Wait-free. `None` if a writer was mid-update.
    pub fn try_read_params(&self) -> Option<Params> {
        seq_read(&self.params_seq, &self.params)
    }

    pub fn write_meters(&self, m: &Meters) {
        seq_write(&self.meters_seq, &self.meters as *const Meters as *mut Meters, m);
    }

    pub fn try_read_meters(&self) -> Option<Meters> {
        seq_read(&self.meters_seq, &self.meters)
    }

    pub fn read_params(&self) -> Params {
        loop {
            if let Some(p) = self.try_read_params() {
                return p;
            }
            std::hint::spin_loop();
        }
    }
}

fn seq_write<T: Copy>(seq: &AtomicU32, dst: *mut T, value: &T) {
    let s = seq.load(Ordering::Relaxed);
    seq.store(s.wrapping_add(1) | 1, Ordering::Relaxed);
    fence(Ordering::Release);
    // SAFETY: dst points into the mapped block; readers detect torn copies via `seq`.
    unsafe { ptr::write_volatile(dst, *value) };
    fence(Ordering::Release);
    seq.store((s | 1).wrapping_add(1), Ordering::Release);
}

fn seq_read<T: Copy>(seq: &AtomicU32, src: &T) -> Option<T> {
    let s1 = seq.load(Ordering::Acquire);
    if s1 & 1 == 1 {
        return None;
    }
    // SAFETY: `src` is a valid, aligned POD in the mapping; a torn read is discarded below.
    let v = unsafe { ptr::read_volatile(src) };
    fence(Ordering::Acquire);
    let s2 = seq.load(Ordering::Relaxed);
    (s1 == s2).then_some(v)
}

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::{Mapping, OpenMode};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seqlock_roundtrip() {
        let mut block: Box<Shared> = unsafe { Box::new(std::mem::zeroed()) };
        block.init(&Params::far_mic());
        assert!(block.is_valid());
        let mut p = Params::noisy_room();
        p.output_gain_db = 2.0;
        block.write_params(&p);
        assert_eq!(block.try_read_params(), Some(p));
        assert_eq!(block.params_generation.load(Ordering::Relaxed), 2);

        let m = Meters { vad: 0.5, blocks: 9, ..Default::default() };
        block.write_meters(&m);
        assert_eq!(block.try_read_meters(), Some(m));
    }

    #[test]
    fn layout_is_stable() {
        // Pure 4-byte fields: size must stay a multiple of 4 and never contain padding surprises.
        assert_eq!(std::mem::size_of::<Params>() % 4, 0);
        assert_eq!(std::mem::align_of::<Shared>(), 4);
    }
}

//! C ABI used by the APO shim (`apo/`). See `apo/mw_dsp.h` for the C declarations.
//!
//! Safety rules for code running inside `audiodg.exe`:
//! * every entry point catches panics and turns them into pass-through,
//! * `mw_process` never allocates, locks or blocks,
//! * an instance that overruns its time budget repeatedly bypasses itself.

use mw_dsp::{Chain, Params, SAMPLE_RATE};
use mw_ipc::flags;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::time::Instant;

#[cfg(windows)]
use mw_ipc::{Mapping, OpenMode};

/// Consecutive over-budget blocks before an instance bypasses itself.
const OVERRUN_LIMIT: u32 = 50;

pub struct MwHandle {
    chain: Chain,
    channels: usize,
    supported: bool,
    poisoned: bool,
    overrun_bypass: bool,
    overrun_streak: u32,
    seen_generation: u32,
    #[cfg(windows)]
    ipc: Option<Mapping>,
}

impl MwHandle {
    fn new(sample_rate: u32, channels: u32) -> Self {
        let supported = sample_rate == SAMPLE_RATE && (1..=8).contains(&channels);
        let mut h = Self {
            chain: Chain::new(SAMPLE_RATE),
            channels: channels.max(1) as usize,
            supported,
            poisoned: false,
            overrun_bypass: false,
            overrun_streak: 0,
            seen_generation: 0,
            #[cfg(windows)]
            ipc: Mapping::open(OpenMode::OpenOrCreate, &Params::default()).ok(),
        };
        #[cfg(windows)]
        if let Some(m) = &h.ipc {
            let s = m.shared();
            s.apo_instances.fetch_add(1, Ordering::Relaxed);
            s.apo_sample_rate.store(sample_rate, Ordering::Relaxed);
            s.apo_channels.store(channels, Ordering::Relaxed);
            s.apo_latency_frames.store(h.chain.latency() as u32, Ordering::Relaxed);
            if !supported {
                s.apo_flags.fetch_or(flags::UNSUPPORTED_FORMAT, Ordering::Relaxed);
            }
        }
        h.sync_params();
        h
    }

    #[inline]
    fn sync_params(&mut self) {
        #[cfg(windows)]
        if let Some(m) = &self.ipc {
            let s = m.shared();
            let generation = s.params_generation.load(Ordering::Acquire);
            if generation != self.seen_generation {
                if let Some(p) = s.try_read_params() {
                    self.chain.set_params(&p);
                    self.seen_generation = generation;
                }
            }
        }
    }

    #[inline]
    fn publish(&self, elapsed_us: u32) {
        #[cfg(windows)]
        if let Some(m) = &self.ipc {
            let s = m.shared();
            s.write_meters(self.chain.meters());
            s.heartbeat.fetch_add(1, Ordering::Relaxed);
            s.apo_max_process_us.fetch_max(elapsed_us, Ordering::Relaxed);
        }
        #[cfg(not(windows))]
        let _ = elapsed_us;
    }

    fn set_flag(&self, flag: u32) {
        #[cfg(windows)]
        if let Some(m) = &self.ipc {
            m.shared().apo_flags.fetch_or(flag, Ordering::Relaxed);
        }
        #[cfg(not(windows))]
        let _ = flag;
    }
}

impl Drop for MwHandle {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(m) = &self.ipc {
            m.shared().apo_instances.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Creates a processing instance. Returns null on failure (caller must pass audio through).
#[no_mangle]
pub extern "C" fn mw_create(sample_rate: u32, channels: u32) -> *mut MwHandle {
    catch_unwind(|| Box::into_raw(Box::new(MwHandle::new(sample_rate, channels)))).unwrap_or(std::ptr::null_mut())
}

/// Algorithmic latency in frames.
#[no_mangle]
pub extern "C" fn mw_latency_frames(h: *const MwHandle) -> u32 {
    if h.is_null() {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| unsafe { (*h).chain.latency() as u32 })).unwrap_or(0)
}

/// Processes `frames` interleaved float frames from `input` to `output` (may alias).
///
/// Returns 0 when `output` holds processed audio, non-zero when the caller must copy
/// `input` to `output` itself (pass-through).
#[no_mangle]
pub extern "C" fn mw_process(h: *mut MwHandle, input: *const f32, output: *mut f32, frames: u32) -> i32 {
    if h.is_null() || input.is_null() || output.is_null() {
        return 1;
    }
    let h = unsafe { &mut *h };
    if h.poisoned || h.overrun_bypass || !h.supported {
        return 2;
    }
    let n = frames as usize * h.channels;
    let start = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| {
        h.sync_params();
        if std::ptr::eq(input, output) {
            let buf = unsafe { std::slice::from_raw_parts_mut(output, n) };
            h.chain.process_interleaved(buf, h.channels);
        } else {
            let inp = unsafe { std::slice::from_raw_parts(input, n) };
            let out = unsafe { std::slice::from_raw_parts_mut(output, n) };
            h.chain.process_interleaved_into(inp, out, h.channels);
        }
    }));
    if result.is_err() {
        h.poisoned = true;
        h.set_flag(flags::POISONED);
        return 3;
    }

    let elapsed_us = start.elapsed().as_micros().min(u32::MAX as u128) as u32;
    // Budget: half the block's real-time duration.
    let budget_us = frames as u64 * 1_000_000 / SAMPLE_RATE as u64 / 2;
    if elapsed_us as u64 > budget_us {
        h.overrun_streak += 1;
        #[cfg(windows)]
        if let Some(m) = &h.ipc {
            m.shared().apo_overruns.fetch_add(1, Ordering::Relaxed);
        }
        if h.overrun_streak >= OVERRUN_LIMIT {
            h.overrun_bypass = true;
            h.set_flag(flags::OVERRUN_BYPASS);
        }
    } else {
        h.overrun_streak = 0;
    }
    h.publish(elapsed_us);
    0
}

/// Clears runtime state (e.g. after a stream restart) without touching params.
#[no_mangle]
pub extern "C" fn mw_reset(h: *mut MwHandle) {
    if h.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        let h = &mut *h;
        let p = *h.chain.params();
        h.chain = Chain::new(SAMPLE_RATE);
        h.chain.set_params(&p);
        h.overrun_streak = 0;
    }));
}

#[no_mangle]
pub extern "C" fn mw_destroy(h: *mut MwHandle) {
    if !h.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe { drop(Box::from_raw(h)) }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_process_destroy() {
        let h = mw_create(48_000, 2);
        assert!(!h.is_null());
        let input = vec![0.1f32; 960];
        let mut out = vec![0.0f32; 960];
        assert_eq!(mw_process(h, input.as_ptr(), out.as_mut_ptr(), 480), 0);
        assert!(mw_latency_frames(h) > 0);
        mw_destroy(h);
    }

    #[test]
    fn unsupported_rate_passes_through() {
        let h = mw_create(44_100, 2);
        let input = vec![0.1f32; 960];
        let mut out = vec![0.0f32; 960];
        assert_ne!(mw_process(h, input.as_ptr(), out.as_mut_ptr(), 480), 0);
        mw_destroy(h);
    }
}

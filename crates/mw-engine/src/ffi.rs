//! C ABI over [`Controller`] for the WinUI 3 front-end (`ui-winui/`, C# P/Invoke).
//!
//! Conventions:
//! * strings in are UTF-8, NUL-terminated; strings out are heap-allocated UTF-8 that the
//!   caller releases with `mw_free_string`,
//! * every entry point catches panics; a panic turns into the "failed" return value,
//! * the controller is single-threaded: call it from the UI thread only.

use crate::audio;
use crate::control::{recordings_dir, Controller, Preset};
use crate::settings;
use crate::soundboard;
use mw_dsp::Meters;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

fn out(s: impl Into<String>) -> *mut c_char {
    CString::new(s.into().replace('\0', "")).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

unsafe fn arg<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        None
    } else {
        CStr::from_ptr(p).to_str().ok()
    }
}

fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(fallback)
}

unsafe fn ctl<'a>(c: *mut Controller) -> Option<&'a mut Controller> {
    c.as_mut()
}

#[no_mangle]
pub unsafe extern "C" fn mw_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

// ---------------------------------------------------------------- static data

/// `[{name, blurb, groups:[{title, hint, toggle, knobs:[{key,label,hint,min,max,step,format,onlyWhenAgcOff}]}]}]`
#[no_mangle]
pub extern "C" fn mw_settings_json() -> *mut c_char {
    guard(std::ptr::null_mut(), || out(settings::settings_json()))
}

/// `[{key, name, tip}]`
#[no_mangle]
pub extern "C" fn mw_presets_json() -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        let v: Vec<_> = Preset::ALL.iter().map(|(_, key, name, tip)| serde_json::json!({ "key": key, "name": name, "tip": tip })).collect();
        out(serde_json::Value::Array(v).to_string())
    })
}

/// `{inputs:[..], outputs:[..], defaultInput, defaultSend, defaultMonitor}` (defaults may be null).
#[no_mangle]
pub extern "C" fn mw_devices_json() -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        let (input, send, monitor) = audio::default_devices();
        out(serde_json::json!({
            "inputs": audio::input_names(),
            "outputs": audio::output_names(),
            "defaultInput": input,
            "defaultSend": send,
            "defaultMonitor": monitor,
        })
        .to_string())
    })
}

#[no_mangle]
pub extern "C" fn mw_recordings_dir() -> *mut c_char {
    guard(std::ptr::null_mut(), || out(recordings_dir().display().to_string()))
}

// ---------------------------------------------------------------- controller lifetime

#[no_mangle]
pub extern "C" fn mw_new() -> *mut Controller {
    guard(std::ptr::null_mut(), || Box::into_raw(Box::new(Controller::new())))
}

#[no_mangle]
pub unsafe extern "C" fn mw_free(c: *mut Controller) {
    if !c.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(c))));
    }
}

// ---------------------------------------------------------------- engine

/// Returns 1 on success, 0 on failure (see `mw_last_error`). `monitor` may be null.
#[no_mangle]
pub unsafe extern "C" fn mw_start(c: *mut Controller, input: *const c_char, send: *const c_char, monitor: *const c_char) -> i32 {
    guard(0, || {
        let (Some(c), Some(input), Some(send)) = (ctl(c), arg(input), arg(send)) else { return 0 };
        c.start(input, send, arg(monitor)) as i32
    })
}

#[no_mangle]
pub unsafe extern "C" fn mw_stop(c: *mut Controller) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.stop();
        }
    })
}

/// Last error message, or null if none.
#[no_mangle]
pub unsafe extern "C" fn mw_last_error(c: *mut Controller) -> *mut c_char {
    guard(std::ptr::null_mut(), || match ctl(c).and_then(|c| c.error().map(str::to_owned)) {
        Some(e) => out(e),
        None => std::ptr::null_mut(),
    })
}

/// `{running, latencyMs, input, output, dropouts}`
#[no_mangle]
pub unsafe extern "C" fn mw_status_json(c: *mut Controller) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        let Some(c) = ctl(c) else { return std::ptr::null_mut() };
        let s = c.status();
        out(serde_json::json!({
            "running": s.running,
            "latencyMs": s.latency_ms,
            "input": s.input,
            "output": s.output,
            "dropouts": s.dropouts,
        })
        .to_string())
    })
}

#[no_mangle]
pub unsafe extern "C" fn mw_set_monitor(c: *mut Controller, on: i32) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.set_monitor(on != 0);
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn mw_get_monitor(c: *mut Controller) -> i32 {
    guard(0, || ctl(c).map(|c| c.monitor() as i32).unwrap_or(0))
}

// ---------------------------------------------------------------- parameters

/// NaN for unknown keys. Switches read as 0.0 / 1.0.
#[no_mangle]
pub unsafe extern "C" fn mw_param_get(c: *mut Controller, key: *const c_char) -> f32 {
    guard(f32::NAN, || {
        let (Some(c), Some(key)) = (ctl(c), arg(key)) else { return f32::NAN };
        c.param(key).unwrap_or(f32::NAN)
    })
}

/// Returns 1 if the key exists. Values are clamped to the control's range.
#[no_mangle]
pub unsafe extern "C" fn mw_param_set(c: *mut Controller, key: *const c_char, value: f32) -> i32 {
    guard(0, || {
        let (Some(c), Some(key)) = (ctl(c), arg(key)) else { return 0 };
        c.set_param(key, value) as i32
    })
}

#[no_mangle]
pub unsafe extern "C" fn mw_toggle_ab(c: *mut Controller) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.toggle_ab();
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn mw_toggle_mute(c: *mut Controller) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.toggle_mute();
        }
    })
}

/// Applies "far" / "quiet" / "noisy" / "custom:<name>". Returns 1 on success.
#[no_mangle]
pub unsafe extern "C" fn mw_preset_apply(c: *mut Controller, key: *const c_char) -> i32 {
    guard(0, || {
        let (Some(c), Some(key)) = (ctl(c), arg(key)) else { return 0 };
        c.apply_key(key) as i32
    })
}

/// Current preset key ("far", "custom:<name>", …), or "" when values were edited since.
#[no_mangle]
pub unsafe extern "C" fn mw_preset_current(c: *mut Controller) -> *mut c_char {
    guard(std::ptr::null_mut(), || out(ctl(c).map(|c| c.current_key()).unwrap_or_default()))
}

/// Key of the preset picked last, still known after edits; "" when there is none.
#[no_mangle]
pub unsafe extern "C" fn mw_preset_picked(c: *mut Controller) -> *mut c_char {
    guard(std::ptr::null_mut(), || ctl(c).map(|c| out(c.picked_key())).unwrap_or(std::ptr::null_mut()))
}

#[no_mangle]
pub unsafe extern "C" fn mw_preset_revert(c: *mut Controller) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.revert();
        }
    })
}

/// `["name", ...]` — saved custom presets.
#[no_mangle]
pub unsafe extern "C" fn mw_custom_presets_json(c: *mut Controller) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        let names = ctl(c).map(|c| c.custom_presets()).unwrap_or_default();
        out(serde_json::Value::from(names).to_string())
    })
}

/// Saves the current sound as a custom preset. Returns null on success, else an error message.
#[no_mangle]
pub unsafe extern "C" fn mw_preset_save(c: *mut Controller, name: *const c_char) -> *mut c_char {
    guard(out("internal error"), || {
        let (Some(c), Some(name)) = (ctl(c), arg(name)) else { return out("invalid name") };
        match c.save_custom(name) {
            Ok(_) => std::ptr::null_mut(),
            Err(e) => out(e),
        }
    })
}

/// Returns 1 if the custom preset was deleted.
#[no_mangle]
pub unsafe extern "C" fn mw_preset_delete(c: *mut Controller, name: *const c_char) -> i32 {
    guard(0, || {
        let (Some(c), Some(name)) = (ctl(c), arg(name)) else { return 0 };
        c.delete_custom(name).is_ok() as i32
    })
}

/// Restores the settings saved by `mw_save_state` (call once at start-up).
#[no_mangle]
pub unsafe extern "C" fn mw_load_state(c: *mut Controller) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.load_state();
        }
    })
}

/// Remembers the current settings for next launch (call on exit). Returns 1 on success.
#[no_mangle]
pub unsafe extern "C" fn mw_save_state(c: *mut Controller) -> i32 {
    guard(0, || ctl(c).map(|c| c.save_state().is_ok() as i32).unwrap_or(0))
}

#[no_mangle]
pub unsafe extern "C" fn mw_reset_page(c: *mut Controller, page: u32) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.reset_page(page as usize);
        }
    })
}

// ---------------------------------------------------------------- soundboard

/// Decodes and saves one sound file; safe on any thread, as it needs no controller.
/// `{id, name, seconds}`, or `{error}`, which finishes the sentence "<file> ...".
#[no_mangle]
pub unsafe extern "C" fn mw_board_prepare(path: *const c_char) -> *mut c_char {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(path) = arg(path) else { return serde_json::json!({ "error": "has a name that can't be read" }) };
        match soundboard::prepare(Path::new(path)) {
            Ok(p) => serde_json::json!({ "id": p.id, "name": p.name, "seconds": p.seconds }),
            Err(e) => serde_json::json!({ "error": format!("{e}") }),
        }
    }));
    // A decoder panic on a malformed file must stay one failed import.
    out(result.unwrap_or_else(|_| serde_json::json!({ "error": "is damaged" })).to_string())
}

/// Loads the saved sounds (call once at start-up).
#[no_mangle]
pub unsafe extern "C" fn mw_board_load(c: *mut Controller) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.board_load();
        }
    })
}

/// `[{id, name, seconds}]`
#[no_mangle]
pub unsafe extern "C" fn mw_board_clips_json(c: *mut Controller) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        let Some(c) = ctl(c) else { return std::ptr::null_mut() };
        let v: Vec<_> = c.board_clips().iter().map(|k| serde_json::json!({ "id": k.id, "name": k.name, "seconds": k.seconds() })).collect();
        out(serde_json::Value::Array(v).to_string())
    })
}

/// Adds a prepared sound. Returns null on success, else an error message.
#[no_mangle]
pub unsafe extern "C" fn mw_board_add(c: *mut Controller, id: *const c_char, name: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let (Some(c), Some(id), Some(name)) = (ctl(c), arg(id), arg(name)) else { return out("invalid sound") };
        c.board_add(id, name).err().map(out).unwrap_or(std::ptr::null_mut())
    }))
    .unwrap_or_else(|_| out("internal error"))
}

/// Returns 1 if the sound was removed.
#[no_mangle]
pub unsafe extern "C" fn mw_board_remove(c: *mut Controller, id: *const c_char) -> i32 {
    guard(0, || {
        let (Some(c), Some(id)) = (ctl(c), arg(id)) else { return 0 };
        c.board_remove(id).is_ok() as i32
    })
}

/// `preview` != 0 plays for the user alone. Returns null on success, else a message.
#[no_mangle]
pub unsafe extern "C" fn mw_board_play(c: *mut Controller, id: *const c_char, preview: i32) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let (Some(c), Some(id)) = (ctl(c), arg(id)) else { return out("invalid sound") };
        c.board_play(id, preview != 0).err().map(out).unwrap_or(std::ptr::null_mut())
    }))
    .unwrap_or_else(|_| out("internal error"))
}

/// Stops the sound if it's playing, otherwise starts it. Returns null on success, else a message.
#[no_mangle]
pub unsafe extern "C" fn mw_board_toggle(c: *mut Controller, id: *const c_char, preview: i32) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let (Some(c), Some(id)) = (ctl(c), arg(id)) else { return out("invalid sound") };
        c.board_toggle(id, preview != 0).err().map(out).unwrap_or(std::ptr::null_mut())
    }))
    .unwrap_or_else(|_| out("internal error"))
}

/// `[{id, preview, secondsLeft}]`
#[no_mangle]
pub unsafe extern "C" fn mw_board_playing_json(c: *mut Controller) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        let Some(c) = ctl(c) else { return std::ptr::null_mut() };
        let v: Vec<_> = c.board_playing().iter().map(|(id, preview, left)| serde_json::json!({ "id": id, "preview": preview, "secondsLeft": left })).collect();
        out(serde_json::Value::Array(v).to_string())
    })
}

#[no_mangle]
pub unsafe extern "C" fn mw_board_stop(c: *mut Controller) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.board_stop();
        }
    })
}

/// Slider positions, 0..=1.
#[no_mangle]
pub unsafe extern "C" fn mw_board_levels(c: *mut Controller, send: f32, monitor: f32) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.board_levels(send, monitor);
        }
    })
}

// ---------------------------------------------------------------- metering

/// Refreshes meters / peak hold / recorder; fills `meters`. Returns 1 on success.
#[no_mangle]
pub unsafe extern "C" fn mw_poll(c: *mut Controller, meters: *mut Meters, peak_hold_db: *mut f32) -> i32 {
    guard(0, || {
        let Some(c) = ctl(c) else { return 0 };
        let m = c.poll();
        if let Some(dst) = meters.as_mut() {
            *dst = m;
        }
        if let Some(dst) = peak_hold_db.as_mut() {
            *dst = c.peak_hold();
        }
        1
    })
}

/// `[{key, severity: "info"|"warning"|"error", title, message}]`
#[no_mangle]
pub unsafe extern "C" fn mw_tips_json(c: *mut Controller) -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        let Some(c) = ctl(c) else { return std::ptr::null_mut() };
        let v: Vec<_> = c
            .tips()
            .iter()
            .map(|t| serde_json::json!({ "key": t.key, "severity": t.severity.as_str(), "title": t.title, "message": t.message }))
            .collect();
        out(serde_json::Value::Array(v).to_string())
    })
}

// ---------------------------------------------------------------- A/B recorder

#[no_mangle]
pub unsafe extern "C" fn mw_record_start(c: *mut Controller, seconds: u32) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.start_recording(seconds.clamp(1, 120));
        }
    })
}

/// Seconds left, or -1 when not recording.
#[no_mangle]
pub unsafe extern "C" fn mw_record_left(c: *mut Controller) -> f32 {
    guard(-1.0, || ctl(c).and_then(|c| c.recording_left()).unwrap_or(-1.0))
}

#[no_mangle]
pub unsafe extern "C" fn mw_record_stop(c: *mut Controller) {
    guard((), || {
        if let Some(c) = ctl(c) {
            c.finish_recording();
        }
    })
}

/// Folder of the last saved A/B recording, or null.
#[no_mangle]
pub unsafe extern "C" fn mw_last_recording_dir(c: *mut Controller) -> *mut c_char {
    guard(std::ptr::null_mut(), || match ctl(c).and_then(|c| c.last_saved().map(|p| p.display().to_string())) {
        Some(p) => out(p),
        None => std::ptr::null_mut(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_through_the_c_abi() {
        unsafe {
            let c = mw_new();
            assert!(!c.is_null());
            let key = CString::new("eq_bass_db").unwrap();
            assert_eq!(mw_param_set(c, key.as_ptr(), 4.5), 1);
            assert_eq!(mw_param_get(c, key.as_ptr()), 4.5);
            let bogus = CString::new("nope").unwrap();
            assert!(mw_param_get(c, bogus.as_ptr()).is_nan());
            let far = CString::new("noisy").unwrap();
            assert_eq!(mw_preset_apply(c, far.as_ptr()), 1);
            let cur = mw_preset_current(c);
            assert_eq!(CStr::from_ptr(cur).to_str().unwrap(), "noisy");
            mw_free_string(cur);
            let mut m = Meters::default();
            let mut hold = 0.0f32;
            assert_eq!(mw_poll(c, &mut m, &mut hold), 1);
            for s in [mw_settings_json(), mw_presets_json(), mw_tips_json(c), mw_status_json(c)] {
                assert!(serde_json::from_str::<serde_json::Value>(CStr::from_ptr(s).to_str().unwrap()).is_ok());
                mw_free_string(s);
            }
            mw_free(c);
        }
    }
}

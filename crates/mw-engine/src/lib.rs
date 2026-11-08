//! Everything a MicWright front-end needs, independent of the UI toolkit:
//!
//! * [`audio`] — capture → mw-dsp chain → send/monitor outputs,
//! * [`settings`] — the user-facing control table (labels, hints, ranges),
//! * [`soundboard`] — clips: importing them, keeping them, mixing them into the voice,
//! * [`control`] — presets, tips, recording and metering state,
//! * `ffi` — a C ABI over [`control::Controller`] used by the WinUI 3 app (`ui-winui/`).

pub mod audio;
pub mod control;
mod ffi;
pub mod settings;
pub mod soundboard;

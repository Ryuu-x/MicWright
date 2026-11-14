//! MicWright, built with GPUI (Zed's GPU-accelerated UI) via gpui-kit.
//!
//! On Linux this is the full app: mic → mw-dsp → "MicWright Virtual Mic" for other apps,
//! plus headphones, with presets, soundboard, tray, hotkeys and setup. On Windows the
//! shipped app is `ui-winui/`, and this one is a test front-end.
//!
//! Command line (a running copy acts on these and the new one exits):
//!   --background     start in the tray without a window
//!   --setup          show the first-run setup again
//!   --settings <n>   open on sound settings page n
//!   --play <name>    play a soundboard sound
//!   --stop-sounds    stop every sound
//!   --toggle-mute    mute or unmute
//!   --toggle-ab      flip between raw and enhanced
//!   --add-sound <f>  add a sound file to the soundboard
//!   --quit           quit the running copy
//!   --selftest       run the audio chain for 3 s without a window and report

#![windows_subsystem = "windows"]
// Tray, hotkeys and the instance socket are Linux-only; elsewhere their plumbing sits idle.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

mod core;
mod hotkey;
mod platform;
mod player;
mod prefs;
mod style;
mod ui;

use crate::core::{AppAction, Core};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use mw_dsp::Params;
use mw_engine::audio::{self, Engine, Shared};
use platform::Event;
use std::sync::atomic::Ordering;
use std::time::Duration;

fn main() {
    #[cfg(target_os = "linux")]
    mw_engine::linux::fix_server_path();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--selftest") {
        std::process::exit(selftest());
    }
    let command = Event::from_args(&args);
    // A copy is already running: hand it the command, or ask it to show its window.
    if platform::forward_to_running(command.as_ref().unwrap_or(&Event::Show)) {
        return;
    }
    if command.is_some() {
        eprintln!("MicWright isn't running.");
        std::process::exit(1);
    }
    let (events_tx, events) = std::sync::mpsc::channel();
    if !platform::claim_instance(events_tx.clone()) {
        platform::forward_to_running(&Event::Show);
        return;
    }
    platform::quit_on_signals(events_tx.clone());
    let background = args.iter().any(|a| a == "--background");

    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx: &mut App| {
        gpui_kit::init(cx);
        // GPUI quits with the last window on Linux by default; we decide (tray or quit).
        cx.set_quit_mode(QuitMode::Explicit);
        Theme::change(ThemeMode::Dark, None, cx);
        style::apply(cx);
        ui::init(cx);
        let core = cx.new(|_| Core::new(events, events_tx));

        // ~30×/s: meters, events from the tray / hotkeys / other launches, saving state.
        let ticking = core.clone();
        cx.spawn(async move |cx| loop {
            cx.background_executor().timer(Duration::from_millis(33)).await;
            let actions = ticking.update(cx, |c, cx| {
                let actions = c.tick();
                if c.window_open {
                    cx.notify();
                }
                actions
            });
            for action in actions {
                let core = ticking.clone();
                let _ = cx.update(|cx| match action {
                    AppAction::ShowWindow => ui::show_window(&core, cx),
                    AppAction::Quit => quit(&core, cx),
                });
            }
        })
        .detach();

        // Apps listening to the default mic depend on us: closing then keeps us in the tray.
        let closing = core.clone();
        cx.on_window_closed(move |cx, _| {
            ui::window_closed(&closing, cx);
            let c = closing.read(cx);
            if !(c.prefs.default_mic && c.has_tray) {
                quit(&closing, cx);
            }
        })
        .detach();

        if !background {
            ui::show_window(&core, cx);
        }
    });
}

fn quit(core: &Entity<Core>, cx: &mut App) {
    core.update(cx, |c, _| c.shutdown());
    cx.quit();
}

// ---------------------------------------------------------------- headless self-test

/// Opens the default devices, runs the chain for 3 s with monitoring off, and prints
/// what the meters saw. Exit code 0 = audio flowed through the chain.
fn selftest() -> i32 {
    #[cfg(target_os = "linux")]
    if let Err(e) = platform::ensure_virtual_mic() {
        println!("virtual mic: {e}");
    }
    let (din, dsend, _) = audio::default_devices();
    println!("inputs:  {:?}\noutputs: {:?}", audio::input_names(), audio::output_names());
    let (Some(input), Some(output)) = (din, dsend) else {
        println!("FAIL: no input/output device");
        return 1;
    };
    let shared = Shared::new(Params::far_mic());
    shared.monitor.store(false, Ordering::Relaxed);
    let engine = match Engine::start(&input, &output, None, shared.clone()) {
        Ok(e) => e,
        Err(e) => {
            println!("FAIL: {e:#}");
            return 1;
        }
    };
    println!("input:  {}\noutput: {}\nlatency: {:.1} ms", engine.input_desc, engine.output_desc, engine.chain_latency_ms);
    std::thread::sleep(Duration::from_secs(3));
    let m = *shared.meters.lock().unwrap();
    println!(
        "blocks {} · in peak {:.1} dBFS · out peak {:.1} dBFS · noise floor {:.1} dBFS · vad {:.2} · dropouts {}",
        m.blocks,
        m.in_peak_db,
        m.out_peak_db,
        m.noise_floor_db,
        m.vad,
        shared.underruns.load(Ordering::Relaxed) + shared.overflows.load(Ordering::Relaxed)
    );
    if m.blocks > 100 {
        println!("PASS");
        0
    } else {
        println!("FAIL: chain did not receive audio");
        1
    }
}

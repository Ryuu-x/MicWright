//! Dropout soak test: runs the engine on the default devices with monitoring off
//! (nothing audible) and reports underruns / overflows every 5 s.
//!
//! cargo run --release -p mw-engine --example soak -- [seconds]

use mw_engine::audio::{default_devices, Engine, Shared};
use std::sync::atomic::Ordering;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(30);
    let (input, send, monitor) = default_devices();
    let with_monitor = std::env::args().any(|a| a == "--monitor");
    let (input, send) = (input.expect("input"), send.expect("send device"));
    let mut params = mw_dsp::Params::far_mic();
    if std::env::args().any(|a| a == "--rnnoise") {
        params.ns_high_quality = 0;
    }
    let shared = Shared::new(params);
    shared.monitor.store(with_monitor, Ordering::Relaxed);
    let engine = Engine::start(&input, &send, if with_monitor { monitor.as_deref() } else { None }, shared.clone())?;
    println!("{} -> {} · latency {:.1} ms", engine.input_desc, engine.output_desc, engine.chain_latency_ms);
    for t in (5..=seconds).step_by(5) {
        std::thread::sleep(Duration::from_secs(5));
        let m = *shared.meters.lock().unwrap();
        println!(
            "{t:3}s underruns {} overflows {} · buffer {} ms · blocks {} · in {:.1} dBFS",
            shared.underruns.load(Ordering::Relaxed),
            shared.overflows.load(Ordering::Relaxed),
            shared.buffer_ms.load(Ordering::Relaxed),
            m.blocks,
            m.in_peak_db
        );
    }
    Ok(())
}

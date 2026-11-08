//! Phase 0: find out whether the original QuadCast reports its tap-to-mute over HID.
//!
//! Usage: `hid-probe [seconds]` — lists every HID collection of the mic, dumps the
//! report descriptors, then logs all input reports while you tap the mute sensor.

use anyhow::Result;
use hidapi::HidApi;
use std::time::{Duration, Instant};

const VID: u16 = 0x0951;
const PID: u16 = 0x16DF;

fn main() -> Result<()> {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let api = HidApi::new()?;

    let infos: Vec<_> = api.device_list().filter(|d| d.vendor_id() == VID && d.product_id() == PID).cloned().collect();
    if infos.is_empty() {
        println!("No HyperX QuadCast ({VID:04x}:{PID:04x}) HID collections found.");
        return Ok(());
    }

    let mut devices = Vec::new();
    for info in &infos {
        println!(
            "== interface {} usage_page 0x{:04x} usage 0x{:04x} ({})",
            info.interface_number(),
            info.usage_page(),
            info.usage(),
            describe(info.usage_page(), info.usage())
        );
        println!("   path: {}", info.path().to_string_lossy());
        match info.open_device(&api) {
            Ok(dev) => {
                let mut buf = [0u8; hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
                match dev.get_report_descriptor(&mut buf) {
                    Ok(n) => {
                        println!("   report descriptor ({n} bytes):");
                        dump_descriptor(&buf[..n]);
                    }
                    Err(e) => println!("   report descriptor: unavailable ({e})"),
                }
                dev.set_blocking_mode(false)?;
                devices.push((info.usage_page(), info.usage(), dev));
            }
            Err(e) => println!("   open failed: {e} (Windows keeps some collections, e.g. keyboards, exclusive)"),
        }
    }

    println!("\nListening for input reports for {seconds}s — tap the mute sensor a few times now...");
    let start = Instant::now();
    let mut count = 0;
    let mut buf = [0u8; 256];
    while start.elapsed() < Duration::from_secs(seconds) {
        for (page, usage, dev) in &devices {
            if let Ok(n) = dev.read_timeout(&mut buf, 5) {
                if n > 0 {
                    count += 1;
                    let hex: Vec<String> = buf[..n].iter().map(|b| format!("{b:02x}")).collect();
                    println!("[{:7.3}s] page 0x{page:04x}/0x{usage:04x}: {}", start.elapsed().as_secs_f32(), hex.join(" "));
                }
            }
        }
    }
    println!("\n{count} input report(s) received.");
    if count == 0 {
        println!("=> Mute state is NOT reported over HID on this unit; use the hotkey / digital-silence fallback.");
    }
    Ok(())
}

fn describe(page: u16, usage: u16) -> &'static str {
    match (page, usage) {
        (0x000c, 0x0001) => "Consumer Control",
        (0x000b, 0x0005) => "Telephony Headset",
        (0x000b, _) => "Telephony",
        (0xff00..=0xffff, _) => "Vendor-defined",
        _ => "?",
    }
}

/// Minimal HID report-descriptor item decoder (enough to spot usages like Phone Mute 0x2F).
fn dump_descriptor(d: &[u8]) {
    let mut i = 0;
    let mut depth = 0usize;
    while i < d.len() {
        let prefix = d[i];
        let size = match prefix & 0x03 {
            3 => 4,
            s => s as usize,
        };
        let tag = prefix & 0xFC;
        let mut val: u32 = 0;
        for k in 0..size {
            if let Some(b) = d.get(i + 1 + k) {
                val |= (*b as u32) << (8 * k);
            }
        }
        let name = match tag {
            0x04 => "Usage Page",
            0x08 => "Usage",
            0xA0 => "Collection",
            0xC0 => "End Collection",
            0x80 => "Input",
            0x90 => "Output",
            0xB0 => "Feature",
            0x84 => "Report ID",
            0x74 => "Report Size",
            0x94 => "Report Count",
            0x14 => "Logical Min",
            0x24 => "Logical Max",
            0x18 => "Usage Min",
            0x28 => "Usage Max",
            _ => "item",
        };
        if tag == 0xC0 {
            depth = depth.saturating_sub(1);
        }
        println!("     {}{name} 0x{val:x}", "  ".repeat(depth));
        if tag == 0xA0 {
            depth += 1;
        }
        i += 1 + size;
    }
}

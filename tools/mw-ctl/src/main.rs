//! Command-line control for the system-wide APO.
//!
//! ```text
//! mw-ctl status                       # endpoint, registration, live APO heartbeat
//! mw-ctl install  --dll apo\build\Release\mw_apo.dll   (elevated)
//! mw-ctl uninstall                                       (elevated)
//! mw-ctl preset far|quiet|noisy|beatbox
//! mw-ctl bypass on|off   ·   mw-ctl mute on|off
//! mw-ctl meters                       # live meters from the APO
//! ```

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use mw_dsp::Params;
use mw_ipc::{flags, Mapping, OpenMode};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(Parser)]
#[command(about = "MicWright control")]
struct Cli {
    /// Capture endpoint id or name substring (default: the active QuadCast).
    #[arg(long, global = true)]
    endpoint: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Status,
    /// List capture endpoints.
    Endpoints,
    /// Install the APO on the QuadCast and restart Windows Audio (elevated).
    Install {
        #[arg(long, default_value = r"apo\build\Release\mw_apo.dll")]
        dll: PathBuf,
        /// Don't restart the audio service.
        #[arg(long)]
        no_restart: bool,
    },
    /// Restore the original effects and unregister the APO (elevated).
    Uninstall {
        #[arg(long)]
        no_restart: bool,
    },
    Preset {
        #[arg(value_enum)]
        name: PresetName,
    },
    Bypass {
        #[arg(value_enum)]
        state: OnOff,
    },
    Mute {
        #[arg(value_enum)]
        state: OnOff,
    },
    /// Show live meters from the APO.
    Meters {
        #[arg(short, long, default_value_t = 10)]
        seconds: u64,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum PresetName {
    Far,
    Quiet,
    Noisy,
    Beatbox,
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Endpoints => {
            for e in mw_device::capture_endpoints()? {
                println!("{} {} [{}]", if e.state == 1 { "●" } else { "○" }, e.display_name(), e.id);
            }
            Ok(())
        }
        Cmd::Status => status(cli.endpoint.as_deref()),
        Cmd::Install { dll, no_restart } => {
            let ep = mw_device::find_endpoint(cli.endpoint.as_deref())?;
            println!("Endpoint: {} [{}]", ep.display_name(), ep.id);
            let installed = mw_device::register_dll(&dll)?;
            println!("Registered {}", installed.display());
            mw_device::attach(&ep)?;
            println!("Attached as stream effect (original effects backed up).");
            if !no_restart {
                println!("Restarting Windows Audio...");
                mw_device::restart_audio_service()?;
                mw_device::cleanup_install_dir();
            }
            println!("Done. Run `mw-ctl status` while an app records from the mic.");
            Ok(())
        }
        Cmd::Uninstall { no_restart } => {
            let ep = mw_device::find_endpoint(cli.endpoint.as_deref())?;
            mw_device::detach(&ep)?;
            println!("Restored original effects on {}.", ep.display_name());
            mw_device::unregister_dll()?;
            if !no_restart {
                mw_device::restart_audio_service()?;
                mw_device::cleanup_install_dir();
            }
            println!("Uninstalled.");
            Ok(())
        }
        Cmd::Preset { name } => edit(|p| {
            let keep = (p.enabled, p.mute);
            *p = match name {
                PresetName::Far => Params::far_mic(),
                PresetName::Quiet => Params::quiet_room(),
                PresetName::Noisy => Params::noisy_room(),
                PresetName::Beatbox => Params::beatbox(),
            };
            (p.enabled, p.mute) = keep;
        }),
        Cmd::Bypass { state } => edit(|p| p.enabled = matches!(state, OnOff::Off) as u32),
        Cmd::Mute { state } => edit(|p| p.mute = matches!(state, OnOff::On) as u32),
        Cmd::Meters { seconds } => meters(seconds),
    }
}

fn open() -> Result<Mapping> {
    Mapping::open(OpenMode::OpenExisting, &Params::default())
        .context("APO shared memory not found — the APO hasn't run yet (is something recording from the mic?)")
}

fn edit(f: impl FnOnce(&mut Params)) -> Result<()> {
    let m = open()?;
    let s = m.shared();
    let mut p = s.read_params();
    f(&mut p);
    s.write_params(&p);
    println!("ok (enabled={}, mute={})", p.enabled, p.mute);
    Ok(())
}

fn status(endpoint: Option<&str>) -> Result<()> {
    let ep = mw_device::find_endpoint(endpoint)?;
    let st = mw_device::apo_status(&ep)?;
    println!("Endpoint         {} [{}]", ep.display_name(), ep.id);
    println!("COM registered   {}{}", yes(st.com_registered), st.dll_path.map(|p| format!("  {}", p.display())).unwrap_or_default());
    if st.com_registered && !st.dll_exists {
        println!("                 ⚠ registered DLL path does not exist");
    }
    println!("Attached (SFX)   {}", yes(st.attached));
    println!("Enhancements on  {}", yes(st.enhancements_enabled));
    println!("Backup stored    {}", yes(st.backup_present));
    for (k, v) in &st.other_effects {
        println!("Other FX         {k} = {v}");
    }

    match Mapping::open(OpenMode::OpenExisting, &Params::default()) {
        Err(_) => println!("APO runtime      not running (no shared memory — open Voice Recorder/Discord to start a capture stream)"),
        Ok(m) => {
            let s = m.shared();
            let hb1 = s.heartbeat.load(Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(300));
            let hb2 = s.heartbeat.load(Ordering::Relaxed);
            let alive = hb2 != hb1;
            println!("APO runtime      {} ({} via {})", if alive { "PROCESSING ✔" } else { "idle" }, plural(s.apo_instances.load(Ordering::Relaxed), "instance"), m.name);
            println!(
                "Format           {} Hz × {} ch, latency {:.1} ms",
                s.apo_sample_rate.load(Ordering::Relaxed),
                s.apo_channels.load(Ordering::Relaxed),
                s.apo_latency_frames.load(Ordering::Relaxed) as f32 / 48.0
            );
            println!("Worst block      {} µs  ·  overruns {}", s.apo_max_process_us.load(Ordering::Relaxed), s.apo_overruns.load(Ordering::Relaxed));
            let f = s.apo_flags.load(Ordering::Relaxed);
            for (bit, msg) in [
                (flags::POISONED, "⚠ a panic was caught — passing through"),
                (flags::OVERRUN_BYPASS, "⚠ CPU budget exceeded — passing through"),
                (flags::UNSUPPORTED_FORMAT, "⚠ stream format not 48 kHz — passing through"),
            ] {
                if f & bit != 0 {
                    println!("                 {msg}");
                }
            }
            let p = s.read_params();
            println!("Params           enabled={} mute={} ns={} agc={} target={} dB", p.enabled, p.mute, p.ns_enabled, p.agc_enabled, p.agc_target_db);
        }
    }
    Ok(())
}

fn meters(seconds: u64) -> Result<()> {
    let m = open()?;
    let s = m.shared();
    println!("   in pk   out pk   out rms  vad   agc    comp  lim   noise");
    for _ in 0..seconds * 4 {
        if let Some(x) = s.try_read_meters() {
            println!(
                "{:7.1} {:8.1} {:8.1} {:5.2} {:+6.1} {:6.1} {:5.1} {:7.1}",
                x.in_peak_db, x.out_peak_db, x.out_rms_db, x.vad, x.agc_gain_db, -x.comp_gr_db, -x.limiter_gr_db, x.noise_floor_db
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

fn yes(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn plural(n: u32, w: &str) -> String {
    format!("{n} {w}{}", if n == 1 { "" } else { "s" })
}


//! Finds the QuadCast capture endpoint and attaches/detaches the MicWright APO.
//!
//! Attaching = writing our CLSID as the endpoint's stream effect (SFX) in
//! `MMDevices\Audio\Capture\{id}\FxProperties`. Every value we overwrite is backed up
//! first under `HKLM\SOFTWARE\MicWright\Backup\{id}` and restored on detach.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use winreg::enums::*;
use winreg::{RegKey, RegValue};

pub const APO_CLSID: &str = "{5C3E1A57-0B2D-4C5A-9E0F-51C3A57E0001}";
pub const QUADCAST_MATCH: &str = "quadcast";

const CAPTURE: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture";
const OUR_KEY: &str = r"SOFTWARE\MicWright";

const PKEY_DEVICE_FRIENDLY_NAME: &str = "{a45c254e-df1c-4efd-8020-67d146a850e0},2";
const PKEY_DEVICE_INTERFACE_NAME: &str = "{b3f8fa53-0004-438e-9003-51a46e139bfc},6";
/// DWORD; 1 = "Audio enhancements" off for the endpoint.
pub const PKEY_DISABLE_SYSFX: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";

/// Stream effect (SFX) CLSID.
pub const PKEY_FX_STREAM: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},5";
/// Composite SFX list (multi-sz), used by newer drivers instead of the single CLSID.
pub const PKEY_COMPOSITE_FX_STREAM: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},13";
/// Processing modes the SFX supports (multi-sz).
pub const PKEY_SFX_MODES: &str = "{d3993a3f-99c2-4402-b5ec-a92a0367664b},5";

/// Every FxProperties value we may write (and therefore back up).
const TOUCHED: [&str; 3] = [PKEY_FX_STREAM, PKEY_COMPOSITE_FX_STREAM, PKEY_SFX_MODES];

/// DEFAULT, COMMUNICATIONS, SPEECH, MEDIA. RAW is deliberately excluded (it must stay unprocessed).
pub const MODES: [&str; 4] = [
    "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}",
    "{98951333-B9CD-48B1-A0A3-FF40682D73F7}",
    "{FC1CFC9B-B9D1-4CFA-9E5C-F7A1E0FF6FF1}",
    "{4780004E-7133-41D8-8C74-660DADD2C0EE}",
];

#[derive(Debug, Clone)]
pub struct Endpoint {
    /// e.g. `{267e5a0d-1ac7-49e5-8f1d-7319e071f905}`
    pub id: String,
    pub name: String,
    pub interface: String,
    /// 1 = active
    pub state: u32,
}

impl Endpoint {
    pub fn display_name(&self) -> String {
        format!("{} ({})", self.name, self.interface)
    }

    fn key_path(&self) -> String {
        format!(r"{CAPTURE}\{}", self.id)
    }
}

fn hklm() -> RegKey {
    RegKey::predef(HKEY_LOCAL_MACHINE)
}

/// All capture endpoints Windows knows about.
pub fn capture_endpoints() -> Result<Vec<Endpoint>> {
    let root = hklm().open_subkey(CAPTURE).context("open MMDevices capture key")?;
    let mut out = Vec::new();
    for id in root.enum_keys().flatten() {
        let Ok(ep) = root.open_subkey(&id) else { continue };
        let state: u32 = ep.get_value("DeviceState").unwrap_or(0);
        let Ok(props) = ep.open_subkey("Properties") else { continue };
        let name: String = props.get_value(PKEY_DEVICE_FRIENDLY_NAME).unwrap_or_default();
        let interface: String = props.get_value(PKEY_DEVICE_INTERFACE_NAME).unwrap_or_default();
        out.push(Endpoint { id, name, interface, state });
    }
    Ok(out)
}

/// The active QuadCast microphone endpoint.
pub fn find_quadcast() -> Result<Endpoint> {
    capture_endpoints()?
        .into_iter()
        .filter(|e| e.state == 1)
        .find(|e| format!("{} {}", e.name, e.interface).to_lowercase().contains(QUADCAST_MATCH))
        .context("no active HyperX QuadCast capture endpoint found (is the mic plugged in?)")
}

pub fn find_endpoint(id_or_name: Option<&str>) -> Result<Endpoint> {
    match id_or_name {
        None => find_quadcast(),
        Some(s) => {
            let s = s.to_lowercase();
            capture_endpoints()?
                .into_iter()
                .find(|e| e.id.to_lowercase() == s || e.display_name().to_lowercase().contains(&s))
                .with_context(|| format!("no capture endpoint matching '{s}'"))
        }
    }
}

#[derive(Debug)]
pub struct ApoStatus {
    pub com_registered: bool,
    pub dll_path: Option<PathBuf>,
    pub dll_exists: bool,
    pub attached: bool,
    pub enhancements_enabled: bool,
    pub backup_present: bool,
    pub other_effects: Vec<(String, String)>,
}

pub fn apo_status(ep: &Endpoint) -> Result<ApoStatus> {
    let dll_path: Option<PathBuf> = hklm()
        .open_subkey(format!(r"SOFTWARE\Classes\CLSID\{APO_CLSID}\InprocServer32"))
        .and_then(|k| k.get_value::<String, _>(""))
        .ok()
        .map(PathBuf::from);
    let fx = hklm().open_subkey(format!(r"{}\FxProperties", ep.key_path())).ok();
    let sfx: String = fx.as_ref().and_then(|k| k.get_value(PKEY_FX_STREAM).ok()).unwrap_or_default();
    let disable: u32 = hklm()
        .open_subkey(format!(r"{}\Properties", ep.key_path()))
        .and_then(|k| k.get_value(PKEY_DISABLE_SYSFX))
        .unwrap_or(0);
    let mut other = Vec::new();
    if let Some(fx) = &fx {
        for (name, value) in fx.enum_values().flatten() {
            if !TOUCHED.contains(&name.as_str()) {
                other.push((name, value.to_string()));
            }
        }
    }
    Ok(ApoStatus {
        com_registered: dll_path.is_some(),
        dll_exists: dll_path.as_deref().is_some_and(Path::exists),
        dll_path,
        attached: sfx.eq_ignore_ascii_case(APO_CLSID),
        enhancements_enabled: disable == 0,
        backup_present: hklm().open_subkey(backup_path(ep)).is_ok(),
        other_effects: other,
    })
}

fn backup_path(ep: &Endpoint) -> String {
    format!(r"{OUR_KEY}\Backup\{}", ep.id)
}

pub fn is_elevated() -> bool {
    // Writing under HKLM\SOFTWARE is a reliable, dependency-free elevation probe.
    hklm().create_subkey_with_flags(OUR_KEY, KEY_WRITE).is_ok()
}

fn require_admin() -> Result<()> {
    if !is_elevated() {
        bail!("this needs an elevated (Administrator) prompt");
    }
    Ok(())
}

/// Default install location; readable by LocalService (audiodg).
pub fn install_dir() -> PathBuf {
    PathBuf::from(std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into())).join("MicWright")
}

/// Copies the DLL into Program Files and registers it (COM + audio engine APO list).
pub fn register_dll(src: &Path) -> Result<PathBuf> {
    require_admin()?;
    let dir = install_dir();
    std::fs::create_dir_all(&dir)?;
    let dst = dir.join("mw_apo.dll");
    if dst.exists() {
        // A loaded DLL can't be overwritten, but it can be renamed out of the way.
        let old = dir.join(format!("mw_apo.{}.old", std::process::id()));
        let _ = std::fs::rename(&dst, &old);
    }
    std::fs::copy(src, &dst).with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
    let status = Command::new("regsvr32").arg("/s").arg(&dst).status()?;
    if !status.success() {
        bail!("regsvr32 failed ({status})");
    }
    Ok(dst)
}

pub fn unregister_dll() -> Result<()> {
    require_admin()?;
    let dst = install_dir().join("mw_apo.dll");
    if dst.exists() {
        let _ = Command::new("regsvr32").args(["/s", "/u"]).arg(&dst).status();
    }
    Ok(())
}

/// Removes leftover renamed DLLs once audiodg has let go of them.
pub fn cleanup_install_dir() {
    if let Ok(rd) = std::fs::read_dir(install_dir()) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "old") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Attaches the APO as the endpoint's SFX (backing up whatever was there).
pub fn attach(ep: &Endpoint) -> Result<()> {
    require_admin()?;
    let fx = hklm()
        .open_subkey_with_flags(format!(r"{}\FxProperties", ep.key_path()), KEY_READ | KEY_SET_VALUE)
        .or_else(|_| hklm().create_subkey(format!(r"{}\FxProperties", ep.key_path())).map(|(k, _)| k))
        .context("open FxProperties for writing")?;

    let current: String = fx.get_value(PKEY_FX_STREAM).unwrap_or_default();
    if !current.eq_ignore_ascii_case(APO_CLSID) {
        backup(ep, &fx)?;
    }

    fx.set_value(PKEY_FX_STREAM, &APO_CLSID.to_string())?;
    let _ = fx.delete_value(PKEY_COMPOSITE_FX_STREAM);
    let modes: Vec<String> = MODES.iter().map(|s| s.to_string()).collect();
    fx.set_value(PKEY_SFX_MODES, &modes)?;

    // Make sure "Audio enhancements" is on, otherwise the engine skips all SFX.
    if let Ok(props) = hklm().open_subkey_with_flags(format!(r"{}\Properties", ep.key_path()), KEY_READ | KEY_SET_VALUE) {
        let disabled: u32 = props.get_value(PKEY_DISABLE_SYSFX).unwrap_or(0);
        if disabled != 0 {
            props.set_value(PKEY_DISABLE_SYSFX, &0u32)?;
        }
    }
    Ok(())
}

fn backup(ep: &Endpoint, fx: &RegKey) -> Result<()> {
    let (bk, _) = hklm().create_subkey(backup_path(ep))?;
    let mut absent = Vec::new();
    for name in TOUCHED {
        match fx.get_raw_value(name) {
            Ok(v) => bk.set_raw_value(name, &v)?,
            Err(_) => absent.push(name.to_string()),
        }
    }
    bk.set_value("__absent", &absent)?;
    Ok(())
}

/// Restores the endpoint's original effects and forgets the backup.
pub fn detach(ep: &Endpoint) -> Result<()> {
    require_admin()?;
    let fx = hklm()
        .open_subkey_with_flags(format!(r"{}\FxProperties", ep.key_path()), KEY_READ | KEY_SET_VALUE)
        .context("open FxProperties for writing")?;
    match hklm().open_subkey(backup_path(ep)) {
        Ok(bk) => {
            let absent: Vec<String> = bk.get_value("__absent").unwrap_or_default();
            for name in TOUCHED {
                if absent.iter().any(|a| a == name) {
                    let _ = fx.delete_value(name);
                } else if let Ok(v) = bk.get_raw_value(name) {
                    fx.set_raw_value(name, &RegValue { bytes: v.bytes, vtype: v.vtype })?;
                }
            }
            drop(bk);
            let _ = hklm().delete_subkey_all(backup_path(ep));
        }
        Err(_) => {
            // No backup: just remove ours if present.
            let cur: String = fx.get_value(PKEY_FX_STREAM).unwrap_or_default();
            if cur.eq_ignore_ascii_case(APO_CLSID) {
                fx.delete_value(PKEY_FX_STREAM)?;
                let _ = fx.delete_value(PKEY_SFX_MODES);
            }
        }
    }
    Ok(())
}

/// Restarts the Windows Audio service so the engine rebuilds its effect graphs.
pub fn restart_audio_service() -> Result<()> {
    require_admin()?;
    let status = Command::new("powershell")
        .args(["-NoProfile", "-Command", "Restart-Service -Name Audiosrv -Force"])
        .status()?;
    if !status.success() {
        bail!("restarting Audiosrv failed ({status})");
    }
    Ok(())
}

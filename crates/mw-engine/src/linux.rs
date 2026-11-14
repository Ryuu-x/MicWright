//! Linux sound-server integration over the PulseAudio protocol, which PipeWire desktops
//! also serve (pipewire-pulse). It gives other apps a "MicWright Virtual Mic" without any
//! third-party driver, and switches the desktop's default mic.
//!
//! The virtual mic is two server modules: a null sink the engine plays into, and a
//! source remapped from that sink's monitor, which is what apps record from. Modules
//! outlive the client that loaded them, so they are found by name rather than tracked.

use anyhow::{bail, Context, Result};
use pulseaudio::protocol::{self, Command};
use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// The sink the engine sends to. Apps see it as a speaker, so its name says what it is.
pub const SINK: &str = "micwright_feed";
pub const SINK_DESC: &str = "MicWright Virtual Mic Input";
/// The microphone other apps pick.
pub const SOURCE: &str = "micwright_mic";
pub const SOURCE_DESC: &str = "MicWright Virtual Mic";

/// One short-lived connection to the sound server.
struct Pulse {
    sock: BufReader<UnixStream>,
    seq: u32,
    version: u16,
}

impl Pulse {
    fn connect() -> Result<Self> {
        let path = pulseaudio::socket_path_from_env().context("no PulseAudio or PipeWire sound server is running")?;
        let stream = UnixStream::connect(&path).with_context(|| format!("can't reach the sound server at {}", path.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut sock = BufReader::new(stream);
        let cookie = pulseaudio::cookie_path_from_env().and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
        let auth = protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie };
        protocol::write_command_message(sock.get_mut(), 0, &Command::Auth(auth), protocol::MAX_VERSION)?;
        let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION)?;
        let version = protocol::MAX_VERSION.min(reply.version);
        let mut props = protocol::Props::new();
        props.set(protocol::Prop::ApplicationName, c"MicWright");
        protocol::write_command_message(sock.get_mut(), 1, &Command::SetClientName(props), version)?;
        protocol::read_reply_message::<protocol::SetClientNameReply>(&mut sock, version)?;
        Ok(Self { sock, seq: 2, version })
    }

    fn send(&mut self, cmd: &Command) -> Result<()> {
        protocol::write_command_message(self.sock.get_mut(), self.seq, cmd, self.version)?;
        self.seq += 1;
        Ok(())
    }

    fn reply<R: protocol::CommandReply>(&mut self, cmd: Command) -> Result<R> {
        self.send(&cmd)?;
        Ok(protocol::read_reply_message::<R>(&mut self.sock, self.version)?.1)
    }

    fn ack(&mut self, cmd: Command) -> Result<()> {
        self.send(&cmd)?;
        protocol::read_ack_message(&mut self.sock)?;
        Ok(())
    }

    fn load_module(&mut self, name: &str, args: &str) -> Result<()> {
        let params = protocol::LoadModuleParams { name: CString::new(name)?, arguments: Some(CString::new(args)?) };
        self.reply::<protocol::LookupReply>(Command::LoadModule(params)).with_context(|| format!("the sound server refused {name}"))?;
        Ok(())
    }

    /// Indexes of the modules that make up the virtual mic, the source's first.
    fn our_modules(&mut self) -> Result<Vec<u32>> {
        let mut modules: Vec<(bool, u32)> = self
            .reply::<protocol::ModuleInfoList>(Command::GetModuleInfoList)?
            .into_iter()
            .filter_map(|m| {
                let args = m.argument?.to_string_lossy().into_owned();
                let source = args.contains(&format!("source_name={SOURCE}"));
                (source || args.contains(&format!("sink_name={SINK}"))).then_some((!source, m.index))
            })
            .collect();
        modules.sort();
        Ok(modules.into_iter().map(|(_, index)| index).collect())
    }
}

/// Creates the virtual mic unless it is already there (e.g. left by an earlier run).
///
/// PulseAudio's module-switch-on-connect makes any new device the default, which would
/// hand every app our mic without asking: the defaults from before are put back.
pub fn ensure_virtual_mic() -> Result<()> {
    let mut p = Pulse::connect()?;
    let before = p.reply::<protocol::ServerInfo>(Command::GetServerInfo)?;
    let created = load_virtual_mic(&mut p)?;
    if created {
        let after = p.reply::<protocol::ServerInfo>(Command::GetServerInfo)?;
        if let Some(source) = before.default_source_name.filter(|s| after.default_source_name.as_ref() != Some(s)) {
            p.ack(Command::SetDefaultSource(source))?;
        }
        if let Some(sink) = before.default_sink_name.filter(|s| after.default_sink_name.as_ref() != Some(s)) {
            p.ack(Command::SetDefaultSink(sink))?;
        }
    }
    Ok(())
}

/// Loads whichever half of the virtual mic is missing; true if it loaded anything.
fn load_virtual_mic(p: &mut Pulse) -> Result<bool> {
    let mut created = false;
    let sinks = p.reply::<protocol::SinkInfoList>(Command::GetSinkInfoList)?;
    if !sinks.iter().any(|s| s.name.to_bytes() == SINK.as_bytes()) {
        p.load_module(
            "module-null-sink",
            &format!("sink_name={SINK} rate=48000 channels=1 channel_map=mono sink_properties='device.description=\"{SINK_DESC}\"'"),
        )?;
        created = true;
    }
    let sources = p.reply::<protocol::SourceInfoList>(Command::GetSourceInfoList)?;
    if !sources.iter().any(|s| s.name.to_bytes() == SOURCE.as_bytes()) {
        p.load_module(
            "module-remap-source",
            &format!("master={SINK}.monitor source_name={SOURCE} source_properties='device.description=\"{SOURCE_DESC}\"'"),
        )?;
        created = true;
    }
    Ok(created)
}

/// Removes the virtual mic. Apps recording from it move to the default mic.
pub fn remove_virtual_mic() -> Result<()> {
    let mut p = Pulse::connect()?;
    for index in p.our_modules()? {
        p.ack(Command::UnloadModule(index))?;
    }
    Ok(())
}

/// The desktop's default mic (a source name, not its description).
pub fn default_source() -> Result<Option<String>> {
    let info = Pulse::connect()?.reply::<protocol::ServerInfo>(Command::GetServerInfo)?;
    Ok(info.default_source_name.map(|n| n.to_string_lossy().into_owned()))
}

pub fn set_default_source(name: &str) -> Result<()> {
    let mut p = Pulse::connect()?;
    let sources = p.reply::<protocol::SourceInfoList>(Command::GetSourceInfoList)?;
    if !sources.iter().any(|s| s.name.to_bytes() == name.as_bytes()) {
        bail!("the mic '{name}' isn't connected");
    }
    p.ack(Command::SetDefaultSource(CString::new(name)?))
}

/// Whether a source with this name exists right now.
pub fn source_exists(name: &str) -> bool {
    Pulse::connect()
        .and_then(|mut p| p.reply::<protocol::SourceInfoList>(Command::GetSourceInfoList))
        .is_ok_and(|sources| sources.iter().any(|s| s.name.to_bytes() == name.as_bytes()))
}

/// Descriptions of sources, sorted into (our own virtual mic and its feed's monitor,
/// which would loop our output back in; other "Monitor of …" sources, which record a
/// speaker). Monitors are rarely a mic, but some setups (a null sink fed by another
/// app) use one as theirs.
pub fn special_inputs() -> (Vec<String>, Vec<String>) {
    let Ok(sources) = Pulse::connect().and_then(|mut p| p.reply::<protocol::SourceInfoList>(Command::GetSourceInfoList)) else {
        return Default::default();
    };
    let mut own = Vec::new();
    let mut monitors = Vec::new();
    for s in sources {
        let ours = s.name.to_bytes() == SOURCE.as_bytes() || s.monitor_of_sink_name.as_ref().is_some_and(|n| n.to_bytes() == SINK.as_bytes());
        let monitor = s.monitor_of_sink_index.is_some();
        let desc = s.description.unwrap_or(s.name).to_string_lossy().into_owned();
        if ours {
            own.push(desc);
        } else if monitor {
            monitors.push(desc);
        }
    }
    (own, monitors)
}

/// The `pulseaudio` crate looks for `$PULSE_RUNTIME_PATH/pulse/native`, but PulseAudio
/// itself puts the socket at `$PULSE_RUNTIME_PATH/native`. Pointing `PULSE_SERVER` at it
/// makes both cpal and this module find the server. Call before starting any threads.
pub fn fix_server_path() {
    if std::env::var_os("PULSE_SERVER").is_some() {
        return;
    }
    if let Some(dir) = std::env::var_os("PULSE_RUNTIME_PATH") {
        let socket = std::path::Path::new(&dir).join("native");
        if socket.exists() {
            std::env::set_var("PULSE_SERVER", format!("unix:{}", socket.display()));
        }
    }
}

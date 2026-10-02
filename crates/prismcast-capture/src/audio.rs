//! Read-only bounded PipeWire discovery and explicit ephemeral audio selection.
//! No discovery call opens a capture stream or constitutes a Core authorization.
use gstreamer::{self as gst, prelude::*};
use prismcast_core::{
    CaptureGeneration, Error, PipeWireAudioMode, PipeWireAudioSettings, Result, SourceId,
};
use serde_json::Value;
use std::{
    collections::HashSet,
    io::Read,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub const MAX_AUDIO_TARGETS: usize = 128;
const MAX_DISCOVERY_BYTES: usize = 2 * 1024 * 1024;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded picker metadata. A node name is an advisory selector, not a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTargetInfo {
    pub node_name: String,
    pub label: String,
    pub mode: PipeWireAudioMode,
}
#[derive(Debug, Clone)]
struct Node {
    info: AudioTargetInfo,
    serial: u64,
}
#[derive(Debug)]
struct Inventory {
    socket: PathBuf,
    cookie: u32,
    nodes: Vec<Node>,
}

/// Frozen identity, local to one explicit source request and daemon instance.
/// No serialization implementation exists; generation is allocated by Core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedAudioTarget {
    socket: PathBuf,
    source_id: SourceId,
    generation: CaptureGeneration,
    settings: PipeWireAudioSettings,
    serial: u64,
    cookie: u32,
}
impl AuthorizedAudioTarget {
    pub fn source_id(&self) -> SourceId {
        self.source_id
    }
    pub fn generation(&self) -> CaptureGeneration {
        self.generation
    }
    pub fn settings(&self) -> &PipeWireAudioSettings {
        &self.settings
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn remote_socket() -> Result<PathBuf> {
    let remote = std::env::var_os("PIPEWIRE_REMOTE").unwrap_or_else(|| "pipewire-0".into());
    let remote = PathBuf::from(remote);
    let socket = if remote.is_absolute() {
        remote
    } else {
        if remote.components().count() != 1 || remote.file_name().is_none() {
            return Err(Error::InvalidInput(
                "unsupported PipeWire remote endpoint".into(),
            ));
        }
        let runtime = std::env::var_os("PIPEWIRE_RUNTIME_DIR")
            .or_else(|| std::env::var_os("XDG_RUNTIME_DIR"))
            .ok_or_else(|| Error::Media("PipeWire runtime directory is unavailable".into()))?;
        let runtime = PathBuf::from(runtime);
        if !runtime.is_absolute() {
            return Err(Error::InvalidInput(
                "PipeWire runtime directory must be absolute".into(),
            ));
        }
        runtime.join(remote)
    };
    if socket.as_os_str().as_bytes().len() >= 108
        || socket
            .as_os_str()
            .as_bytes()
            .iter()
            .any(|byte| *byte < 32 || *byte == 127)
    {
        return Err(Error::InvalidInput(
            "unsupported PipeWire socket path".into(),
        ));
    }
    Ok(socket)
}
fn read_inventory(socket: &Path) -> Result<Inventory> {
    // Fixed argv, no shell interpolation; stderr is discarded rather than queued.
    let mut process = Process(
        Command::new("pw-dump")
            .arg("--remote")
            .arg(socket)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| Error::Media(format!("PipeWire discovery cannot start: {error}")))?,
    );
    let stdout = process
        .0
        .stdout
        .take()
        .ok_or_else(|| Error::Media("PipeWire discovery has no stdout".into()))?;
    let (completed, result) = mpsc::sync_channel(1);
    // One joined reader per synchronous discovery; byte cap bounds its allocation.
    let reader = thread::Builder::new()
        .name("prismcast-pw-discovery-read".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let outcome = stdout
                .take((MAX_DISCOVERY_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = completed.send(outcome);
        })?;
    let deadline = Instant::now() + DISCOVERY_TIMEOUT;
    let mut bytes = None;
    let outcome = loop {
        match result.try_recv() {
            Ok(Ok(value)) if value.len() > MAX_DISCOVERY_BYTES => {
                break Err(Error::Media(
                    "PipeWire discovery output exceeds its byte limit".into(),
                ))
            }
            Ok(Ok(value)) => bytes = Some(value),
            Ok(Err(error)) => {
                break Err(Error::Media(format!(
                    "PipeWire discovery read failed: {error}"
                )))
            }
            Err(_) => {}
        }
        match process.0.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    break Err(Error::Media("PipeWire discovery failed".into()));
                }
                if let Some(bytes) = bytes.take() {
                    break Ok(bytes);
                }
            }
            Err(error) => {
                break Err(Error::Media(format!(
                    "PipeWire discovery wait failed: {error}"
                )))
            }
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            break Err(Error::Media("PipeWire discovery timed out".into()));
        }
        thread::sleep(Duration::from_millis(5));
    };
    // Kill/reap before joining, so read EOF is guaranteed for the fixed pw-dump
    // helper (which creates no descendant processes retaining its output pipe).
    drop(process);
    reader
        .join()
        .map_err(|_| Error::Media("PipeWire discovery reader panicked".into()))?;
    let mut inventory = parse_inventory(&outcome?)?;
    inventory.socket = socket.to_path_buf();
    Ok(inventory)
}
fn parse_inventory(bytes: &[u8]) -> Result<Inventory> {
    if bytes.len() > MAX_DISCOVERY_BYTES {
        return Err(Error::Media("PipeWire discovery output too large".into()));
    }
    let objects: Vec<Value> = serde_json::from_slice(bytes)
        .map_err(|_| Error::Media("invalid PipeWire discovery JSON".into()))?;
    let core = objects
        .iter()
        .find(|object| {
            object.get("type").and_then(Value::as_str) == Some("PipeWire:Interface:Core")
        })
        .ok_or_else(|| Error::Media("PipeWire discovery lacks daemon identity".into()))?;
    let cookie = core
        .pointer("/info/cookie")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| Error::Media("invalid PipeWire daemon identity".into()))?;
    let mut nodes = Vec::new();
    for object in objects {
        if object.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(props) = object.pointer("/info/props") else {
            continue;
        };
        let mode = match props.get("media.class").and_then(Value::as_str) {
            Some("Audio/Source") => PipeWireAudioMode::Input,
            Some("Audio/Sink") => PipeWireAudioMode::Output,
            Some("Stream/Output/Audio") => PipeWireAudioMode::Application,
            _ => continue,
        };
        let Some(name) = props.get("node.name").and_then(Value::as_str) else {
            continue;
        };
        let settings = PipeWireAudioSettings {
            schema_version: 1,
            target: name.into(),
            mode,
        };
        if settings.validate().is_err() {
            continue;
        }
        let serial = props
            .get("object.serial")
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()));
        let Some(serial) = serial.filter(|value| *value != 0) else {
            continue;
        };
        if nodes.len() == MAX_AUDIO_TARGETS {
            return Err(Error::Media(
                "PipeWire audio target capacity exhausted".into(),
            ));
        }
        let raw_label = props
            .get("node.description")
            .or_else(|| props.get("application.name"))
            .and_then(Value::as_str)
            .unwrap_or(name);
        let label = raw_label
            .chars()
            .filter(|c| !c.is_control())
            .scan(0, |bytes, c| {
                *bytes += c.len_utf8();
                (*bytes <= 512).then_some(c)
            })
            .collect();
        nodes.push(Node {
            info: AudioTargetInfo {
                node_name: name.into(),
                label,
                mode,
            },
            serial,
        });
    }
    Ok(Inventory {
        socket: PathBuf::new(),
        cookie,
        nodes,
    })
}
fn select(
    inventory: &Inventory,
    source_id: SourceId,
    generation: CaptureGeneration,
    settings: &PipeWireAudioSettings,
) -> Result<AuthorizedAudioTarget> {
    settings.validate()?;
    if generation.value() == 0 {
        return Err(Error::InvalidInput(
            "audio capture requires a live request generation".into(),
        ));
    }
    let mut matches = inventory
        .nodes
        .iter()
        .filter(|node| node.info.node_name == settings.target && node.info.mode == settings.mode);
    let node = matches
        .next()
        .ok_or_else(|| Error::NotFound("selected PipeWire audio target is unavailable".into()))?;
    if matches.next().is_some() {
        return Err(Error::InvalidInput(
            "selected PipeWire audio target name is ambiguous".into(),
        ));
    }
    Ok(AuthorizedAudioTarget {
        socket: inventory.socket.clone(),
        source_id,
        generation,
        settings: settings.clone(),
        serial: node.serial,
        cookie: inventory.cookie,
    })
}
fn verify(inventory: &Inventory, target: &AuthorizedAudioTarget) -> Result<()> {
    let current = select(
        inventory,
        target.source_id,
        target.generation,
        &target.settings,
    )?;
    if &current != target {
        return Err(Error::Media(
            "authorized PipeWire audio target was replaced; authorize again".into(),
        ));
    }
    Ok(())
}

/// Run on a blocking/native owner thread; bounded read-only picker discovery.
pub fn discover_audio_targets() -> Result<Vec<AudioTargetInfo>> {
    Ok(read_inventory(&remote_socket()?)?
        .nodes
        .into_iter()
        .map(|node| node.info)
        .collect())
}
/// Resolve exactly one current object; caller must possess a Core request effect.
pub fn resolve_audio_target(
    source_id: SourceId,
    generation: CaptureGeneration,
    settings: &PipeWireAudioSettings,
) -> Result<AuthorizedAudioTarget> {
    select(
        &read_inventory(&remote_socket()?)?,
        source_id,
        generation,
        settings,
    )
}
/// One bounded inventory read verifies every grant before a graph rebuild.
pub fn validate_audio_targets(targets: &[AuthorizedAudioTarget]) -> Result<()> {
    if targets.is_empty() {
        return Ok(());
    }
    if targets.len() > 8 {
        return Err(Error::InvalidInput(
            "audio capture supports at most eight grants".into(),
        ));
    }
    let inventory = read_inventory(&targets[0].socket)?;
    let mut ids = HashSet::new();
    for target in targets {
        if !ids.insert(target.source_id) {
            return Err(Error::InvalidInput(
                "duplicate authorized audio source".into(),
            ));
        }
        if target.socket != inventory.socket {
            return Err(Error::InvalidInput(
                "audio grants refer to different PipeWire endpoints".into(),
            ));
        }
        verify(&inventory, target)?;
    }
    Ok(())
}
/// Construct a source only after the owner verified the whole allowlist.
/// Construction alone opens no stream; the containing pipeline starts it.
fn build_authorized_audio_source(
    target: &AuthorizedAudioTarget,
    remote: &OwnedFd,
) -> Result<gst::Element> {
    gst::init().map_err(|error| Error::Media(error.to_string()))?;
    let source = gst::ElementFactory::make("pipewiresrc")
        .build()
        .map_err(|error| Error::Media(error.to_string()))?;
    for property in [
        "target-object",
        "stream-properties",
        "min-buffers",
        "max-buffers",
        "use-bufferpool",
        "provide-clock",
        "on-disconnect",
        "fd",
        "autoconnect",
    ] {
        if source.find_property(property).is_none() {
            return Err(Error::Media(format!(
                "installed PipeWire source lacks {property}"
            )));
        }
    }
    let properties = gst::Structure::builder("props")
        .field("media.type", "Audio")
        .field("media.category", "Capture")
        .field("media.class", "Stream/Input/Audio")
        .field(
            "node.name",
            format!(
                "prismcast.audio.{}.{}",
                target.source_id,
                target.generation.value()
            ),
        )
        .field("node.dont-fallback", true)
        .field("node.dont-reconnect", true)
        .field("node.dont-move", true)
        .field(
            "stream.capture.sink",
            target.settings.mode == PipeWireAudioMode::Output,
        )
        .build();
    source.set_property("target-object", target.serial.to_string());
    source.set_property("fd", remote.as_raw_fd());
    source.set_property("autoconnect", true);
    source.set_property("stream-properties", properties);
    source.set_property("min-buffers", 2_i32);
    source.set_property("max-buffers", 8_i32);
    source.set_property("use-bufferpool", false);
    source.set_property("provide-clock", false);
    source.set_property_from_str("on-disconnect", "error");
    Ok(source)
}

/// Fresh, not-yet-consumed daemon connection plus inactive capture element.
/// Retain the owned descriptor until the containing pipeline has reached NULL.
pub struct ConnectedAudioSource {
    source_id: SourceId,
    element: gst::Element,
    remote: OwnedFd,
}
/// Interrupt protocol waits before deliberate NULL teardown; the plugin's dup
/// refers to the same underlying connection as this retained descriptor.
pub fn disconnect_audio_remote(remote: &OwnedFd) {
    let _ = socket2::SockRef::from(remote).shutdown(std::net::Shutdown::Both);
}
/// Reject a known-closed pinned connection before invoking plugin state changes.
/// Peek consumes no PipeWire protocol bytes. This is a freshness preflight, not
/// an identity lookup: the connected socket remains the daemon-epoch guard.
pub fn check_audio_remote(remote: &OwnedFd) -> Result<()> {
    let socket = socket2::SockRef::from(remote);
    socket.set_nonblocking(true)?;
    let result = socket.peek(&mut [std::mem::MaybeUninit::uninit(); 1]);
    socket.set_nonblocking(false)?;
    match result {
        Ok(0) => Err(Error::Media(
            "authorized PipeWire connection closed; authorize again".into(),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        Err(error) => Err(Error::Media(format!(
            "authorized PipeWire connection failed: {error}"
        ))),
    }
}
impl ConnectedAudioSource {
    pub fn into_parts(self) -> (SourceId, gst::Element, OwnedFd) {
        (self.source_id, self.element, self.remote)
    }
}
/// Pin all sockets before a single identity check. A later daemon restart kills
/// these old connections rather than allowing the replacement daemon to bind a
/// reused serial. GStreamer duplicates each supplied descriptor internally.
pub fn connect_authorized_audio_sources(
    targets: &[AuthorizedAudioTarget],
) -> Result<Vec<ConnectedAudioSource>> {
    if targets.len() > 8 {
        return Err(Error::InvalidInput(
            "audio capture supports at most eight grants".into(),
        ));
    }
    let mut remotes = Vec::new();
    for target in targets {
        remotes.push(connect_socket(&target.socket)?);
    }
    validate_audio_targets(targets)?;
    targets
        .iter()
        .zip(remotes)
        .map(|(target, remote)| {
            let element = build_authorized_audio_source(target, &remote)?;
            Ok(ConnectedAudioSource {
                source_id: target.source_id,
                element,
                remote,
            })
        })
        .collect()
}
fn connect_socket(path: &Path) -> Result<OwnedFd> {
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
    socket.set_nonblocking(true)?;
    let address = socket2::SockAddr::unix(path)?;
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match socket.connect(&address) {
            Ok(()) => {
                socket.set_nonblocking(false)?;
                return Ok(OwnedFd::from(socket));
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5))
            }
            Err(error) => {
                return Err(Error::Media(format!(
                    "PipeWire socket connection failed: {error}"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connect_to_full_unix_backlog_has_a_finite_deadline() {
        let path = std::env::temp_dir().join(format!("prismcast-backlog-{}", SourceId::new()));
        let listener =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        listener
            .bind(&socket2::SockAddr::unix(&path).unwrap())
            .unwrap();
        listener.listen(1).unwrap();
        let _first = connect_socket(&path).unwrap();
        let _second = connect_socket(&path).unwrap();
        let started = Instant::now();
        assert!(connect_socket(&path).is_err());
        assert!(started.elapsed() >= Duration::from_millis(240));
        assert!(started.elapsed() < Duration::from_secs(1));
        std::fs::remove_file(path).unwrap();
    }
    fn inventory(cookie: u32, serial: u64, duplicate: bool) -> Inventory {
        let mut objects = vec![
            serde_json::json!({"type":"PipeWire:Interface:Core","info":{"cookie":cookie}}),
            serde_json::json!({"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"chosen","media.class":"Audio/Source","object.serial":serial}}}),
        ];
        if duplicate {
            objects.push(objects[1].clone());
        }
        parse_inventory(&serde_json::to_vec(&objects).unwrap()).unwrap()
    }
    #[test]
    fn selection_rejects_missing_ambiguous_replaced_and_restarted_objects() {
        let settings = PipeWireAudioSettings {
            schema_version: 1,
            target: "chosen".into(),
            mode: PipeWireAudioMode::Input,
        };
        let id = SourceId::new();
        let gen = CaptureGeneration::new(1);
        let target = select(&inventory(100, 20, false), id, gen, &settings).unwrap();
        assert!(verify(&inventory(100, 20, false), &target).is_ok());
        assert!(verify(&inventory(100, 21, false), &target).is_err());
        assert!(verify(&inventory(101, 20, false), &target).is_err());
        assert!(select(&inventory(100, 20, true), id, gen, &settings).is_err());
        let missing = PipeWireAudioSettings {
            target: "missing".into(),
            ..settings.clone()
        };
        assert!(select(&inventory(100, 20, false), id, gen, &missing).is_err());
        let wrong = PipeWireAudioSettings {
            mode: PipeWireAudioMode::Application,
            ..settings
        };
        assert!(select(&inventory(100, 20, false), id, gen, &wrong).is_err());
    }
    #[test]
    fn discovery_classes_metadata_and_output_bounds_are_explicit() {
        assert!(parse_inventory(b"[]").is_err());
        assert!(parse_inventory(&vec![b' '; MAX_DISCOVERY_BYTES + 1]).is_err());
        let mut objects =
            vec![serde_json::json!({"type":"PipeWire:Interface:Core","info":{"cookie":1}})];
        for (serial, class) in [
            (1, "Audio/Source"),
            (2, "Audio/Sink"),
            (3, "Stream/Output/Audio"),
            (4, "Stream/Input/Audio"),
            (5, "Video/Source"),
        ] {
            objects.push(serde_json::json!({"type":"PipeWire:Interface:Node","info":{"props":{"node.name":format!("node{serial}"),"media.class":class,"object.serial":serial,"node.description":"x".repeat(600)}}}));
        }
        let parsed = parse_inventory(&serde_json::to_vec(&objects).unwrap()).unwrap();
        assert_eq!(parsed.nodes.len(), 3);
        assert!(parsed.nodes.iter().all(|node| node.info.label.len() == 512));
        objects.extend((0..129).map(|i| serde_json::json!({"type":"PipeWire:Interface:Node","info":{"props":{"node.name":format!("extra{i}"),"media.class":"Audio/Source","object.serial":i+10}}})));
        assert!(parse_inventory(&serde_json::to_vec(&objects).unwrap()).is_err());
    }
}

//! Test-only private PipeWire daemon. Never discovers or captures host hardware.
#![allow(dead_code)]
use prismcast_core::{PipeWireAudioMode, PipeWireAudioSettings};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const CONFIG: &str = r#"context.properties = { core.daemon = true core.name = prismcast-test default.clock.rate = 48000 }
context.spa-libs = { audio.convert.* = audioconvert/libspa-audioconvert audio.adapt = audioconvert/libspa-audioconvert support.* = support/libspa-support audiotestsrc = audiotestsrc/libspa-audiotestsrc }
context.modules = [ { name = libpipewire-module-protocol-native } { name = libpipewire-module-metadata } { name = libpipewire-module-spa-node-factory } { name = libpipewire-module-client-node } { name = libpipewire-module-adapter } { name = libpipewire-module-link-factory } { name = libpipewire-module-access } ]
context.objects = [
 { factory = spa-node-factory args = { factory.name = support.node.driver node.name = Dummy-Driver priority.driver = 20000 } }
 { factory = adapter args = { factory.name = audiotestsrc node.name = prismcast-test-source media.class = Audio/Source audio.position = [ FL FR ] node.param.Props = { frequency = 440.0 volume = 0.5 live = true } } }
 { factory = adapter args = { factory.name = support.null-audio-sink node.name = prismcast-test-sink media.class = Audio/Sink audio.position = [ FL FR ] } }
]
"#;

/// Parent wrapper isolates environment in a subprocess, including PipeWire and
/// WirePlumber paths. A private policy-only profile has no hardware enumerators.
pub fn run_isolated(test_name: &str) {
    run_isolated_mode(test_name, false);
}
pub fn run_isolated_closed_fd(test_name: &str) {
    run_isolated_mode(test_name, true);
}
fn run_isolated_mode(test_name: &str, closed_fd: bool) {
    let directory = std::env::temp_dir().join(format!(
        "prismcast-pw-{}-{}",
        std::process::id(),
        prismcast_core::SourceId::new()
    ));
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    if closed_fd {
        command.env("PRISMCAST_TEST_CLOSED_FD_GST", "1");
    }
    let mut child = WorkerGroup(
        command
            .process_group(0)
            .args([
                "--exact",
                test_name,
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env("PRISMCAST_PRIVATE_AUDIO_FIXTURE", &directory)
            .env("PIPEWIRE_RUNTIME_DIR", &directory)
            .env("XDG_RUNTIME_DIR", &directory)
            .env("PIPEWIRE_REMOTE", "prismcast-test")
            .env("XDG_STATE_HOME", directory.join("state"))
            .env("XDG_CONFIG_HOME", directory.join("config"))
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(40);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            panic!("isolated worker timed out; logs in {}", directory.display());
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "isolated worker failed; logs in {}",
        directory.display()
    );
    fs::remove_dir_all(directory).unwrap();
}

struct WorkerGroup(Child);
impl Drop for WorkerGroup {
    fn drop(&mut self) {
        // Private process group includes daemon/policy/playback grandchildren;
        // killing only the worker would bypass their Rust Drop cleanup.
        let _ = Command::new("/usr/bin/kill")
            .args(["-KILL", "--", &format!("-{}", self.0.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.0.wait();
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
pub struct Fixture {
    pub directory: PathBuf,
    application: Option<Process>,
    sentinel: Option<Process>,
    policy: Option<Process>,
    daemon: Option<Process>,
}
impl Fixture {
    pub fn start() -> Self {
        let directory = PathBuf::from(
            std::env::var_os("PRISMCAST_PRIVATE_AUDIO_FIXTURE")
                .expect("only run fixture through subprocess wrapper"),
        );
        assert_eq!(
            std::env::var_os("PIPEWIRE_RUNTIME_DIR").unwrap(),
            directory.as_os_str()
        );
        assert_eq!(std::env::var("PIPEWIRE_REMOTE").unwrap(), "prismcast-test");
        assert_eq!(directory.parent().unwrap(), std::env::temp_dir());
        assert!(directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("prismcast-pw-"));
        fs::write(directory.join("pipewire.conf"), CONFIG).unwrap();
        let mut fixture = Self {
            directory,
            application: None,
            sentinel: None,
            policy: None,
            daemon: None,
        };
        fixture.start_daemon();
        fixture.restart_application();
        fixture.sentinel = Some(fixture.playback("prismcast-test-sentinel", "0.5", "997"));
        fixture.wait_targets();
        fixture
    }
    fn spawn(&self, program: &str, args: &[&str], name: &str) -> Process {
        let log = fs::File::create(self.directory.join(format!("{name}.log"))).unwrap();
        Process(
            Command::new(program)
                .args(args)
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        )
    }
    fn start_daemon(&mut self) {
        self.daemon = Some(self.spawn(
            "pipewire",
            &["-c", self.directory.join("pipewire.conf").to_str().unwrap()],
            "daemon",
        ));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.directory.join("prismcast-test").exists() {
            assert!(Instant::now() < deadline, "private daemon did not start");
            thread::sleep(Duration::from_millis(10));
        }
        self.policy = Some(self.spawn("wireplumber", &["-p", "policy"], "policy"));
        thread::sleep(Duration::from_millis(400));
    }
    fn playback(&self, name: &str, amplitude: &str, frequency: &str) -> Process {
        let props = format!("stream-properties=props,node.name=(string){name},media.class=(string)Stream/Output/Audio,media.type=(string)Audio,media.category=(string)Playback,node.dont-fallback=(boolean)false");
        // Playback has one private null sink. Capture branches independently use
        // strict dont-fallback=true and exact serial+daemon connection identity.
        self.spawn(
            "gst-launch-1.0",
            &[
                "-q",
                "audiotestsrc",
                "is-live=true",
                &format!("volume={amplitude}"),
                &format!("freq={frequency}"),
                "!",
                "audio/x-raw,format=F32LE,rate=48000,channels=2",
                "!",
                "pipewiresink",
                "target-object=prismcast-test-sink",
                "async=false",
                "sync=false",
                "use-bufferpool=false",
                &props,
            ],
            name,
        )
    }
    pub fn stop_application(&mut self) {
        self.application.take();
    }
    pub fn restart_application(&mut self) {
        self.stop_application();
        self.application = Some(self.playback("prismcast-test-app", "0.25", "440"));
    }
    pub fn restart_daemon(&mut self) {
        self.application.take();
        self.sentinel.take();
        self.policy.take();
        self.daemon.take();
        self.start_daemon();
    }
    pub fn wait_targets(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(targets) = prismcast_capture::audio::discover_audio_targets() {
                if targets
                    .iter()
                    .any(|node| node.node_name == "prismcast-test-app")
                    && targets
                        .iter()
                        .any(|node| node.node_name == "prismcast-test-sentinel")
                {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "private playback streams did not appear; logs in {}",
                self.directory.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn source_settings() -> PipeWireAudioSettings {
        settings("prismcast-test-source", PipeWireAudioMode::Input)
    }
    pub fn output_settings() -> PipeWireAudioSettings {
        settings("prismcast-test-sink", PipeWireAudioMode::Output)
    }
    pub fn application_settings() -> PipeWireAudioSettings {
        settings("prismcast-test-app", PipeWireAudioMode::Application)
    }
}
fn settings(target: &str, mode: PipeWireAudioMode) -> PipeWireAudioSettings {
    PipeWireAudioSettings {
        schema_version: 1,
        target: target.into(),
        mode,
    }
}

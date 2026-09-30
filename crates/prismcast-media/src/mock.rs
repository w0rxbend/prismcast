//! In-memory mock implementations of every backend trait, for unit tests in
//! this crate and for downstream crates' test suites (acceptance criterion of
//! ARCH-004; also the seam used until the `Gst*` backends land).
//!
//! Mocks record every call for assertions, queue [`BackendEvent`]s for
//! draining, and support failure injection via a FIFO of errors consumed by
//! the next fallible call. None of this is suitable for production media
//! work — there is intentionally no real media anywhere in this module.

use std::collections::VecDeque;
use std::sync::Mutex;

use prismcast_core::{
    CanvasId, EncoderId, EncoderSettings, Error, FilterId, OutputId, OutputKind, Result, Scene,
    SceneId, SceneItem, SceneItemId, Service, SourceId, SourceKind, Transition, VideoConfig,
};

use crate::component::{BackendComponent, BackendEvent, ComponentState};
use crate::compositor::CompositorBackend;
use crate::encoder::{EncoderBackend, EncoderCapability, EncoderRegistry};
use crate::filter::{AudioFilterBackend, FilterBackend, FilterDescriptor, VideoFilterBackend};
use crate::output::{OutputBackend, OutputCapabilities, OutputStats};
use crate::service::{IngestEndpoint, ServiceProbe, StreamingServiceBackend};
use crate::source::{AudioLevels, SourceBackend};

/// Shared bookkeeping for mocks of [`BackendComponent`] subtraits: state,
/// pending events, and a FIFO of injected failures.
#[derive(Debug, Default)]
pub struct MockComponentCore {
    state: ComponentState,
    events: Vec<BackendEvent>,
    failures: VecDeque<Error>,
}

impl MockComponentCore {
    /// Sets the reported state without emitting an event.
    pub fn set_state(&mut self, state: ComponentState) {
        self.state = state;
    }

    /// Queues an event to be returned by the next `drain_events`.
    pub fn push_event(&mut self, event: BackendEvent) {
        self.events.push(event);
    }

    /// Queues a failure returned by the next fallible call.
    pub fn inject_failure(&mut self, error: Error) {
        self.failures.push_back(error);
    }

    /// Pops the next injected failure, if any.
    pub fn take_failure(&mut self) -> Result<()> {
        match self.failures.pop_front() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl BackendComponent for MockComponentCore {
    fn state(&self) -> ComponentState {
        self.state
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        std::mem::take(&mut self.events)
    }
}

/// In-memory [`SourceBackend`].
#[derive(Debug)]
pub struct MockSourceBackend {
    /// Shared state/events/failure injection.
    pub core: MockComponentCore,
    id: SourceId,
    kind: SourceKind,
    schema: serde_json::Value,
    levels: Option<AudioLevels>,
    /// Recorded `start()` calls.
    pub starts: u32,
    /// Recorded `stop()` calls.
    pub stops: u32,
    /// Recorded settings payloads, in order.
    pub settings_updates: Vec<serde_json::Value>,
}

impl MockSourceBackend {
    /// Creates a stopped mock for the given source identity.
    pub fn new(id: SourceId, kind: SourceKind) -> Self {
        Self {
            core: MockComponentCore::default(),
            id,
            kind,
            schema: serde_json::Value::Null,
            levels: None,
            starts: 0,
            stops: 0,
            settings_updates: Vec::new(),
        }
    }

    /// Sets the schema returned by `settings_schema()`.
    pub fn with_schema(mut self, schema: serde_json::Value) -> Self {
        self.schema = schema;
        self
    }

    /// Sets the levels returned by `audio_levels()`.
    pub fn with_levels(mut self, levels: AudioLevels) -> Self {
        self.levels = Some(levels);
        self
    }
}

impl BackendComponent for MockSourceBackend {
    fn state(&self) -> ComponentState {
        self.core.state()
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        self.core.drain_events()
    }
}

impl SourceBackend for MockSourceBackend {
    fn source_id(&self) -> SourceId {
        self.id
    }

    fn kind(&self) -> SourceKind {
        self.kind
    }

    fn settings_schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    fn start(&mut self) -> Result<()> {
        self.core.take_failure()?;
        self.starts += 1;
        self.core.set_state(ComponentState::Running);
        self.core.push_event(BackendEvent::StateChanged {
            state: ComponentState::Running,
        });
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.core.take_failure()?;
        self.stops += 1;
        self.core.set_state(ComponentState::Stopped);
        self.core.push_event(BackendEvent::StateChanged {
            state: ComponentState::Stopped,
        });
        Ok(())
    }

    fn update_settings(&mut self, settings: serde_json::Value) -> Result<()> {
        self.core.take_failure()?;
        self.settings_updates.push(settings);
        Ok(())
    }

    fn audio_levels(&self) -> Option<AudioLevels> {
        self.levels.clone()
    }
}

/// In-memory [`FilterBackend`] shared by the video/audio mock wrappers.
#[derive(Debug)]
pub struct MockFilterBackend {
    /// Shared state/events/failure injection.
    pub core: MockComponentCore,
    id: FilterId,
    descriptor: FilterDescriptor,
    schema: serde_json::Value,
    /// Current enabled flag.
    pub enabled: bool,
    /// Recorded settings payloads, in order.
    pub settings_updates: Vec<serde_json::Value>,
}

impl MockFilterBackend {
    /// Creates an enabled mock filter with the given descriptor.
    pub fn new(id: FilterId, descriptor: FilterDescriptor) -> Self {
        Self {
            core: MockComponentCore::default(),
            id,
            descriptor,
            schema: serde_json::Value::Null,
            enabled: true,
            settings_updates: Vec::new(),
        }
    }
}

impl BackendComponent for MockFilterBackend {
    fn state(&self) -> ComponentState {
        self.core.state()
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        self.core.drain_events()
    }
}

impl FilterBackend for MockFilterBackend {
    fn filter_id(&self) -> FilterId {
        self.id
    }

    fn descriptor(&self) -> FilterDescriptor {
        self.descriptor.clone()
    }

    fn settings_schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    fn update_settings(&mut self, settings: serde_json::Value) -> Result<()> {
        self.core.take_failure()?;
        self.settings_updates.push(settings);
        Ok(())
    }

    fn set_enabled(&mut self, enabled: bool) -> Result<()> {
        self.core.take_failure()?;
        self.enabled = enabled;
        Ok(())
    }
}

/// In-memory [`VideoFilterBackend`].
#[derive(Debug)]
pub struct MockVideoFilterBackend(pub MockFilterBackend);

impl BackendComponent for MockVideoFilterBackend {
    fn state(&self) -> ComponentState {
        self.0.state()
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        self.0.drain_events()
    }
}

impl FilterBackend for MockVideoFilterBackend {
    fn filter_id(&self) -> FilterId {
        self.0.filter_id()
    }

    fn descriptor(&self) -> FilterDescriptor {
        self.0.descriptor()
    }

    fn settings_schema(&self) -> serde_json::Value {
        self.0.settings_schema()
    }

    fn update_settings(&mut self, settings: serde_json::Value) -> Result<()> {
        self.0.update_settings(settings)
    }

    fn set_enabled(&mut self, enabled: bool) -> Result<()> {
        self.0.set_enabled(enabled)
    }
}

impl VideoFilterBackend for MockVideoFilterBackend {}

/// In-memory [`AudioFilterBackend`].
#[derive(Debug)]
pub struct MockAudioFilterBackend(pub MockFilterBackend);

impl BackendComponent for MockAudioFilterBackend {
    fn state(&self) -> ComponentState {
        self.0.state()
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        self.0.drain_events()
    }
}

impl FilterBackend for MockAudioFilterBackend {
    fn filter_id(&self) -> FilterId {
        self.0.filter_id()
    }

    fn descriptor(&self) -> FilterDescriptor {
        self.0.descriptor()
    }

    fn settings_schema(&self) -> serde_json::Value {
        self.0.settings_schema()
    }

    fn update_settings(&mut self, settings: serde_json::Value) -> Result<()> {
        self.0.update_settings(settings)
    }

    fn set_enabled(&mut self, enabled: bool) -> Result<()> {
        self.0.set_enabled(enabled)
    }
}

impl AudioFilterBackend for MockAudioFilterBackend {}

/// In-memory [`CompositorBackend`]; records calls instead of compositing.
#[derive(Debug, Default)]
pub struct MockCompositorBackend {
    /// Shared state/events/failure injection.
    pub core: MockComponentCore,
    /// Recorded canvas configurations, in order.
    pub canvas_configs: Vec<(CanvasId, VideoConfig)>,
    /// Recorded full scene syncs, in order.
    pub synced_scenes: Vec<SceneId>,
    /// Recorded item upserts, in order.
    pub upserted_items: Vec<(SceneId, SceneItem)>,
    /// Recorded item removals, in order.
    pub removed_items: Vec<(SceneId, SceneItemId)>,
    /// Currently selected program scene.
    pub program_scene: Option<SceneId>,
    /// Recorded transitions, in order.
    pub transitions: Vec<Transition>,
}

impl BackendComponent for MockCompositorBackend {
    fn state(&self) -> ComponentState {
        self.core.state()
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        self.core.drain_events()
    }
}

impl CompositorBackend for MockCompositorBackend {
    fn configure_canvas(&mut self, canvas_id: CanvasId, video: VideoConfig) -> Result<()> {
        self.core.take_failure()?;
        self.canvas_configs.push((canvas_id, video));
        Ok(())
    }

    fn sync_scene(&mut self, scene: &Scene) -> Result<()> {
        self.core.take_failure()?;
        self.synced_scenes.push(scene.id);
        Ok(())
    }

    fn upsert_item(&mut self, scene_id: SceneId, item: &SceneItem) -> Result<()> {
        self.core.take_failure()?;
        self.upserted_items.push((scene_id, item.clone()));
        Ok(())
    }

    fn remove_item(&mut self, scene_id: SceneId, item_id: SceneItemId) -> Result<()> {
        self.core.take_failure()?;
        self.removed_items.push((scene_id, item_id));
        Ok(())
    }

    fn set_program_scene(&mut self, scene_id: SceneId) -> Result<()> {
        self.core.take_failure()?;
        self.program_scene = Some(scene_id);
        Ok(())
    }

    fn start_transition(&mut self, transition: &Transition) -> Result<()> {
        self.core.take_failure()?;
        self.transitions.push(transition.clone());
        Ok(())
    }
}

/// In-memory [`EncoderBackend`].
#[derive(Debug)]
pub struct MockEncoderBackend {
    /// Shared state/events/failure injection.
    pub core: MockComponentCore,
    settings: EncoderSettings,
    /// Whether `force_keyframe()` succeeds (mirrors
    /// [`EncoderCapability::supports_force_keyframe`]).
    pub force_keyframe_supported: bool,
    /// Recorded live settings updates, in order.
    pub settings_updates: Vec<EncoderSettings>,
    /// Recorded force-keyframe requests.
    pub force_keyframes: u32,
}

impl MockEncoderBackend {
    /// Creates a stopped mock for the given settings.
    pub fn new(settings: EncoderSettings) -> Self {
        Self {
            core: MockComponentCore::default(),
            settings,
            force_keyframe_supported: true,
            settings_updates: Vec::new(),
            force_keyframes: 0,
        }
    }
}

impl BackendComponent for MockEncoderBackend {
    fn state(&self) -> ComponentState {
        self.core.state()
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        self.core.drain_events()
    }
}

impl EncoderBackend for MockEncoderBackend {
    fn encoder_id(&self) -> EncoderId {
        self.settings.id
    }

    fn settings(&self) -> &EncoderSettings {
        &self.settings
    }

    fn update_settings(&mut self, settings: &EncoderSettings) -> Result<()> {
        self.core.take_failure()?;
        self.settings_updates.push(settings.clone());
        Ok(())
    }

    fn force_keyframe(&mut self) -> Result<()> {
        self.core.take_failure()?;
        if self.force_keyframe_supported {
            self.force_keyframes += 1;
            Ok(())
        } else {
            Err(Error::InvalidInput(
                "encoder does not support force-keyframe".to_string(),
            ))
        }
    }
}

/// In-memory [`EncoderRegistry`] with a fixed capability list.
#[derive(Debug, Default)]
pub struct MockEncoderRegistry {
    /// Capabilities returned by `probe()`.
    pub capabilities: Vec<EncoderCapability>,
    /// Settings of every created encoder, in order (`Mutex` because `create`
    /// takes `&self`).
    pub created: Mutex<Vec<EncoderSettings>>,
}

impl MockEncoderRegistry {
    /// Creates a registry reporting the given capabilities.
    pub fn with_capabilities(capabilities: Vec<EncoderCapability>) -> Self {
        Self {
            capabilities,
            created: Mutex::new(Vec::new()),
        }
    }
}

impl EncoderRegistry for MockEncoderRegistry {
    fn probe(&self) -> Vec<EncoderCapability> {
        self.capabilities.clone()
    }

    fn create(&self, settings: EncoderSettings) -> Result<Box<dyn EncoderBackend>> {
        if self.capabilities.iter().any(|c| c.codec == settings.codec) {
            let mut created = self
                .created
                .lock()
                .map_err(|e| Error::Media(format!("mock registry lock poisoned: {e}")))?;
            created.push(settings.clone());
            Ok(Box::new(MockEncoderBackend::new(settings)))
        } else {
            Err(Error::Media(format!(
                "no encoder implementation for codec {:?}",
                settings.codec
            )))
        }
    }
}

/// In-memory [`OutputBackend`]; records calls and returns canned stats.
#[derive(Debug)]
pub struct MockOutputBackend {
    /// Shared state/events/failure injection.
    pub core: MockComponentCore,
    id: OutputId,
    kind: OutputKind,
    /// Capabilities reported by `capabilities()`.
    pub caps: OutputCapabilities,
    /// Stats returned by `statistics()`.
    pub stats: OutputStats,
    /// Recorded lifecycle calls.
    pub starts: u32,
    /// Recorded `stop()` calls.
    pub stops: u32,
    /// Whether currently paused.
    pub paused: bool,
    /// Recorded `split()` calls.
    pub splits: u32,
}

impl MockOutputBackend {
    /// Creates a stopped mock for the given output identity.
    pub fn new(id: OutputId, kind: OutputKind) -> Self {
        Self {
            core: MockComponentCore::default(),
            id,
            kind,
            caps: OutputCapabilities::default(),
            stats: OutputStats::default(),
            starts: 0,
            stops: 0,
            paused: false,
            splits: 0,
        }
    }

    /// Sets the reported capabilities.
    pub fn with_capabilities(mut self, caps: OutputCapabilities) -> Self {
        self.caps = caps;
        self
    }

    fn require(&self, supported: bool, what: &str) -> Result<()> {
        if supported {
            Ok(())
        } else {
            Err(Error::InvalidInput(format!(
                "{what} not supported by {:?} output",
                self.kind
            )))
        }
    }
}

impl BackendComponent for MockOutputBackend {
    fn state(&self) -> ComponentState {
        self.core.state()
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        self.core.drain_events()
    }
}

impl OutputBackend for MockOutputBackend {
    fn output_id(&self) -> OutputId {
        self.id
    }

    fn kind(&self) -> OutputKind {
        self.kind
    }

    fn capabilities(&self) -> OutputCapabilities {
        self.caps
    }

    fn start(&mut self) -> Result<()> {
        self.core.take_failure()?;
        self.starts += 1;
        self.core.set_state(ComponentState::Running);
        self.core.push_event(BackendEvent::StateChanged {
            state: ComponentState::Running,
        });
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.core.take_failure()?;
        self.stops += 1;
        self.paused = false;
        self.core.set_state(ComponentState::Stopped);
        self.core.push_event(BackendEvent::StateChanged {
            state: ComponentState::Stopped,
        });
        Ok(())
    }

    fn pause(&mut self) -> Result<()> {
        self.require(self.caps.pause_resume, "pause")?;
        self.core.take_failure()?;
        self.paused = true;
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        self.require(self.caps.pause_resume, "resume")?;
        self.core.take_failure()?;
        self.paused = false;
        Ok(())
    }

    fn split(&mut self) -> Result<()> {
        self.require(self.caps.manual_split, "split")?;
        self.core.take_failure()?;
        self.splits += 1;
        Ok(())
    }

    fn statistics(&self) -> OutputStats {
        self.stats.clone()
    }
}

/// In-memory [`StreamingServiceBackend`] with canned responses.
#[derive(Debug)]
pub struct MockStreamingServiceBackend {
    protocol: OutputKind,
    /// Error returned by `validate()`, if any.
    pub validate_error: Option<Error>,
    /// Endpoints returned by `endpoints()`.
    pub canned_endpoints: Vec<IngestEndpoint>,
    /// Probe returned by `probe()`.
    pub canned_probe: ServiceProbe,
    /// IDs of validated services, in order (`Mutex` because the trait takes
    /// `&self`).
    pub validated: Mutex<Vec<prismcast_core::ServiceId>>,
}

impl MockStreamingServiceBackend {
    /// Creates a mock for the given protocol family.
    pub fn new(protocol: OutputKind) -> Self {
        Self {
            protocol,
            validate_error: None,
            canned_endpoints: Vec::new(),
            canned_probe: ServiceProbe::default(),
            validated: Mutex::new(Vec::new()),
        }
    }
}

impl StreamingServiceBackend for MockStreamingServiceBackend {
    fn protocol(&self) -> OutputKind {
        self.protocol
    }

    fn validate(&self, service: &Service) -> Result<()> {
        let mut validated = self
            .validated
            .lock()
            .map_err(|e| Error::Media(format!("mock service lock poisoned: {e}")))?;
        validated.push(service.id);
        match &self.validate_error {
            Some(error) => Err(Error::InvalidInput(error.to_string())),
            None => Ok(()),
        }
    }

    fn endpoints(&self, _service: &Service) -> Result<Vec<IngestEndpoint>> {
        Ok(self.canned_endpoints.clone())
    }

    fn probe(&self, _service: &Service) -> Result<ServiceProbe> {
        Ok(self.canned_probe.clone())
    }
}

//! The output graph: N independent outputs + the computed encoder plan.
//!
//! [`OutputGraph`] is the domain-level runtime model of PLAN.md §10–11 and
//! ADR-0007. It holds:
//!
//! - a registry of **declared encoders** (`EncoderId` → [`EncoderSpec`]),
//! - one independent [`OutputRuntime`] per output (state machine, reconnect
//!   policy, statistics — no shared mutable state between outputs),
//! - the current [`EncoderPlan`], recomputed from scratch whenever the output
//!   set, encoder registrations, or program video configuration change.
//!
//! The graph deliberately contains **no GStreamer types**: it is the pure
//! planning/state layer the media engine (`prismcast-media`) executes. The
//! media engine instantiates one physical encoder per
//! [`EncoderGroup::instance`] and a packet tee with one branch per consumer;
//! this crate decides *what* to build, not *how*.

use indexmap::IndexMap;

use prismcast_core::{
    EncoderId, EncoderSettings, Output, OutputId, OutputState, ReconnectPolicy, VideoConfig,
};

use crate::error::{OutputGraphError, Result};
use crate::plan::{EncoderPlan, PlanEntry};
use crate::runtime::{OutputRuntime, ReconnectStep};
use crate::spec::EncoderSpec;

/// Domain-level output graph (PLAN.md §10–11, ADR-0007).
#[derive(Debug)]
pub struct OutputGraph {
    video: VideoConfig,
    encoders: IndexMap<EncoderId, EncoderSpec>,
    bindings: IndexMap<OutputId, Vec<EncoderId>>,
    runtimes: IndexMap<OutputId, OutputRuntime>,
    plan: EncoderPlan,
}

impl OutputGraph {
    /// Creates an empty graph for the given program video configuration.
    pub fn new(video: VideoConfig) -> Self {
        Self {
            video,
            encoders: IndexMap::new(),
            bindings: IndexMap::new(),
            runtimes: IndexMap::new(),
            plan: EncoderPlan::default(),
        }
    }

    /// The program video configuration specs are derived from.
    pub fn video_config(&self) -> &VideoConfig {
        &self.video
    }

    /// The current encoder sharing plan.
    pub fn plan(&self) -> &EncoderPlan {
        &self.plan
    }

    /// The runtime of one output, if present.
    pub fn runtime(&self, output_id: OutputId) -> Option<&OutputRuntime> {
        self.runtimes.get(&output_id)
    }

    /// All outputs currently in the graph, in insertion order.
    pub fn outputs(&self) -> impl Iterator<Item = OutputId> + '_ {
        self.runtimes.keys().copied()
    }

    /// Updates the program video configuration and re-plans: resolution/FPS
    /// are part of the share-identity, so a canvas change can split or merge
    /// encoder groups.
    pub fn set_video_config(&mut self, video: VideoConfig) {
        self.video = video;
        self.replan();
    }

    /// Registers (or re-registers) a video encoder's settings, deriving its
    /// share-identity against the current video configuration.
    ///
    /// Re-registering an existing ID with new settings is how settings changes
    /// enter the graph; the plan is recomputed immediately (ADR-0007).
    pub fn register_video_encoder(
        &mut self,
        settings: &EncoderSettings,
        color_format: Option<&str>,
    ) {
        let spec = EncoderSpec::from_video(settings, &self.video, color_format);
        self.encoders.insert(settings.id, spec);
        self.replan();
    }

    /// Registers (or re-registers) an audio encoder's settings.
    pub fn register_audio_encoder(&mut self, settings: &EncoderSettings) {
        let spec = EncoderSpec::from_audio(settings);
        self.encoders.insert(settings.id, spec);
        self.replan();
    }

    /// Adds an output to the graph with a fresh (`Stopped`) runtime.
    ///
    /// # Errors
    ///
    /// - [`OutputGraphError::DuplicateOutput`] if the output ID is already in
    ///   the graph.
    /// - [`OutputGraphError::UnknownEncoder`] if the output references an
    ///   encoder whose settings were never registered.
    /// - [`OutputGraphError::EncoderKindMismatch`] if a video/audio encoder is
    ///   referenced in the wrong slot.
    pub fn add_output(&mut self, output: &Output) -> Result<()> {
        if self.runtimes.contains_key(&output.id) {
            return Err(OutputGraphError::DuplicateOutput(output.id));
        }

        let mut declared = Vec::with_capacity(1 + output.audio_encoders.len());
        let video_spec = self
            .encoders
            .get(&output.video_encoder)
            .ok_or(OutputGraphError::UnknownEncoder(output.video_encoder))?;
        if !video_spec.is_video() {
            return Err(OutputGraphError::EncoderKindMismatch(output.video_encoder));
        }
        declared.push(output.video_encoder);

        for &audio_encoder in &output.audio_encoders {
            let spec = self
                .encoders
                .get(&audio_encoder)
                .ok_or(OutputGraphError::UnknownEncoder(audio_encoder))?;
            if spec.is_video() {
                return Err(OutputGraphError::EncoderKindMismatch(audio_encoder));
            }
            declared.push(audio_encoder);
        }

        tracing::debug!(output_id = %output.id, name = %output.name, "output added to graph");
        self.runtimes.insert(
            output.id,
            OutputRuntime::restore(output.id, output.reconnect_policy, output.state),
        );
        self.bindings.insert(output.id, declared);
        self.replan();
        Ok(())
    }

    /// Removes an output from the graph and re-plans.
    ///
    /// # Errors
    ///
    /// - [`OutputGraphError::UnknownOutput`] if the output is not in the graph.
    /// - [`OutputGraphError::OutputNotStopped`] unless the output is `Stopped`
    ///   or `Failed` (mirrors the domain rule in `prismcast-core`).
    pub fn remove_output(&mut self, output_id: OutputId) -> Result<()> {
        let runtime = self
            .runtimes
            .get(&output_id)
            .ok_or(OutputGraphError::UnknownOutput(output_id))?;
        if !matches!(runtime.state(), OutputState::Stopped | OutputState::Failed) {
            return Err(OutputGraphError::OutputNotStopped {
                output_id,
                state: runtime.state(),
            });
        }

        tracing::debug!(output_id = %output_id, "output removed from graph");
        self.runtimes.shift_remove(&output_id);
        self.bindings.shift_remove(&output_id);
        self.replan();
        Ok(())
    }

    /// Applies a validated lifecycle transition to one output.
    ///
    /// Transitions are strictly per-output: there is no graph-level state and
    /// no broadcast, so a transition (including into `Failed`) cannot touch
    /// sibling outputs (PLAN.md §11, §50).
    ///
    /// # Errors
    ///
    /// [`OutputGraphError::UnknownOutput`] or
    /// [`OutputGraphError::IllegalTransition`].
    pub fn transition(&mut self, output_id: OutputId, to: OutputState) -> Result<()> {
        self.runtime_mut(output_id)?.transition(to)
    }

    /// Records a connection loss on one output and returns the reconnect
    /// decision (see [`OutputRuntime::connection_lost`]).
    ///
    /// # Errors
    ///
    /// [`OutputGraphError::UnknownOutput`] or
    /// [`OutputGraphError::NotReconnectable`].
    pub fn connection_lost(
        &mut self,
        output_id: OutputId,
        reason: impl Into<String>,
    ) -> Result<ReconnectStep> {
        self.runtime_mut(output_id)?.connection_lost(reason)
    }

    /// Replaces one output's reconnect policy.
    ///
    /// # Errors
    ///
    /// [`OutputGraphError::UnknownOutput`] if the output is not in the graph.
    pub fn set_reconnect_policy(
        &mut self,
        output_id: OutputId,
        policy: ReconnectPolicy,
    ) -> Result<()> {
        self.runtime_mut(output_id)?.set_policy(policy);
        Ok(())
    }

    /// Mutable statistics handle for the media layer.
    ///
    /// # Errors
    ///
    /// [`OutputGraphError::UnknownOutput`] if the output is not in the graph.
    pub fn stats_mut(&mut self, output_id: OutputId) -> Result<&mut crate::runtime::OutputStats> {
        Ok(self.runtime_mut(output_id)?.stats_mut())
    }

    fn runtime_mut(&mut self, output_id: OutputId) -> Result<&mut OutputRuntime> {
        self.runtimes
            .get_mut(&output_id)
            .ok_or(OutputGraphError::UnknownOutput(output_id))
    }

    fn replan(&mut self) {
        let mut entries = Vec::new();
        for (output_id, declared) in &self.bindings {
            for encoder_id in declared {
                // Every bound encoder was validated against the registry in
                // `add_output`; registrations are never removed, so this is
                // unreachable in practice — skip defensively rather than panic.
                let Some(spec) = self.encoders.get(encoder_id) else {
                    continue;
                };
                entries.push(PlanEntry {
                    output_id: *output_id,
                    declared: *encoder_id,
                    spec: spec.clone(),
                });
            }
        }
        self.plan = EncoderPlan::compute(&entries);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::runtime::ReconnectStep;
    use prismcast_core::OutputKind;

    fn settings(codec: &str, bitrate_kbps: u32) -> EncoderSettings {
        EncoderSettings {
            id: EncoderId::new(),
            codec: codec.to_string(),
            bitrate_kbps,
            keyframe_interval: Some(120),
            settings: serde_json::Value::Null,
        }
    }

    /// Graph with Twitch + YouTube outputs sharing identical encoder settings
    /// (independently registered), plus a dedicated recording output.
    fn multistream_graph() -> (OutputGraph, Output, Output, Output) {
        let mut graph = OutputGraph::new(VideoConfig::default());

        let twitch_enc = settings("h264", 6_000);
        let mut youtube_enc = twitch_enc.clone();
        youtube_enc.id = EncoderId::new();
        let rec_enc = settings("h265", 20_000);
        graph.register_video_encoder(&twitch_enc, Some("nv12"));
        graph.register_video_encoder(&youtube_enc, Some("nv12"));
        graph.register_video_encoder(&rec_enc, Some("nv12"));

        let twitch = Output::new(OutputKind::Rtmp, "Twitch", twitch_enc.id);
        let youtube = Output::new(OutputKind::Rtmp, "YouTube", youtube_enc.id);
        let recording = Output::new(OutputKind::Recording, "Recording", rec_enc.id);
        graph.add_output(&twitch).unwrap();
        graph.add_output(&youtube).unwrap();
        graph.add_output(&recording).unwrap();

        (graph, twitch, youtube, recording)
    }

    #[test]
    fn graph_computes_sharing_plan_from_output_set() {
        let (graph, twitch, youtube, recording) = multistream_graph();
        let plan = graph.plan();

        assert_eq!(
            plan.instance_count(),
            2,
            "twitch+youtube share, recording dedicated"
        );
        assert!(plan.is_shared(twitch.video_encoder));
        assert!(plan.is_shared(youtube.video_encoder));
        assert!(!plan.is_shared(recording.video_encoder));
        assert_eq!(
            plan.instance_for(twitch.video_encoder),
            plan.instance_for(youtube.video_encoder)
        );
    }

    #[test]
    fn duplicate_output_is_rejected() {
        let (mut graph, twitch, _, _) = multistream_graph();
        let err = graph.add_output(&twitch).unwrap_err();
        assert_eq!(err, OutputGraphError::DuplicateOutput(twitch.id));
    }

    #[test]
    fn unregistered_encoder_is_rejected() {
        let mut graph = OutputGraph::new(VideoConfig::default());
        let output = Output::new(OutputKind::Rtmp, "orphan", EncoderId::new());
        let err = graph.add_output(&output).unwrap_err();
        assert!(matches!(err, OutputGraphError::UnknownEncoder(_)));
    }

    #[test]
    fn audio_encoder_in_video_slot_is_rejected() {
        let mut graph = OutputGraph::new(VideoConfig::default());
        let enc = settings("aac", 160);
        graph.register_audio_encoder(&enc);
        let output = Output::new(OutputKind::Rtmp, "bad", enc.id);
        let err = graph.add_output(&output).unwrap_err();
        assert!(matches!(err, OutputGraphError::EncoderKindMismatch(_)));
    }

    #[test]
    fn one_output_failing_never_touches_siblings() {
        let (mut graph, twitch, youtube, recording) = multistream_graph();
        for id in [twitch.id, youtube.id, recording.id] {
            graph.transition(id, OutputState::Starting).unwrap();
            graph.transition(id, OutputState::Running).unwrap();
        }

        // Kill Twitch dead: no retries allowed, straight to Failed.
        graph
            .set_reconnect_policy(
                twitch.id,
                ReconnectPolicy {
                    max_retries: 0,
                    ..ReconnectPolicy::default()
                },
            )
            .unwrap();
        assert_eq!(
            graph.connection_lost(twitch.id, "auth rejected").unwrap(),
            ReconnectStep::Exhausted
        );

        assert_eq!(
            graph.runtime(twitch.id).unwrap().state(),
            OutputState::Failed
        );
        assert_eq!(
            graph.runtime(youtube.id).unwrap().state(),
            OutputState::Running
        );
        assert_eq!(
            graph.runtime(recording.id).unwrap().state(),
            OutputState::Running
        );
        // The shared encoder keeps feeding YouTube: plan still has 2 instances
        // and both consumers of the shared group.
        assert_eq!(graph.plan().instance_count(), 2);
        assert_eq!(
            graph.plan().group_for_output(youtube.id),
            graph.plan().group_for_output(twitch.id),
            "plan is a pure function of the output set; failure does not re-plan"
        );
    }

    #[test]
    fn reconnecting_output_recovers_independently() {
        let (mut graph, twitch, youtube, _) = multistream_graph();
        for id in [twitch.id, youtube.id] {
            graph.transition(id, OutputState::Starting).unwrap();
            graph.transition(id, OutputState::Running).unwrap();
        }

        let step = graph.connection_lost(twitch.id, "tcp reset").unwrap();
        assert_eq!(
            step,
            ReconnectStep::Retry {
                attempt: 1,
                backoff: Duration::from_millis(1_000)
            }
        );
        assert_eq!(
            graph.runtime(twitch.id).unwrap().state(),
            OutputState::Reconnecting { attempt: 1 }
        );
        assert_eq!(
            graph.runtime(youtube.id).unwrap().state(),
            OutputState::Running
        );

        graph.transition(twitch.id, OutputState::Running).unwrap();
        assert_eq!(
            graph.runtime(twitch.id).unwrap().state(),
            OutputState::Running
        );
    }

    #[test]
    fn removing_a_shared_consumer_splits_the_group() {
        let (mut graph, twitch, youtube, _) = multistream_graph();
        assert_eq!(graph.plan().instance_count(), 2);

        graph.remove_output(twitch.id).unwrap();

        assert_eq!(graph.plan().instance_count(), 2);
        assert!(
            !graph.plan().is_shared(youtube.video_encoder),
            "sole remaining consumer becomes a dedicated branch"
        );
        assert!(graph.plan().group_for_output(twitch.id).is_none());
    }

    #[test]
    fn running_output_cannot_be_removed() {
        let (mut graph, twitch, _, _) = multistream_graph();
        graph.transition(twitch.id, OutputState::Starting).unwrap();
        graph.transition(twitch.id, OutputState::Running).unwrap();
        let err = graph.remove_output(twitch.id).unwrap_err();
        assert!(matches!(err, OutputGraphError::OutputNotStopped { .. }));
        assert!(graph.runtime(twitch.id).is_some(), "still in the graph");
    }

    #[test]
    fn settings_change_replans() {
        let (mut graph, twitch, youtube, _) = multistream_graph();
        assert_eq!(graph.plan().instance_count(), 2);

        // YouTube reconfigured to a lower bitrate: sharing no longer legal.
        let mut new_youtube = settings("h264", 4_500);
        new_youtube.id = youtube.video_encoder;
        graph.register_video_encoder(&new_youtube, Some("nv12"));

        assert_eq!(graph.plan().instance_count(), 3);
        assert!(!graph.plan().is_shared(twitch.video_encoder));
        assert!(!graph.plan().is_shared(youtube.video_encoder));
    }

    #[test]
    fn video_config_change_replans() {
        let (mut graph, twitch, youtube, _) = multistream_graph();
        assert_eq!(graph.plan().instance_count(), 2);

        // Registered specs were derived from the old config; re-derive for the
        // new canvas the way the application layer would.
        graph.set_video_config(VideoConfig {
            width: 1280,
            height: 720,
            fps_num: 30,
            fps_den: 1,
        });
        let enc = settings("h264", 6_000);
        let mut twitch_enc = enc.clone();
        twitch_enc.id = twitch.video_encoder;
        let mut youtube_enc = enc;
        youtube_enc.id = youtube.video_encoder;
        graph.register_video_encoder(&twitch_enc, Some("nv12"));
        graph.register_video_encoder(&youtube_enc, Some("nv12"));

        assert_eq!(
            graph.plan().instance_count(),
            2,
            "twitch/youtube still share at the new resolution"
        );
    }

    #[test]
    fn unknown_output_operations_are_errors() {
        let (mut graph, _, _, _) = multistream_graph();
        let unknown = OutputId::new();
        assert_eq!(
            graph
                .transition(unknown, OutputState::Starting)
                .unwrap_err(),
            OutputGraphError::UnknownOutput(unknown)
        );
        assert_eq!(
            graph.remove_output(unknown).unwrap_err(),
            OutputGraphError::UnknownOutput(unknown)
        );
        assert!(matches!(
            graph.connection_lost(unknown, "x").unwrap_err(),
            OutputGraphError::UnknownOutput(_)
        ));
    }

    #[test]
    fn stats_are_per_output() {
        let (mut graph, twitch, youtube, _) = multistream_graph();
        graph.stats_mut(twitch.id).unwrap().bytes_sent = 1_000;
        assert_eq!(graph.runtime(youtube.id).unwrap().stats().bytes_sent, 0);
        assert_eq!(graph.runtime(twitch.id).unwrap().stats().bytes_sent, 1_000);
    }
}

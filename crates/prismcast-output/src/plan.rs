//! Encoder sharing plan (PLAN.md §11, ADR-0007).
//!
//! The sharing plan answers one question: for the current set of outputs,
//! which declared encoders fold into the same physical encoder instance, and
//! which outputs consume each instance through the encoded-packet tee?
//!
//! ```text
//! Renderer
//!    ↓
//! Encoder instance            ← one per distinct EncoderSpec
//!    ↓
//! Encoded packet tee
//!    ├── Twitch mux/output    ← consumers
//!    └── YouTube mux/output
//! ```
//!
//! The plan is a pure function of `(output, declared encoder, spec)` triples
//! ([`PlanEntry`]); it is recomputed from scratch whenever the output set,
//! encoder settings, or program video configuration change (ADR-0007: "the
//! mapping is computed from settings equality and re-planned when settings
//! change"). Re-planning from scratch keeps the planner free of incremental
//! bookkeeping bugs; output sets are small (tens, not thousands).
//!
//! Determinism: the representative instance of a group is the smallest member
//! [`EncoderId`], and groups/members/consumers are all sorted by ID, so equal
//! input sets always produce identical plans regardless of insertion order.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use prismcast_core::{EncoderId, OutputId};

use crate::spec::EncoderSpec;

/// One declared encoder of one output, as input to the planner.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanEntry {
    /// Output that declared the encoder.
    pub output_id: OutputId,
    /// The encoder ID as declared on the output.
    pub declared: EncoderId,
    /// Share-identity of the declared encoder.
    pub spec: EncoderSpec,
}

/// A group of declared encoders that share one physical encoder instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderGroup {
    /// The share-identity every member matches.
    pub spec: EncoderSpec,
    /// The physical instance: the smallest member ID (deterministic choice).
    /// In the media graph this is the single encoder feeding the packet tee.
    pub instance: EncoderId,
    /// Declared encoder IDs folded into `instance`, sorted ascending.
    pub members: Vec<EncoderId>,
    /// Outputs consuming this instance through the tee, sorted ascending.
    pub consumers: Vec<OutputId>,
}

impl EncoderGroup {
    /// Whether this group actually shares (`false` = dedicated encoder branch).
    pub fn is_shared(&self) -> bool {
        self.members.len() > 1
    }
}

/// The computed mapping of outputs and declared encoders onto physical
/// encoder instances.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EncoderPlan {
    groups: Vec<EncoderGroup>,
}

impl EncoderPlan {
    /// Computes the sharing plan for a set of plan entries.
    ///
    /// Entries with equal [`EncoderSpec`]s fold into one group; everything
    /// else gets a dedicated group of one. The result is deterministic for an
    /// equal input set (see module docs).
    pub fn compute(entries: &[PlanEntry]) -> Self {
        let mut by_spec: HashMap<&EncoderSpec, Vec<&PlanEntry>> = HashMap::new();
        for entry in entries {
            by_spec.entry(&entry.spec).or_default().push(entry);
        }

        let mut groups: Vec<EncoderGroup> = by_spec
            .into_values()
            .map(|members| {
                let mut member_ids: Vec<EncoderId> =
                    members.iter().map(|entry| entry.declared).collect();
                member_ids.sort_unstable();
                member_ids.dedup();

                let mut consumers: Vec<OutputId> =
                    members.iter().map(|entry| entry.output_id).collect();
                consumers.sort_unstable();
                consumers.dedup();

                // The smallest member ID exists because `members` came from a
                // non-empty HashMap bucket.
                let instance = member_ids[0];
                let spec = members[0].spec.clone();
                EncoderGroup {
                    spec,
                    instance,
                    members: member_ids,
                    consumers,
                }
            })
            .collect();
        groups.sort_unstable_by_key(|group| group.instance);

        Self { groups }
    }

    /// All encoder groups, sorted by instance ID.
    pub fn groups(&self) -> &[EncoderGroup] {
        &self.groups
    }

    /// Number of physical encoder instances the media graph must create.
    pub fn instance_count(&self) -> usize {
        self.groups.len()
    }

    /// The group containing the output's declared encoder, if any.
    pub fn group_for_output(&self, output_id: OutputId) -> Option<&EncoderGroup> {
        self.groups
            .iter()
            .find(|group| group.consumers.contains(&output_id))
    }

    /// The group a declared encoder folds into, if any.
    pub fn group_for_encoder(&self, declared: EncoderId) -> Option<&EncoderGroup> {
        self.groups
            .iter()
            .find(|group| group.members.contains(&declared))
    }

    /// The physical instance a declared encoder resolves to.
    pub fn instance_for(&self, declared: EncoderId) -> Option<EncoderId> {
        self.group_for_encoder(declared).map(|group| group.instance)
    }

    /// Whether a declared encoder shares its instance with at least one other
    /// declared encoder.
    pub fn is_shared(&self, declared: EncoderId) -> bool {
        self.group_for_encoder(declared)
            .is_some_and(EncoderGroup::is_shared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::{EncoderSettings, VideoConfig};

    fn settings(codec: &str, bitrate_kbps: u32) -> EncoderSettings {
        EncoderSettings {
            id: EncoderId::new(),
            codec: codec.to_string(),
            bitrate_kbps,
            keyframe_interval: Some(120),
            settings: serde_json::json!({"preset": "veryfast"}),
        }
    }

    fn video_entry(output_id: OutputId, settings: &EncoderSettings) -> PlanEntry {
        PlanEntry {
            output_id,
            declared: settings.id,
            spec: EncoderSpec::from_video(settings, &VideoConfig::default(), Some("nv12")),
        }
    }

    #[test]
    fn identical_settings_share_one_instance() {
        let (a, b) = (OutputId::new(), OutputId::new());
        // Independently created settings with different IDs but equal values.
        let mut enc_a = settings("h264", 6_000);
        let mut enc_b = enc_a.clone();
        enc_b.id = EncoderId::new();
        enc_a.id = EncoderId::new();

        let plan = EncoderPlan::compute(&[video_entry(a, &enc_a), video_entry(b, &enc_b)]);

        assert_eq!(plan.instance_count(), 1);
        assert!(plan.is_shared(enc_a.id));
        assert!(plan.is_shared(enc_b.id));
        assert_eq!(
            plan.instance_for(enc_a.id),
            plan.instance_for(enc_b.id),
            "both declared encoders resolve to the same instance"
        );
        let group = &plan.groups()[0];
        assert_eq!(group.consumers, {
            let mut v = vec![a, b];
            v.sort_unstable();
            v
        });
    }

    #[test]
    fn differing_bitrate_forces_dedicated_encoders() {
        let (a, b) = (OutputId::new(), OutputId::new());
        let enc_a = settings("h264", 6_000);
        let mut enc_b = enc_a.clone();
        enc_b.id = EncoderId::new();
        enc_b.bitrate_kbps = 8_000;

        let plan = EncoderPlan::compute(&[video_entry(a, &enc_a), video_entry(b, &enc_b)]);

        assert_eq!(plan.instance_count(), 2);
        assert!(!plan.is_shared(enc_a.id));
        assert!(!plan.is_shared(enc_b.id));
        assert_ne!(plan.instance_for(enc_a.id), plan.instance_for(enc_b.id));
    }

    #[test]
    fn differing_resolution_forces_dedicated_encoders() {
        let (a, b) = (OutputId::new(), OutputId::new());
        let enc = settings("h264", 6_000);
        let entry_a = video_entry(a, &enc);
        let mut entry_b = video_entry(b, &enc);
        entry_b.declared = EncoderId::new();
        if let Some(video) = &mut entry_b.spec.video {
            video.width = 1280;
            video.height = 720;
        }

        let plan = EncoderPlan::compute(&[entry_a.clone(), entry_b]);

        assert_eq!(plan.instance_count(), 2);
        assert!(!plan.is_shared(entry_a.declared));
    }

    #[test]
    fn differing_color_format_forces_dedicated_encoders() {
        let (a, b) = (OutputId::new(), OutputId::new());
        let enc = settings("h264", 6_000);
        let video = VideoConfig::default();
        let entries = [
            PlanEntry {
                output_id: a,
                declared: enc.id,
                spec: EncoderSpec::from_video(&enc, &video, Some("nv12")),
            },
            PlanEntry {
                output_id: b,
                declared: EncoderId::new(),
                spec: EncoderSpec::from_video(&enc, &video, Some("p010_10le")),
            },
        ];

        let plan = EncoderPlan::compute(&entries);
        assert_eq!(plan.instance_count(), 2);
    }

    #[test]
    fn mixed_grouping_three_outputs_two_share() {
        let (a, b, c) = (OutputId::new(), OutputId::new(), OutputId::new());
        let enc_shared_a = settings("h264", 6_000);
        let mut enc_shared_b = enc_shared_a.clone();
        enc_shared_b.id = EncoderId::new();
        let enc_dedicated = settings("av1", 4_000);

        let plan = EncoderPlan::compute(&[
            video_entry(a, &enc_shared_a),
            video_entry(b, &enc_shared_b),
            video_entry(c, &enc_dedicated),
        ]);

        assert_eq!(plan.instance_count(), 2);
        assert_eq!(
            plan.group_for_output(a),
            plan.group_for_output(b),
            "a and b share a group"
        );
        assert_ne!(plan.group_for_output(a), plan.group_for_output(c));
    }

    #[test]
    fn audio_and_video_encoders_never_share() {
        let output = OutputId::new();
        let enc = settings("h264", 6_000);
        let video = VideoConfig::default();
        let entries = [
            PlanEntry {
                output_id: output,
                declared: enc.id,
                spec: EncoderSpec::from_video(&enc, &video, None),
            },
            PlanEntry {
                output_id: output,
                declared: EncoderId::new(),
                spec: EncoderSpec::from_audio(&enc),
            },
        ];

        let plan = EncoderPlan::compute(&entries);
        assert_eq!(plan.instance_count(), 2);
    }

    #[test]
    fn plan_is_deterministic_regardless_of_input_order() {
        let outputs: Vec<OutputId> = (0..4).map(|_| OutputId::new()).collect();
        let enc = settings("h264", 6_000);
        let forward: Vec<PlanEntry> = outputs
            .iter()
            .map(|&output_id| video_entry(output_id, &enc))
            .collect();
        let mut reverse = forward.clone();
        reverse.reverse();

        assert_eq!(
            EncoderPlan::compute(&forward),
            EncoderPlan::compute(&reverse)
        );
        // One group; instance is the smallest declared ID.
        let plan = EncoderPlan::compute(&forward);
        let min_id = forward.iter().map(|entry| entry.declared).min().unwrap();
        assert_eq!(plan.groups()[0].instance, min_id);
    }

    #[test]
    fn settings_json_key_order_does_not_affect_sharing() {
        let (a, b) = (OutputId::new(), OutputId::new());
        let mut enc_a = settings("h264", 6_000);
        enc_a.settings = serde_json::json!({"preset": "veryfast", "profile": "high"});
        let mut enc_b = enc_a.clone();
        enc_b.id = EncoderId::new();
        enc_b.settings = serde_json::json!({"profile": "high", "preset": "veryfast"});

        let plan = EncoderPlan::compute(&[video_entry(a, &enc_a), video_entry(b, &enc_b)]);
        assert_eq!(plan.instance_count(), 1);
    }

    #[test]
    fn empty_input_yields_empty_plan() {
        let plan = EncoderPlan::compute(&[]);
        assert_eq!(plan.instance_count(), 0);
        assert!(plan.group_for_output(OutputId::new()).is_none());
    }
}

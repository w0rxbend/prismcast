//! Responses: correlation, status, and typed result payloads.
//!
//! Every [`crate::request::Request`] gets exactly one [`RequestResponse`]
//! echoing the client's `request_id` and the request type tag (the
//! obs-websocket correlation pattern, RES-007 §Message envelope). Failures
//! are structured [`crate::error::WireError`]s, not integer-plus-comment.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::data::{AudioMixerConfig, Output, Profile, Scene, SceneCollection, Source};
use crate::error::WireError;
use crate::subscription::SubscriptionSet;

/// Outcome of a single request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseStatus {
    /// Whether the request succeeded.
    pub ok: bool,
    /// The structured failure; present iff `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
}

impl ResponseStatus {
    /// Success.
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }

    /// Failure from a structured error.
    pub fn error(error: WireError) -> Self {
        Self {
            ok: false,
            error: Some(error),
        }
    }
}

/// Server answer to a [`crate::request::Request`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestResponse {
    /// The client's correlation ID, echoed verbatim.
    pub request_id: String,
    /// The request type tag, echoed for logging/debugging.
    pub request_type: String,
    /// Success/failure and structured error.
    pub status: ResponseStatus,
    /// Typed result payload; present on success when the request yields data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ResponseData>,
}

/// Typed result payloads.
///
/// Command requests mostly answer with the IDs of the entities they
/// created; queries answer with the requested data. The tag (`data` field)
/// makes payload shape self-describing for schema generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "data", rename_all = "snake_case")]
pub enum ResponseData {
    /// The request succeeded and yields no data.
    Empty,
    /// `add_scene` result.
    SceneCreated {
        /// The new scene's server-assigned ID.
        scene_id: Uuid,
    },
    /// `add_scene_item` / `duplicate_scene_item` result.
    SceneItemCreated {
        /// The new item's server-assigned ID.
        item_id: Uuid,
    },
    /// `add_source` result.
    SourceCreated {
        /// The new source's server-assigned ID.
        source_id: Uuid,
    },
    /// `add_audio_bus` result.
    AudioBusCreated {
        /// The new bus's server-assigned ID.
        bus_id: Uuid,
    },
    /// `add_output` result.
    OutputCreated {
        /// The new output's server-assigned ID.
        output_id: Uuid,
    },
    /// `add_profile` result.
    ProfileCreated {
        /// The new profile's server-assigned ID.
        profile_id: Uuid,
    },
    /// `add_scene_collection` result.
    CollectionCreated {
        /// The new collection's server-assigned ID.
        collection_id: Uuid,
    },
    /// `get_version` result: capability discovery.
    Version {
        /// Server software version.
        prismcast_version: String,
        /// Negotiated protocol version of this session.
        protocol_version: u32,
        /// Request type tags available at the negotiated protocol version.
        /// (RES-007 conclusion 2: name-list discovery now; a full
        /// machine-readable schema document is a planned follow-up.)
        available_requests: Vec<String>,
    },
    /// `get_snapshot` result.
    Snapshot {
        /// The full initial state.
        snapshot: Box<crate::data::StateSnapshot>,
    },
    /// `list_scenes` result.
    SceneList {
        /// Scenes in working-set order.
        scenes: Vec<Scene>,
    },
    /// `get_scene` result.
    Scene {
        /// The requested scene.
        scene: Box<Scene>,
    },
    /// `list_sources` result.
    SourceList {
        /// All shared sources.
        sources: Vec<Source>,
    },
    /// `get_source` result.
    Source {
        /// The requested source.
        source: Box<Source>,
    },
    /// `list_outputs` result.
    OutputList {
        /// All outputs with lifecycle state.
        outputs: Vec<Output>,
    },
    /// `get_output` result.
    Output {
        /// The requested output.
        output: Box<Output>,
    },
    /// `get_audio_state` result.
    AudioState {
        /// Buses, routes, and mixer state.
        audio: AudioMixerConfig,
    },
    /// `list_profiles` result.
    ProfileList {
        /// All profiles.
        profiles: Vec<Profile>,
        /// The active profile.
        active: Option<Uuid>,
    },
    /// `list_scene_collections` result.
    CollectionList {
        /// All collections (full payloads; expected to be small).
        collections: Vec<SceneCollection>,
        /// The active collection.
        active: Option<Uuid>,
    },
    /// `update_subscriptions` / `get_subscriptions` result.
    Subscriptions {
        /// The subscription set now in effect.
        subscriptions: SubscriptionSet,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn success_response_roundtrip() {
        let response = RequestResponse {
            request_id: "req-7".into(),
            request_type: "add_scene".into(),
            status: ResponseStatus::ok(),
            data: Some(ResponseData::SceneCreated {
                scene_id: Uuid::new_v4(),
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(response, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn error_response_shape() {
        let response = RequestResponse {
            request_id: "req-8".into(),
            request_type: "remove_scene".into(),
            status: ResponseStatus::error(
                WireError::new(ErrorKind::NotFound, "no such scene").with_field("scene_id"),
            ),
            data: None,
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["status"]["ok"], false);
        assert_eq!(value["status"]["error"]["code"], 600);
        assert_eq!(value["status"]["error"]["kind"], "not_found");
        assert_eq!(value["status"]["error"]["field"], "scene_id");
        assert!(value.get("data").is_none());
        let back: RequestResponse = serde_json::from_value(value).unwrap();
        assert_eq!(response, back);
    }

    #[test]
    fn response_data_roundtrip_all_variants() {
        let id = Uuid::new_v4();
        let variants = vec![
            ResponseData::Empty,
            ResponseData::SceneCreated { scene_id: id },
            ResponseData::SceneItemCreated { item_id: id },
            ResponseData::SourceCreated { source_id: id },
            ResponseData::AudioBusCreated { bus_id: id },
            ResponseData::OutputCreated { output_id: id },
            ResponseData::ProfileCreated { profile_id: id },
            ResponseData::CollectionCreated { collection_id: id },
            ResponseData::Version {
                prismcast_version: "0.1.0".into(),
                protocol_version: 1,
                available_requests: vec!["get_version".into()],
            },
            ResponseData::Snapshot {
                snapshot: Box::default(),
            },
            ResponseData::SceneList { scenes: Vec::new() },
            ResponseData::Scene {
                scene: Box::new(Scene {
                    id,
                    name: "s".into(),
                    items: Vec::new(),
                }),
            },
            ResponseData::SourceList {
                sources: Vec::new(),
            },
            ResponseData::OutputList {
                outputs: Vec::new(),
            },
            ResponseData::AudioState {
                audio: AudioMixerConfig {
                    buses: Vec::new(),
                    routes: Vec::new(),
                    mixer: Vec::new(),
                },
            },
            ResponseData::ProfileList {
                profiles: Vec::new(),
                active: Some(id),
            },
            ResponseData::CollectionList {
                collections: Vec::new(),
                active: None,
            },
            ResponseData::Subscriptions {
                subscriptions: SubscriptionSet::default_all(),
            },
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let back: ResponseData =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("deserialize {json}: {e}"));
            assert_eq!(variant, back, "roundtrip {json}");
        }
    }
}

//! obs-websocket 5.x wire types (RES-007; ADR-0010, ADR-0020).
//!
//! These types are **obs-shaped** and deliberately separate from
//! `prismcast-protocol`: the native wire schema stays untouched and the two
//! protocols evolve independently. Serde shapes match obs-websocket 5.7.4
//! (the version pinned by the OBS 32.2.2 baseline) exactly — `{op, d}`
//! envelopes, camelCase OBS field names (`requestType`, `eventSubscriptions`,
//! ...). The golden fixture tests at the bottom pin the exact field names so
//! a rename here fails loudly instead of breaking real clients at runtime.
//!
//! Both codecs obs-websocket 5.x defines are served: JSON text frames
//! (`obswebsocket.json`, the default) and MessagePack binary frames
//! (`obswebsocket.msgpack`, OBSWS-002; ADR-0021). Both encode the same
//! `{op, d}` envelope shape — MessagePack uses struct-as-map encoding, so
//! the wire types here are shared by both codecs unchanged.

use serde::{Deserialize, Serialize};

/// The obs-websocket version this adapter advertises in `Hello`: 5.7.4, the
/// released baseline pinned by OBS 32.2.2 (RES-007; ADR-0020 §d).
pub const OBS_WEBSOCKET_VERSION: &str = "5.7.4";

/// The only RPC version this adapter speaks (obs-websocket's `rpcVersion`
/// has been 1 since 5.0.0).
pub const RPC_VERSION: u32 = 1;

/// The WebSocket subprotocol tag selecting the JSON codec.
pub const SUBPROTOCOL_JSON: &str = "obswebsocket.json";

/// The WebSocket subprotocol tag selecting the MessagePack codec (binary
/// frames, struct-as-map encoding; ADR-0021).
pub const SUBPROTOCOL_MSGPACK: &str = "obswebsocket.msgpack";

/// Message opcodes (`op` field of the envelope). Op 4 is deliberately
/// unused upstream (4.x-era semantics) and rejected like any unknown opcode.
pub mod op {
    /// Server → client greeting (versions, auth challenge).
    pub const HELLO: u64 = 0;
    /// Client → server session request (auth response, subscriptions).
    pub const IDENTIFY: u64 = 1;
    /// Server → client session confirmation.
    pub const IDENTIFIED: u64 = 2;
    /// Client → server subscription update.
    pub const REIDENTIFY: u64 = 3;
    /// Server → client event.
    pub const EVENT: u64 = 5;
    /// Client → server request.
    pub const REQUEST: u64 = 6;
    /// Server → client request response.
    pub const REQUEST_RESPONSE: u64 = 7;
    /// Client → server request batch.
    pub const REQUEST_BATCH: u64 = 8;
    /// Server → client batch response.
    pub const REQUEST_BATCH_RESPONSE: u64 = 9;
}

/// WebSocket close codes (application-private 4000+ range; RES-007 §Close
/// codes). 4001 is unused upstream. These are the adapter's own constants;
/// the overlapping numeric space of `prismcast_protocol`'s `CloseCode` is
/// coincidental and stays untouched.
pub mod close {
    /// No specific reason.
    pub const UNKNOWN_REASON: u16 = 4000;
    /// A message could not be decoded, or had the wrong shape/kind.
    pub const MESSAGE_DECODE_ERROR: u16 = 4002;
    /// A required field of `d` is missing.
    pub const MISSING_DATA_FIELD: u16 = 4003;
    /// A field of `d` has the wrong type.
    pub const INVALID_DATA_FIELD_TYPE: u16 = 4004;
    /// A field of `d` has an invalid value.
    pub const INVALID_DATA_FIELD_VALUE: u16 = 4005;
    /// The `op` opcode is not known (or missing).
    pub const UNKNOWN_OPCODE: u16 = 4006;
    /// Traffic other than a single `Identify` before identification.
    pub const NOT_IDENTIFIED: u16 = 4007;
    /// `Identify` was sent twice.
    pub const ALREADY_IDENTIFIED: u16 = 4008;
    /// Authentication failed or was missing.
    pub const AUTHENTICATION_FAILED: u16 = 4009;
    /// The requested `rpcVersion` cannot be served.
    pub const UNSUPPORTED_RPC_VERSION: u16 = 4010;
    /// The session was invalidated by the server (kick).
    pub const SESSION_INVALIDATED: u16 = 4011;
    /// The session required a feature this server does not provide.
    pub const UNSUPPORTED_FEATURE: u16 = 4012;
}

/// `RequestStatus` codes, grouped by hundreds (RES-007 §Request status
/// codes).
pub mod status {
    /// The request succeeded.
    pub const SUCCESS: u16 = 100;
    /// The `requestType` field was missing.
    pub const MISSING_REQUEST_TYPE: u16 = 203;
    /// The `requestType` is not known to (or not implemented by) the server.
    pub const UNKNOWN_REQUEST_TYPE: u16 = 204;
    /// Unspecified request-shape error.
    pub const GENERIC_ERROR: u16 = 205;
    /// The batch `executionType` is not supported by this server.
    pub const UNSUPPORTED_REQUEST_BATCH_EXECUTION_TYPE: u16 = 206;
    /// The server is not ready to perform the request.
    pub const NOT_READY: u16 = 207;
    /// A required field of `requestData` is missing.
    pub const MISSING_REQUEST_FIELD: u16 = 300;
    /// The `requestData` field was required but missing.
    pub const MISSING_REQUEST_DATA: u16 = 301;
    /// A request field has an invalid value.
    pub const INVALID_REQUEST_FIELD: u16 = 400;
    /// A request field has the wrong type.
    pub const INVALID_REQUEST_FIELD_TYPE: u16 = 401;
    /// A request field is out of the accepted range.
    pub const REQUEST_FIELD_OUT_OF_RANGE: u16 = 402;
    /// A request field is empty but must not be.
    pub const REQUEST_FIELD_EMPTY: u16 = 403;
    /// Too many request fields were provided.
    pub const TOO_MANY_REQUEST_FIELDS: u16 = 404;
    /// The output is already running.
    pub const OUTPUT_RUNNING: u16 = 500;
    /// The output is not running.
    pub const OUTPUT_NOT_RUNNING: u16 = 501;
    /// The output is paused.
    pub const OUTPUT_PAUSED: u16 = 502;
    /// The output is not paused.
    pub const OUTPUT_NOT_PAUSED: u16 = 503;
    /// The output is disabled.
    pub const OUTPUT_DISABLED: u16 = 504;
    /// Studio mode is active.
    pub const STUDIO_MODE_ACTIVE: u16 = 505;
    /// Studio mode is not active.
    pub const STUDIO_MODE_NOT_ACTIVE: u16 = 506;
    /// The addressed resource does not exist.
    pub const RESOURCE_NOT_FOUND: u16 = 600;
    /// The resource already exists.
    pub const RESOURCE_ALREADY_EXISTS: u16 = 601;
    /// The resource has the wrong type.
    pub const INVALID_RESOURCE_TYPE: u16 = 602;
    /// Not enough resources to perform the action.
    pub const NOT_ENOUGH_RESOURCES: u16 = 603;
    /// The resource is in the wrong state.
    pub const INVALID_RESOURCE_STATE: u16 = 604;
    /// The input kind is invalid for the request.
    pub const INVALID_INPUT_KIND: u16 = 605;
    /// The resource is not configurable.
    pub const RESOURCE_NOT_CONFIGURABLE: u16 = 606;
    /// The filter kind is invalid for the request.
    pub const INVALID_FILTER_KIND: u16 = 607;
    /// Resource creation failed.
    pub const RESOURCE_CREATION_FAILED: u16 = 700;
    /// Acting on the resource failed.
    pub const RESOURCE_ACTION_FAILED: u16 = 701;
    /// The request failed during processing.
    pub const REQUEST_PROCESSING_FAILED: u16 = 702;
    /// The action cannot be performed.
    pub const CANNOT_ACT: u16 = 703;
}

/// `EventSubscription` bitmask values (RES-007 §Event subscription model).
/// Category bits 0–11; high-volume bits 16–19 are opt-in only and never part
/// of [`ALL`](Self::ALL).
pub mod subscription {
    /// No events.
    pub const NONE: u32 = 0;
    /// General events (exit, vendor, custom).
    pub const GENERAL: u32 = 1 << 0;
    /// Scene collection / profile lifecycle.
    pub const CONFIG: u32 = 1 << 1;
    /// Scene CRUD and program/preview switches.
    pub const SCENES: u32 = 1 << 2;
    /// Input CRUD, volume/mute/track changes.
    pub const INPUTS: u32 = 1 << 3;
    /// Transition events.
    pub const TRANSITIONS: u32 = 1 << 4;
    /// Filter events.
    pub const FILTERS: u32 = 1 << 5;
    /// Stream/record/replay/virtualcam output state.
    pub const OUTPUTS: u32 = 1 << 6;
    /// Scene item CRUD, enable/lock/select.
    pub const SCENE_ITEMS: u32 = 1 << 7;
    /// Media input playback state.
    pub const MEDIA_INPUTS: u32 = 1 << 8;
    /// Third-party vendor events.
    pub const VENDORS: u32 = 1 << 9;
    /// UI events (studio mode, screenshots).
    pub const UI: u32 = 1 << 10;
    /// Canvas events (obs-websocket 5.7.0+).
    pub const CANVASES: u32 = 1 << 11;
    /// All category bits (the `Identify` default); excludes high-volume.
    pub const ALL: u32 = GENERAL
        | CONFIG
        | SCENES
        | INPUTS
        | TRANSITIONS
        | FILTERS
        | OUTPUTS
        | SCENE_ITEMS
        | MEDIA_INPUTS
        | VENDORS
        | UI
        | CANVASES;
    /// High-volume: input volume meters (~every 50 ms).
    pub const INPUT_VOLUME_METERS: u32 = 1 << 16;
    /// High-volume: input active-state changes.
    pub const INPUT_ACTIVE_STATE_CHANGED: u32 = 1 << 17;
    /// High-volume: input show-state changes.
    pub const INPUT_SHOW_STATE_CHANGED: u32 = 1 << 18;
    /// High-volume: scene item transform changes.
    pub const SCENE_ITEM_TRANSFORM_CHANGED: u32 = 1 << 19;
}

/// Builds the `{op, d}` envelope around a payload.
pub fn envelope<T: Serialize>(op: u64, d: &T) -> serde_json::Value {
    serde_json::json!({ "op": op, "d": d })
}

/// Server greeting (op 0), sent immediately on connect.
///
/// Upstream's `obsStudioVersion` is deliberately omitted (ADR-0020 §d):
/// there is no OBS version to report and clients treat the field as
/// optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The emulated obs-websocket version ([`OBS_WEBSOCKET_VERSION`]).
    #[serde(rename = "obsWebSocketVersion")]
    pub obs_web_socket_version: String,
    /// The RPC version the server speaks (1).
    #[serde(rename = "rpcVersion")]
    pub rpc_version: u32,
    /// Auth challenge; present iff the server requires authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<Authentication>,
}

/// The auth challenge carried by [`Hello`] (`salt` stable per server start,
/// `challenge` per session; RES-007 §Authentication).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authentication {
    /// Base64-encoded per-session challenge.
    pub challenge: String,
    /// Base64-encoded per-server salt.
    pub salt: String,
}

/// Client session request (op 1). Until `Identified` arrives the client may
/// send nothing else; a second `Identify` closes the session with 4008.
///
/// Unknown fields (`ignoreNonFatalRequestChecks`, client info, ...) are
/// ignored on decode, per obs-websocket's own unknown-field tolerance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identify {
    /// The RPC version the client wants; only 1 is accepted.
    #[serde(rename = "rpcVersion")]
    pub rpc_version: u32,
    /// The authentication string (SHA-256 challenge response; on
    /// token-configured Prismcast servers, the token itself — a Prismcast
    /// extension, see [`crate::obs_ws`] module docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<String>,
    /// Event subscription bitmask; absent means
    /// [`subscription::ALL`].
    #[serde(
        rename = "eventSubscriptions",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub event_subscriptions: Option<u32>,
}

/// Server session confirmation (op 2). Also the response to `Reidentify`
/// (obs-websocket answers `Reidentify` with an `Identified` message).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identified {
    /// The RPC version both sides will speak (1).
    #[serde(rename = "negotiatedRpcVersion")]
    pub negotiated_rpc_version: u32,
}

/// Client subscription update (op 3). Only `eventSubscriptions` may change;
/// other fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reidentify {
    /// New event subscription bitmask.
    #[serde(
        rename = "eventSubscriptions",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub event_subscriptions: Option<u32>,
}

/// Server event (op 5). `eventIntent` echoes the subscription bitmask bit
/// that gated this event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// obs event name (e.g. `"SceneCreated"`).
    #[serde(rename = "eventType")]
    pub event_type: String,
    /// The subscription bit this event belongs to.
    #[serde(rename = "eventIntent")]
    pub event_intent: u32,
    /// Event payload; omitted when empty.
    #[serde(rename = "eventData", default, skip_serializing_if = "Option::is_none")]
    pub event_data: Option<serde_json::Value>,
}

/// Client request (op 6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// The request name (e.g. `"GetVersion"`).
    #[serde(rename = "requestType")]
    pub request_type: String,
    /// Opaque client-supplied correlation ID, mirrored in the response.
    #[serde(rename = "requestId")]
    pub request_id: String,
    /// Request payload; omitted when empty.
    #[serde(
        rename = "requestData",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub request_data: Option<serde_json::Value>,
}

/// The outcome of one request: `result` is true iff `code == 100`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestStatus {
    /// Whether the request succeeded.
    pub result: bool,
    /// The grouped status code (see [`status`]).
    pub code: u16,
    /// Human-readable detail; required for some codes, omitted when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

impl RequestStatus {
    /// Success (code 100).
    pub fn ok() -> Self {
        Self {
            result: true,
            code: status::SUCCESS,
            comment: None,
        }
    }

    /// A failure with a code and an explanatory comment.
    pub fn error(code: u16, comment: impl Into<String>) -> Self {
        Self {
            result: false,
            code,
            comment: Some(comment.into()),
        }
    }
}

/// Server request response (op 7); mirrors `requestType`/`requestId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestResponse {
    /// The mirrored request name.
    #[serde(rename = "requestType")]
    pub request_type: String,
    /// The mirrored correlation ID.
    #[serde(rename = "requestId")]
    pub request_id: String,
    /// The outcome.
    #[serde(rename = "requestStatus")]
    pub request_status: RequestStatus,
    /// Response payload; omitted on failure.
    #[serde(
        rename = "responseData",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub response_data: Option<serde_json::Value>,
}

/// One request inside a [`RequestBatch`]: batch members have no
/// `requestId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchRequest {
    /// The request name.
    #[serde(rename = "requestType")]
    pub request_type: String,
    /// Request payload; omitted when empty.
    #[serde(
        rename = "requestData",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub request_data: Option<serde_json::Value>,
}

/// How a batch is executed. Only [`SerialRealtime`](Self::SerialRealtime) is
/// supported by this adapter; `SerialFrame` (graphics-thread coupling) and
/// `Parallel` are deferred (OBSWS-002+) and answered with status code 206.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestBatchExecutionType {
    /// Process serially, as fast as possible (wire value 0, the default).
    SerialRealtime,
    /// Process serially in sync with the graphics thread (wire value 1).
    SerialFrame,
    /// Process on a thread pool (wire value 2).
    Parallel,
}

impl RequestBatchExecutionType {
    /// Maps a wire value to an execution type; `None` for out-of-range
    /// values (the session closes with 4005, like upstream).
    pub fn from_wire(value: i64) -> Option<Self> {
        match value {
            0 => Some(Self::SerialRealtime),
            1 => Some(Self::SerialFrame),
            2 => Some(Self::Parallel),
            _ => None,
        }
    }

    /// The wire value.
    pub fn code(self) -> i64 {
        match self {
            Self::SerialRealtime => 0,
            Self::SerialFrame => 1,
            Self::Parallel => 2,
        }
    }
}

/// Client request batch (op 8). Missing/null `executionType` means
/// [`RequestBatchExecutionType::SerialRealtime`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestBatch {
    /// Opaque client-supplied correlation ID for the whole batch.
    #[serde(rename = "requestId")]
    pub request_id: String,
    /// Stop a serial batch at the first failed request.
    #[serde(rename = "haltOnFailure", default)]
    pub halt_on_failure: bool,
    /// Execution type; absent means serial-realtime.
    #[serde(
        rename = "executionType",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub execution_type: Option<i64>,
    /// The requests to execute.
    pub requests: Vec<BatchRequest>,
}

/// One result inside a [`RequestBatchResponse`]: like [`RequestResponse`]
/// but without `requestId` (batch members carry none upstream).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchResult {
    /// The mirrored request name.
    #[serde(rename = "requestType")]
    pub request_type: String,
    /// The outcome.
    #[serde(rename = "requestStatus")]
    pub request_status: RequestStatus,
    /// Response payload; omitted on failure.
    #[serde(
        rename = "responseData",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub response_data: Option<serde_json::Value>,
}

/// Server batch response (op 9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestBatchResponse {
    /// The mirrored batch correlation ID.
    #[serde(rename = "requestId")]
    pub request_id: String,
    /// Per-request results, in request order; with `haltOnFailure` the
    /// vector ends at the first failure.
    pub results: Vec<BatchResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses a fixture, re-serializes, and asserts exact semantic equality
    /// (field-name drift protection: serde renames must reproduce the
    /// obs-websocket wire shape; compared as `Value`, so object key order —
    /// not wire-significant — is ignored).
    fn assert_golden<T>(fixture: &str, value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug + PartialEq,
    {
        let parsed: T = serde_json::from_str(fixture).expect("fixture decodes");
        assert_eq!(&parsed, value, "fixture decode mismatch");
        let serialized = serde_json::to_value(value).expect("encode");
        let fixture_value: serde_json::Value = serde_json::from_str(fixture).expect("fixture json");
        assert_eq!(serialized, fixture_value, "wire shape drifted");
    }

    #[test]
    fn hello_golden_fixture() {
        assert_golden(
            r#"{"obsWebSocketVersion":"5.7.4","rpcVersion":1,"authentication":{"challenge":"ztTBnnuqrqaKDzRM3xcVdbYm38ZX7L8CMv0cRAKGYFg=","salt":"lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI="}}"#,
            &Hello {
                obs_web_socket_version: "5.7.4".into(),
                rpc_version: 1,
                authentication: Some(Authentication {
                    challenge: "ztTBnnuqrqaKDzRM3xcVdbYm38ZX7L8CMv0cRAKGYFg=".into(),
                    salt: "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=".into(),
                }),
            },
        );
        // Without auth the field is omitted entirely.
        let hello = Hello {
            obs_web_socket_version: OBS_WEBSOCKET_VERSION.into(),
            rpc_version: RPC_VERSION,
            authentication: None,
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert_eq!(json, r#"{"obsWebSocketVersion":"5.7.4","rpcVersion":1}"#);
        assert!(!json.contains("obs_web_socket_version"));
    }

    #[test]
    fn identify_golden_fixture() {
        assert_golden(
            r#"{"rpcVersion":1,"authentication":"ScJezpEqvLaUbYm7fZOAYGVa+hwkSJeMSkPA/s7nTtE=","eventSubscriptions":33}"#,
            &Identify {
                rpc_version: 1,
                authentication: Some("ScJezpEqvLaUbYm7fZOAYGVa+hwkSJeMSkPA/s7nTtE=".into()),
                event_subscriptions: Some(33),
            },
        );
        // Minimal identify: only rpcVersion.
        let minimal: Identify = serde_json::from_str(r#"{"rpcVersion":1}"#).unwrap();
        assert_eq!(minimal.authentication, None);
        assert_eq!(minimal.event_subscriptions, None);
        // Unknown fields (e.g. ignoreNonFatalRequestChecks) are tolerated.
        let lenient: Identify =
            serde_json::from_str(r#"{"rpcVersion":1,"ignoreNonFatalRequestChecks":true}"#).unwrap();
        assert_eq!(lenient.rpc_version, 1);
    }

    #[test]
    fn identified_golden_fixture() {
        assert_golden(
            r#"{"negotiatedRpcVersion":1}"#,
            &Identified {
                negotiated_rpc_version: 1,
            },
        );
    }

    #[test]
    fn reidentify_golden_fixture() {
        assert_golden(
            r#"{"eventSubscriptions":77}"#,
            &Reidentify {
                event_subscriptions: Some(77),
            },
        );
        let empty: Reidentify = serde_json::from_str(r"{}").unwrap();
        assert_eq!(empty.event_subscriptions, None);
    }

    #[test]
    fn event_golden_fixture() {
        assert_golden(
            r#"{"eventType":"SceneCreated","eventIntent":4,"eventData":{"sceneName":"Main","isGroup":false}}"#,
            &Event {
                event_type: "SceneCreated".into(),
                event_intent: subscription::SCENES,
                event_data: Some(serde_json::json!({"sceneName": "Main", "isGroup": false})),
            },
        );
        // No payload → no eventData key.
        let bare = Event {
            event_type: "ExitStarted".into(),
            event_intent: subscription::GENERAL,
            event_data: None,
        };
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"eventType":"ExitStarted","eventIntent":1}"#
        );
    }

    #[test]
    fn request_golden_fixture() {
        assert_golden(
            r#"{"requestType":"SetCurrentProgramScene","requestId":"f819dcf0-89cc-11eb-8f0d-382c4ac93b9c","requestData":{"sceneName":"Main"}}"#,
            &Request {
                request_type: "SetCurrentProgramScene".into(),
                request_id: "f819dcf0-89cc-11eb-8f0d-382c4ac93b9c".into(),
                request_data: Some(serde_json::json!({"sceneName": "Main"})),
            },
        );
    }

    #[test]
    fn request_status_golden_fixtures() {
        assert_golden(r#"{"result":true,"code":100}"#, &RequestStatus::ok());
        assert_golden(
            r#"{"result":false,"code":204,"comment":"unknown request type"}"#,
            &RequestStatus::error(status::UNKNOWN_REQUEST_TYPE, "unknown request type"),
        );
    }

    #[test]
    fn request_response_golden_fixture() {
        assert_golden(
            r#"{"requestType":"GetVersion","requestId":"req-1","requestStatus":{"result":true,"code":100},"responseData":{"rpcVersion":1}}"#,
            &RequestResponse {
                request_type: "GetVersion".into(),
                request_id: "req-1".into(),
                request_status: RequestStatus::ok(),
                response_data: Some(serde_json::json!({"rpcVersion": 1})),
            },
        );
    }

    #[test]
    fn request_batch_golden_fixture() {
        assert_golden(
            r#"{"requestId":"batch-1","haltOnFailure":true,"executionType":0,"requests":[{"requestType":"Sleep","requestData":{"sleepMillis":100}},{"requestType":"GetVersion"}]}"#,
            &RequestBatch {
                request_id: "batch-1".into(),
                halt_on_failure: true,
                execution_type: Some(0),
                requests: vec![
                    BatchRequest {
                        request_type: "Sleep".into(),
                        request_data: Some(serde_json::json!({"sleepMillis": 100})),
                    },
                    BatchRequest {
                        request_type: "GetVersion".into(),
                        request_data: None,
                    },
                ],
            },
        );
        // Defaults: haltOnFailure false, executionType absent.
        let minimal: RequestBatch =
            serde_json::from_str(r#"{"requestId":"b","requests":[]}"#).unwrap();
        assert!(!minimal.halt_on_failure);
        assert_eq!(minimal.execution_type, None);
        assert_eq!(
            serde_json::to_string(&minimal).unwrap(),
            r#"{"requestId":"b","haltOnFailure":false,"requests":[]}"#
        );
    }

    #[test]
    fn request_batch_response_golden_fixture() {
        assert_golden(
            r#"{"requestId":"batch-1","results":[{"requestType":"Sleep","requestStatus":{"result":true,"code":100}},{"requestType":"GetVersion","requestStatus":{"result":false,"code":204,"comment":"unknown"}}]}"#,
            &RequestBatchResponse {
                request_id: "batch-1".into(),
                results: vec![
                    BatchResult {
                        request_type: "Sleep".into(),
                        request_status: RequestStatus::ok(),
                        response_data: None,
                    },
                    BatchResult {
                        request_type: "GetVersion".into(),
                        request_status: RequestStatus::error(204, "unknown"),
                        response_data: None,
                    },
                ],
            },
        );
    }

    #[test]
    fn envelope_shape_is_op_d() {
        let value = envelope(
            op::IDENTIFIED,
            &Identified {
                negotiated_rpc_version: 1,
            },
        );
        assert_eq!(
            value,
            serde_json::json!({"op": 2, "d": {"negotiatedRpcVersion": 1}})
        );
        // The envelope has exactly the `op` and `d` keys.
        let object = value.as_object().unwrap();
        let mut keys: Vec<&String> = object.keys().collect();
        keys.sort();
        assert_eq!(keys, ["d", "op"]);
    }

    #[test]
    fn execution_type_wire_values() {
        assert_eq!(
            RequestBatchExecutionType::from_wire(0),
            Some(RequestBatchExecutionType::SerialRealtime)
        );
        assert_eq!(
            RequestBatchExecutionType::from_wire(1),
            Some(RequestBatchExecutionType::SerialFrame)
        );
        assert_eq!(
            RequestBatchExecutionType::from_wire(2),
            Some(RequestBatchExecutionType::Parallel)
        );
        assert_eq!(RequestBatchExecutionType::from_wire(-1), None);
        assert_eq!(RequestBatchExecutionType::from_wire(3), None);
        for ty in [
            RequestBatchExecutionType::SerialRealtime,
            RequestBatchExecutionType::SerialFrame,
            RequestBatchExecutionType::Parallel,
        ] {
            assert_eq!(RequestBatchExecutionType::from_wire(ty.code()), Some(ty));
        }
    }

    #[test]
    fn subscription_bitmask_matches_obs_5_x() {
        assert_eq!(subscription::NONE, 0);
        assert_eq!(subscription::GENERAL, 1);
        assert_eq!(subscription::CANVASES, 1 << 11);
        assert_eq!(subscription::INPUT_VOLUME_METERS, 1 << 16);
        assert_eq!(subscription::SCENE_ITEM_TRANSFORM_CHANGED, 1 << 19);
        // `All` covers exactly the category bits 0..=11, not high-volume.
        assert_eq!(subscription::ALL, (1 << 12) - 1);
        assert_eq!(subscription::ALL & subscription::INPUT_VOLUME_METERS, 0);
    }
}

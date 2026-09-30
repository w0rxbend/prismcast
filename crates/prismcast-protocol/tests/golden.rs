//! Golden message tests (PLAN.md §63): the exact JSON bytes of
//! representative protocol messages are pinned here. Any intentional wire
//! format change must update these strings in the same commit — a failing
//! golden test is the tripwire that the wire schema moved.
//!
//! These shapes are also the canonical examples referenced from
//! `docs/protocols/native-protocol.md`.

use prismcast_protocol::batch::{BatchRequest, RequestBatch};
use prismcast_protocol::data::{Anchor, Transform, Vec2};
use prismcast_protocol::error::{ErrorKind, WireError};
use prismcast_protocol::event::{EventMessage, MeterEvent, SceneEvent, WireEvent};
use prismcast_protocol::handshake::{
    AuthChallenge, AuthResponse, ClientInfo, Hello, Identified, Identify, Permission,
};
use prismcast_protocol::message::{ClientMessage, ServerMessage};
use prismcast_protocol::request::{Request, RequestKind};
use prismcast_protocol::response::{RequestResponse, ResponseData, ResponseStatus};
use prismcast_protocol::subscription::{EventCategory, Subscription, SubscriptionSet};
use uuid::Uuid;

const ID_A: &str = "11111111-1111-4111-8111-111111111111";
const ID_B: &str = "22222222-2222-4222-8222-222222222222";

fn uuid_a() -> Uuid {
    Uuid::parse_str(ID_A).unwrap()
}

fn uuid_b() -> Uuid {
    Uuid::parse_str(ID_B).unwrap()
}

/// Asserts both directions: `message` serializes to exactly `expected`, and
/// `expected` deserializes back to `message`.
fn assert_golden<T>(message: &T, expected: &str)
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq,
{
    let actual = serde_json::to_string(message).unwrap();
    assert_eq!(actual, expected, "serialization drifted from golden shape");
    let back: T = serde_json::from_str(expected).unwrap();
    assert_eq!(*message, back, "golden shape no longer roundtrips");
}

#[test]
fn golden_hello_without_auth() {
    let message = ServerMessage::Hello(Hello {
        prismcast_version: "0.1.0".into(),
        protocol_version: 1,
        min_protocol_version: 1,
        authentication: None,
    });
    assert_golden(
        &message,
        r#"{"type":"hello","data":{"prismcast_version":"0.1.0","protocol_version":1,"min_protocol_version":1}}"#,
    );
}

#[test]
fn golden_hello_with_auth_challenge() {
    let message = ServerMessage::Hello(Hello {
        prismcast_version: "0.1.0".into(),
        protocol_version: 1,
        min_protocol_version: 1,
        authentication: Some(AuthChallenge {
            salt: "c2FsdA==".into(),
            challenge: "Y2hhbGxlbmdl".into(),
        }),
    });
    assert_golden(
        &message,
        r#"{"type":"hello","data":{"prismcast_version":"0.1.0","protocol_version":1,"min_protocol_version":1,"authentication":{"salt":"c2FsdA==","challenge":"Y2hhbGxlbmdl"}}}"#,
    );
}

#[test]
fn golden_identify_with_token_and_subscriptions() {
    let message = ClientMessage::Identify(Identify {
        protocol_version: 1,
        authentication: Some(AuthResponse::Token {
            token: "secret-token".into(),
        }),
        subscriptions: Some(SubscriptionSet {
            entries: vec![
                Subscription::category(EventCategory::Scene),
                Subscription::meters(vec![uuid_a()], Some(100)),
            ],
        }),
        client: Some(ClientInfo {
            name: "prismcast-cli".into(),
            version: Some("0.1.0".into()),
        }),
    });
    assert_golden(
        &message,
        &format!(
            r#"{{"type":"identify","data":{{"protocol_version":1,"authentication":{{"method":"token","token":"secret-token"}},"subscriptions":[{{"category":"scene"}},{{"category":"meter","entity_ids":["{ID_A}"],"throttle_ms":100}}],"client":{{"name":"prismcast-cli","version":"0.1.0"}}}}}}"#
        ),
    );
}

#[test]
fn golden_identified() {
    let message = ServerMessage::Identified(Identified {
        negotiated_protocol_version: 1,
        session_id: uuid_a(),
        permissions: vec![Permission::Read, Permission::ControlScenes],
    });
    assert_golden(
        &message,
        &format!(
            r#"{{"type":"identified","data":{{"negotiated_protocol_version":1,"session_id":"{ID_A}","permissions":["read","control_scenes"]}}}}"#
        ),
    );
}

#[test]
fn golden_request_add_scene() {
    let message = ClientMessage::Request(Request {
        request_id: "req-1".into(),
        kind: RequestKind::AddScene {
            name: "Main".into(),
        },
    });
    assert_golden(
        &message,
        r#"{"type":"request","data":{"request_id":"req-1","request":"add_scene","name":"Main"}}"#,
    );
}

#[test]
fn golden_request_set_scene_item_transform() {
    let message = ClientMessage::Request(Request {
        request_id: "req-2".into(),
        kind: RequestKind::SetSceneItemTransform {
            scene_id: uuid_a(),
            item_id: uuid_b(),
            transform: Transform {
                position: Vec2 { x: 10.0, y: 20.0 },
                scale: Vec2 { x: 1.5, y: 1.5 },
                rotation: 90.0,
                anchor: Anchor::Center,
            },
        },
    });
    assert_golden(
        &message,
        &format!(
            r#"{{"type":"request","data":{{"request_id":"req-2","request":"set_scene_item_transform","scene_id":"{ID_A}","item_id":"{ID_B}","transform":{{"position":{{"x":10.0,"y":20.0}},"scale":{{"x":1.5,"y":1.5}},"rotation":90.0,"anchor":"center"}}}}}}"#
        ),
    );
}

#[test]
fn golden_request_response_success() {
    let message = ServerMessage::RequestResponse(RequestResponse {
        request_id: "req-1".into(),
        request_type: "add_scene".into(),
        status: ResponseStatus::ok(),
        data: Some(ResponseData::SceneCreated { scene_id: uuid_a() }),
    });
    assert_golden(
        &message,
        &format!(
            r#"{{"type":"request_response","data":{{"request_id":"req-1","request_type":"add_scene","status":{{"ok":true}},"data":{{"data":"scene_created","scene_id":"{ID_A}"}}}}}}"#
        ),
    );
}

#[test]
fn golden_request_response_error() {
    let message = ServerMessage::RequestResponse(RequestResponse {
        request_id: "req-9".into(),
        request_type: "stop_output".into(),
        status: ResponseStatus::error(
            WireError::new(ErrorKind::StateConflict, "output is not running")
                .with_field("output_id")
                .with_details(serde_json::json!({"state": "stopped"})),
        ),
        data: None,
    });
    assert_golden(
        &message,
        r#"{"type":"request_response","data":{"request_id":"req-9","request_type":"stop_output","status":{"ok":false,"error":{"code":500,"kind":"state_conflict","message":"output is not running","field":"output_id","details":{"state":"stopped"}}}}}"#,
    );
}

#[test]
fn golden_request_batch() {
    let message = ClientMessage::RequestBatch(RequestBatch {
        request_id: "batch-1".into(),
        halt_on_failure: true,
        requests: vec![
            BatchRequest {
                request_id: Some("m-1".into()),
                kind: RequestKind::SetSourceMuted {
                    source_id: uuid_a(),
                    muted: true,
                },
            },
            BatchRequest {
                request_id: None,
                kind: RequestKind::TransitionToProgram,
            },
        ],
    });
    assert_golden(
        &message,
        &format!(
            r#"{{"type":"request_batch","data":{{"request_id":"batch-1","halt_on_failure":true,"requests":[{{"request_id":"m-1","request":"set_source_muted","source_id":"{ID_A}","muted":true}},{{"request":"transition_to_program"}}]}}}}"#
        ),
    );
}

#[test]
fn golden_event_scene_added() {
    let message = ServerMessage::Event(EventMessage {
        seq: 0,
        category: EventCategory::Scene,
        event: WireEvent::Scene(SceneEvent::Added {
            scene_id: uuid_a(),
            name: "Main".into(),
        }),
    });
    assert_golden(
        &message,
        &format!(
            r#"{{"type":"event","data":{{"seq":0,"category":"scene","domain":"scene","event":"added","scene_id":"{ID_A}","name":"Main"}}}}"#
        ),
    );
}

#[test]
fn golden_event_meter_levels() {
    let message = ServerMessage::Event(EventMessage {
        seq: 512,
        category: EventCategory::Meter,
        event: WireEvent::Meter(MeterEvent::Levels {
            source_id: uuid_a(),
            peak_dbfs: vec![-3.0, -3.5],
            rms_dbfs: vec![-12.0, -12.4],
        }),
    });
    assert_golden(
        &message,
        &format!(
            r#"{{"type":"event","data":{{"seq":512,"category":"meter","domain":"meter","event":"levels","source_id":"{ID_A}","peak_dbfs":[-3.0,-3.5],"rms_dbfs":[-12.0,-12.4]}}}}"#
        ),
    );
}

#[test]
fn golden_request_update_subscriptions() {
    let message = ClientMessage::Request(Request {
        request_id: "req-3".into(),
        kind: RequestKind::UpdateSubscriptions {
            subscriptions: SubscriptionSet::none(),
        },
    });
    assert_golden(
        &message,
        r#"{"type":"request","data":{"request_id":"req-3","request":"update_subscriptions","subscriptions":[]}}"#,
    );
}

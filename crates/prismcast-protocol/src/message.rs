//! Message envelopes: the complete set of frames in each direction.
//!
//! Every frame is a self-describing tagged object: `{"type": ..., "data":
//! {...}}`. Unlike obs-websocket's numeric `op` codes, string tags are
//! readable in logs, stable under renumbering, and generate cleanly into a
//! schema (RES-007 weaknesses 2–3). Op 4-style numeric gaps are unnecessary
//! when tags are names.

use serde::{Deserialize, Serialize};

use crate::batch::{RequestBatch, RequestBatchResponse};
use crate::event::EventMessage;
use crate::handshake::{Hello, Identified, Identify};
use crate::request::Request;
use crate::response::RequestResponse;

/// Every message a client can send.
///
/// Until [`Identified`](crate::handshake::Identified) is received, only
/// `Identify` is legal; anything else terminates the session with
/// [`crate::handshake::CloseCode::NotIdentified`], and a second `Identify`
/// with [`crate::handshake::CloseCode::AlreadyIdentified`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Session establishment (first and only pre-identification message).
    Identify(Identify),
    /// A single request.
    Request(Request),
    /// A serial batch of requests.
    RequestBatch(RequestBatch),
}

/// Every message the server can send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Initial greeting with versions and optional auth challenge; the
    /// first message on every connection.
    Hello(Hello),
    /// Session established.
    Identified(Identified),
    /// A subscribed event.
    Event(EventMessage),
    /// Answer to a single request.
    RequestResponse(RequestResponse),
    /// Answer to a request batch.
    RequestBatchResponse(RequestBatchResponse),
}

impl ServerMessage {
    /// The `type` tag of this message (for logging).
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::Identified(_) => "identified",
            Self::Event(_) => "event",
            Self::RequestResponse(_) => "request_response",
            Self::RequestBatchResponse(_) => "request_batch_response",
        }
    }
}

impl ClientMessage {
    /// The `type` tag of this message (for logging).
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Identify(_) => "identify",
            Self::Request(_) => "request",
            Self::RequestBatch(_) => "request_batch",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::RequestKind;

    #[test]
    fn envelope_tags_are_snake_case() {
        let message = ClientMessage::Request(Request {
            request_id: "r".into(),
            kind: RequestKind::GetVersion,
        });
        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["type"], "request");
        assert_eq!(value["data"]["request"], "get_version");

        let batch = ClientMessage::RequestBatch(RequestBatch {
            request_id: "b".into(),
            halt_on_failure: false,
            requests: Vec::new(),
        });
        let value = serde_json::to_value(&batch).unwrap();
        assert_eq!(value["type"], "request_batch");
        assert_eq!(batch.tag(), "request_batch");
    }

    #[test]
    fn envelope_roundtrip_both_directions() {
        let client = ClientMessage::Identify(Identify {
            protocol_version: 1,
            authentication: None,
            subscriptions: None,
            client: None,
        });
        let json = serde_json::to_string(&client).unwrap();
        assert_eq!(client, serde_json::from_str(&json).unwrap());
        assert_eq!(client.tag(), "identify");

        let server = ServerMessage::Hello(Hello {
            prismcast_version: "0.1.0".into(),
            protocol_version: 1,
            min_protocol_version: 1,
            authentication: None,
        });
        let json = serde_json::to_string(&server).unwrap();
        assert_eq!(server, serde_json::from_str(&json).unwrap());
        assert_eq!(server.tag(), "hello");
    }

    #[test]
    fn unknown_type_tag_is_a_decode_error() {
        let result = serde_json::from_str::<ClientMessage>(r#"{"type":"teleport","data":{}}"#);
        assert!(result.is_err());
    }
}

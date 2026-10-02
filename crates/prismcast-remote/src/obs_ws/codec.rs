//! The obs-websocket wire codec (OBSWS-002; ADR-0021).
//!
//! obs-websocket 5.x defines two codecs behind the `Sec-WebSocket-Protocol`
//! subprotocol negotiation: JSON on text frames (`obswebsocket.json`, the
//! default) and MessagePack on binary frames (`obswebsocket.msgpack`). Both
//! encode the **same** `{op, d}` envelope shape: MessagePack uses
//! struct-as-map encoding (`rmp_serde::to_vec_named`, string-keyed maps), so
//! the session engine and everything above it — translation, request
//! dispatch, event gating — works on `serde_json::Value` envelopes and never
//! learns which codec is on the wire. `rmp-serde` is already a
//! `prismcast-remote` dependency (the IPC codec), so this adds no
//! dependency.
//!
//! The codec lives exactly at the session framing boundary: the outbound
//! writer ([`ObsCodec::encode`]) and the inbound frame reader
//! ([`ObsCodec::decode`]). A frame of the wrong kind for the negotiated
//! codec, an undecodable payload, or hostile MessagePack (ext types, garbage
//! bytes) maps to a [`CodecError`], which the session closes with 4002
//! (`MessageDecodeError`) — never a panic. The raw-payload size limit is
//! enforced by the caller before decode (it is a transport property, equal
//! for both codecs).

use tokio_tungstenite::tungstenite::Message;

/// The negotiated wire codec of one obs session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObsCodec {
    /// JSON text frames (the obs default, `obswebsocket.json`).
    Json,
    /// MessagePack binary frames, struct-as-map (`obswebsocket.msgpack`).
    MsgPack,
}

/// A frame that cannot be decoded under the negotiated codec. Carries the
/// WebSocket close reason for the 4002 (`MessageDecodeError`) close.
#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
pub(crate) struct CodecError {
    reason: String,
}

impl CodecError {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// The close reason the session sends with its 4002 close frame.
    pub(crate) fn close_reason(&self) -> &str {
        &self.reason
    }
}

impl ObsCodec {
    /// Encodes one `{op, d}` envelope as an outbound WebSocket frame.
    pub(crate) fn encode(&self, value: &serde_json::Value) -> Message {
        match self {
            // Serializing a Value is total; a failure here is a bug.
            Self::Json => Message::Text(value.to_string().into()),
            // Encoding a JSON-shaped value as struct-as-map MessagePack is
            // total (map keys are strings by construction); a failure here
            // is a bug.
            Self::MsgPack => Message::Binary(
                rmp_serde::to_vec_named(value)
                    .expect("serde_json::Value is always msgpack-encodable")
                    .into(),
            ),
        }
    }

    /// Decodes one inbound WebSocket frame into an envelope value.
    ///
    /// `Ok(None)` means the frame carries no protocol payload (ping/pong —
    /// answered automatically by tungstenite); the caller continues its read
    /// loop. `Err` maps to a 4002 (`MessageDecodeError`) close: a frame of
    /// the wrong kind for the negotiated codec, or a payload that does not
    /// decode. Close frames are handled by the caller before decode.
    pub(crate) fn decode(
        &self,
        message: &Message,
    ) -> Result<Option<serde_json::Value>, CodecError> {
        match (self, message) {
            (Self::Json, Message::Text(text)) => serde_json::from_str(text)
                .map(Some)
                .map_err(|e| CodecError::new(format!("unable to decode Json: {e}"))),
            (Self::MsgPack, Message::Binary(payload)) => {
                rmp_serde::from_slice::<serde_json::Value>(payload)
                    .map(Some)
                    .map_err(|e| CodecError::new(format!("unable to decode MsgPack: {e}")))
            }
            (Self::Json, Message::Binary(_)) => Err(CodecError::new(
                "session encoding is Json, but a binary message was received",
            )),
            (Self::MsgPack, Message::Text(_)) => Err(CodecError::new(
                "session encoding is MsgPack, but a text message was received",
            )),
            // Ping/Pong carry no protocol payload; Close frames are handled
            // by the caller before decode and raw `Frame`s never surface here.
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip() {
        let value = serde_json::json!({"op": 5, "d": {"eventType": "SceneCreated"}});
        let message = ObsCodec::Json.encode(&value);
        let decoded = ObsCodec::Json
            .decode(&message)
            .expect("decode")
            .expect("payload");
        assert_eq!(decoded, value);
    }

    #[test]
    fn msgpack_roundtrip_is_binary() {
        let value = serde_json::json!({"op": 7, "d": {"requestId": "r-1", "code": 100}});
        let message = ObsCodec::MsgPack.encode(&value);
        assert!(matches!(message, Message::Binary(_)), "msgpack is binary");
        let decoded = ObsCodec::MsgPack
            .decode(&message)
            .expect("decode")
            .expect("payload");
        assert_eq!(decoded, value);
    }

    #[test]
    fn wrong_frame_kind_is_a_codec_error() {
        let text = Message::Text("{}".into());
        let binary = Message::Binary(Vec::new().into());
        assert!(ObsCodec::MsgPack.decode(&text).is_err(), "text in msgpack");
        assert!(ObsCodec::Json.decode(&binary).is_err(), "binary in json");
    }

    #[test]
    fn hostile_msgpack_is_a_codec_error_not_a_panic() {
        // MsgPack ext type (0xd4 fixext1) — not representable as a JSON value.
        let ext = Message::Binary(vec![0xd4, 0x01, 0x00].into());
        assert!(ObsCodec::MsgPack.decode(&ext).is_err());
        // Garbage bytes.
        let garbage = Message::Binary(vec![0xc1].into());
        assert!(ObsCodec::MsgPack.decode(&garbage).is_err());
    }

    #[test]
    fn ping_pong_carry_no_payload() {
        for message in [
            Message::Ping(Vec::new().into()),
            Message::Pong(Vec::new().into()),
        ] {
            for codec in [ObsCodec::Json, ObsCodec::MsgPack] {
                assert_eq!(codec.decode(&message).expect("decode"), None);
            }
        }
    }
}

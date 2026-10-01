//! Unix-socket framing and the MessagePack codec (IPC-001; ADR-0006 §2).
//!
//! Frames are a 4-byte big-endian length prefix followed by a MessagePack
//! payload. Payloads use `rmp-serde`'s human-readable mode with structs as
//! string-keyed maps (the `to_vec_named` shape): the envelope stays
//! self-describing (`{"type": ..., "data": {...}}`), unknown-field tolerance
//! on decode works, and serde types that switch on `is_human_readable` (e.g.
//! `Uuid`, which would otherwise encode as a 16-byte binary blob that
//! `serde_json::Value` cannot represent) serialize as strings — one
//! JSON-shaped data model serves both codecs (protocol doc §1).
//!
//! Decoding goes through a [`serde_json::Value`] intermediate: the protocol
//! data model is strictly JSON-shaped, so the conversion is lossless, and the
//! intermediate lets peers classify the `type` tag before committing to a
//! typed decode (needed for the protocol's "unknown tag" error semantics and
//! for the [`ClosingNotice`] frame, which is not part of
//! `prismcast_protocol::ServerMessage`).
//!
//! ## Closing notice
//!
//! Unix streams have no close codes, so before closing a session the server
//! sends a final frame `{"type": "closing", "data": {"code", "reason",
//! "message"}}` carrying the same numeric code the WebSocket transport would
//! use as a close code (protocol doc §8).

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;

use prismcast_protocol::handshake::CloseCode;
use prismcast_protocol::message::ServerMessage;

use crate::session::{FrameReadError, FrameReader, FrameWriteError, FrameWriter};

/// Default maximum frame payload size (4 MiB). Bounds memory per connection
/// (PLAN.md §75); larger frames are rejected before allocation.
pub const DEFAULT_MAX_FRAME_SIZE: usize = 4 * 1024 * 1024;

/// Length of the frame prefix.
const LEN_PREFIX: usize = 4;

/// The `type` tag of a terminal closing-notice frame.
pub const CLOSING_FRAME_TYPE: &str = "closing";

/// Terminal notice sent before the server closes a session (IPC substitute
/// for WebSocket close codes, protocol doc §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosingNotice {
    /// Numeric code, aligned with [`CloseCode`].
    pub code: u16,
    /// Snake-case reason label (e.g. `"slow_consumer"`).
    pub reason: String,
    /// Human-readable explanation.
    pub message: String,
}

impl ClosingNotice {
    /// Builds a notice from a close code and a message.
    pub fn new(code: CloseCode, message: impl Into<String>) -> Self {
        Self {
            code: code.code(),
            reason: reason_label(code).to_string(),
            message: message.into(),
        }
    }

    /// The typed close code, if the numeric code is a known one.
    pub fn close_code(&self) -> Option<CloseCode> {
        CloseCode::from_code(self.code)
    }
}

/// Snake-case label for a close code (the WebSocket close reason strings).
fn reason_label(code: CloseCode) -> &'static str {
    match code {
        CloseCode::UnknownReason => "unknown_reason",
        CloseCode::MessageDecodeError => "message_decode_error",
        CloseCode::UnknownMessageType => "unknown_message_type",
        CloseCode::NotIdentified => "not_identified",
        CloseCode::AlreadyIdentified => "already_identified",
        CloseCode::AuthenticationFailed => "authentication_failed",
        CloseCode::UnsupportedProtocolVersion => "unsupported_protocol_version",
        CloseCode::SessionInvalidated => "session_invalidated",
        CloseCode::UnsupportedFeature => "unsupported_feature",
        CloseCode::SlowConsumer => "slow_consumer",
        CloseCode::RateLimited => "rate_limited",
        CloseCode::ServerShutdown => "server_shutdown",
    }
}

/// Encodes a value as a MessagePack payload (string-keyed struct maps,
/// human-readable scalars — see module docs).
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let mut buf = Vec::new();
    {
        let mut serializer = rmp_serde::Serializer::new(&mut buf)
            .with_struct_map()
            .with_human_readable();
        value.serialize(&mut serializer)?;
    }
    Ok(buf)
}

/// Encodes a closing notice as a `{"type": "closing", "data": {...}}` frame.
pub fn encode_closing(notice: &ClosingNotice) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    #[derive(Serialize)]
    struct ClosingFrame<'a> {
        #[serde(rename = "type")]
        frame_type: &'static str,
        data: &'a ClosingNotice,
    }
    encode(&ClosingFrame {
        frame_type: CLOSING_FRAME_TYPE,
        data: notice,
    })
}

/// Decodes a MessagePack payload into a generic value for tag classification.
pub fn decode_value(payload: &[u8]) -> Result<serde_json::Value, rmp_serde::decode::Error> {
    let mut deserializer = rmp_serde::Deserializer::new(payload).with_human_readable();
    serde_json::Value::deserialize(&mut deserializer)
}

/// Writes one length-prefixed frame.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> std::io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"))?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

/// Reads one length-prefixed frame. Returns `None` on a clean EOF at a frame
/// boundary; an oversized frame is an error *before* its payload is
/// allocated or read.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_payload: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; LEN_PREFIX];
    let mut read = 0;
    while read < LEN_PREFIX {
        let n = reader.read(&mut len_buf[read..]).await?;
        if n == 0 {
            if read == 0 {
                return Ok(None);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed mid-frame",
            ));
        }
        read += n;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > max_payload {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame payload {len} bytes exceeds limit of {max_payload}"),
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

/// Splits a Unix stream into the IPC transport's session halves.
pub(crate) fn split_ipc(
    stream: UnixStream,
    max_frame_size: usize,
) -> (IpcFrameReader, IpcFrameWriter) {
    let (reader, writer) = stream.into_split();
    (
        IpcFrameReader {
            inner: reader,
            max_frame_size,
        },
        IpcFrameWriter { inner: writer },
    )
}

/// IPC [`FrameReader`]: length-prefixed MessagePack frames decoded through a
/// [`serde_json::Value`] intermediate (see module docs).
pub(crate) struct IpcFrameReader {
    inner: tokio::net::unix::OwnedReadHalf,
    max_frame_size: usize,
}

impl FrameReader for IpcFrameReader {
    async fn read_value(&mut self) -> Result<Option<serde_json::Value>, FrameReadError> {
        let Some(payload) = read_frame(&mut self.inner, self.max_frame_size)
            .await
            .map_err(|e| FrameReadError(e.to_string()))?
        else {
            return Ok(None);
        };
        decode_value(&payload)
            .map(Some)
            .map_err(|e| FrameReadError(e.to_string()))
    }
}

/// IPC [`FrameWriter`]: MessagePack payloads behind a length prefix; the
/// closing notice is sent as a `{"type": "closing"}` frame (protocol doc §8).
pub(crate) struct IpcFrameWriter {
    inner: tokio::net::unix::OwnedWriteHalf,
}

impl FrameWriter for IpcFrameWriter {
    async fn write_message(&mut self, message: &ServerMessage) -> Result<(), FrameWriteError> {
        // Encoding this type is total; a failure here is a bug, not a client
        // error.
        let bytes = encode(message).map_err(|e| FrameWriteError(e.to_string()))?;
        write_frame(&mut self.inner, &bytes)
            .await
            .map_err(|e| FrameWriteError(e.to_string()))
    }

    async fn write_close(&mut self, notice: &ClosingNotice) -> Result<(), FrameWriteError> {
        let bytes = encode_closing(notice).map_err(|e| FrameWriteError(e.to_string()))?;
        write_frame(&mut self.inner, &bytes)
            .await
            .map_err(|e| FrameWriteError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip_over_duplex() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let payload = encode(&serde_json::json!({"type": "ping", "n": 42})).expect("encode");
        write_frame(&mut a, &payload).await.expect("write");
        let back = read_frame(&mut b, DEFAULT_MAX_FRAME_SIZE)
            .await
            .expect("read")
            .expect("frame");
        assert_eq!(back, payload);
        let value = decode_value(&back).expect("decode");
        assert_eq!(value["n"], 42);
        drop(a);
        assert_eq!(
            read_frame(&mut b, DEFAULT_MAX_FRAME_SIZE)
                .await
                .expect("eof"),
            None
        );
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_allocation() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        a.write_all(&(DEFAULT_MAX_FRAME_SIZE as u32 + 1).to_be_bytes())
            .await
            .expect("write prefix");
        let error = read_frame(&mut b, DEFAULT_MAX_FRAME_SIZE)
            .await
            .expect_err("must reject");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn closing_notice_roundtrip_with_tag() {
        let notice = ClosingNotice::new(CloseCode::SlowConsumer, "outbound queue overflow");
        let bytes = encode_closing(&notice).expect("encode");
        let value = decode_value(&bytes).expect("decode");
        assert_eq!(value["type"], CLOSING_FRAME_TYPE);
        let back: ClosingNotice =
            serde_json::from_value(value["data"].clone()).expect("closing data");
        assert_eq!(back, notice);
        assert_eq!(back.close_code(), Some(CloseCode::SlowConsumer));
        assert_eq!(back.reason, "slow_consumer");
    }
}

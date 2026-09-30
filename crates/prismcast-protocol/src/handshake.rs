//! Session establishment: `Hello` / `Identify` / `Identified`.
//!
//! The handshake intentionally resembles obs-websocket 5.x (RES-007 §
//! Connection lifecycle, ADR-0010 §2): the server speaks first, the client
//! must `Identify` before any other traffic, and violations terminate the
//! session with a [`CloseCode`].
//!
//! Differences from obs-websocket, decided for the native protocol:
//! - Subscription changes after identification are a regular **request**
//!   (`update_subscriptions`, see [`crate::request::RequestKind`]), so they
//!   are correlatable and errorable — there is no `Reidentify` message
//!   (RES-007 open question 3, answered: request).
//! - Authentication supports both challenge-response (for the Unix-IPC and
//!   local paths, where TLS is absent) and bearer tokens (for the TLS
//!   WebSocket path, PLAN.md §24).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::subscription::SubscriptionSet;

/// Server greeting, sent immediately after the connection is established.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    /// Server software version (e.g. `"0.1.0"`).
    pub prismcast_version: String,
    /// Highest protocol version the server supports.
    pub protocol_version: u32,
    /// Lowest protocol version the server can still serve.
    pub min_protocol_version: u32,
    /// Authentication challenge; absent when the server has auth disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authentication: Option<AuthChallenge>,
}

/// Challenge data for password-based authentication (SHA-256
/// challenge-response, same construction as obs-websocket: the client
/// answers with `base64(sha256(base64(sha256(password + salt)) + challenge))`).
///
/// `salt` is generated once per server start, `challenge` per session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthChallenge {
    /// Base64-encoded per-server salt.
    pub salt: String,
    /// Base64-encoded per-session challenge.
    pub challenge: String,
}

/// Client authentication response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum AuthResponse {
    /// Answer to an [`AuthChallenge`] (password-based, local paths).
    Challenge {
        /// Base64-encoded challenge response.
        response: String,
    },
    /// Bearer token (PLAN.md §24 token authentication; TLS paths).
    Token {
        /// The configured token string.
        token: String,
    },
}

/// Optional client self-description for logs and session listings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Client software name (e.g. `"prismcast-cli"`).
    pub name: String,
    /// Client software version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Client session request. Until `Identified` is received the client must
/// not send anything else; a second `Identify` terminates the session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identify {
    /// Protocol version the client wants to speak.
    pub protocol_version: u32,
    /// Authentication response; required iff [`Hello`] carried a challenge
    /// or the server is configured with token auth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authentication: Option<AuthResponse>,
    /// Initial event subscriptions. `None` means the default set (all
    /// standard categories, no high-volume categories); an empty set means
    /// no events at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<SubscriptionSet>,
    /// Optional client self-description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientInfo>,
}

/// Server confirmation; the session is ready after this message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identified {
    /// The protocol version both sides will speak for this session.
    pub negotiated_protocol_version: u32,
    /// Unique session ID (used in server logs and `session_invalidated`
    /// flows).
    pub session_id: Uuid,
    /// Permissions granted to this session (PLAN.md §24 permission model).
    pub permissions: Vec<Permission>,
}

/// Authorization scopes attached to a session (PLAN.md §24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Read state, receive subscribed events.
    Read,
    /// Scene and scene-item commands.
    ControlScenes,
    /// Audio mixer/routing commands.
    ControlAudio,
    /// Output lifecycle commands (start/stop/reconfigure outputs).
    ControlOutputs,
    /// Profiles, scene collections, and global configuration.
    ModifyConfiguration,
    /// Full control; supersedes the other scopes.
    Admin,
}

/// Session termination reasons.
///
/// On WebSocket these map to application-private close codes (4000+ range,
/// like obs-websocket, RES-007 §Close codes); on Unix IPC the server sends a
/// final structured error and closes the connection with the same code in
/// the payload. Codes 4001 and 4003–4005 are deliberately left unused to
/// stay numerically compatible with obs-websocket's code space, easing the
/// future compatibility adapter (ADR-0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseCode {
    /// No specific reason.
    UnknownReason,
    /// A message could not be decoded (malformed JSON/MessagePack, wrong
    /// frame kind for the negotiated codec).
    MessageDecodeError,
    /// The `type` tag of a message is not known to this server.
    UnknownMessageType,
    /// The client sent traffic other than a single `Identify` before being
    /// identified.
    NotIdentified,
    /// The client sent `Identify` twice.
    AlreadyIdentified,
    /// Authentication failed or was missing.
    AuthenticationFailed,
    /// The server cannot honor the requested protocol version.
    UnsupportedProtocolVersion,
    /// The session was kicked by an administrator; clients must not
    /// auto-reconnect.
    SessionInvalidated,
    /// The session required a feature this server does not provide.
    UnsupportedFeature,
    /// The client's outbound queue overflowed (slow consumer); the server
    /// shed the session per the backpressure policy (see
    /// `docs/protocols/native-protocol.md` §Backpressure).
    SlowConsumer,
    /// The client exceeded the inbound request rate limit.
    RateLimited,
    /// The server is shutting down cleanly.
    ServerShutdown,
}

impl CloseCode {
    /// Returns the numeric close code.
    pub fn code(self) -> u16 {
        match self {
            Self::UnknownReason => 4000,
            Self::MessageDecodeError => 4002,
            Self::UnknownMessageType => 4006,
            Self::NotIdentified => 4007,
            Self::AlreadyIdentified => 4008,
            Self::AuthenticationFailed => 4009,
            Self::UnsupportedProtocolVersion => 4010,
            Self::SessionInvalidated => 4011,
            Self::UnsupportedFeature => 4012,
            Self::SlowConsumer => 4013,
            Self::RateLimited => 4014,
            Self::ServerShutdown => 4015,
        }
    }

    /// Maps a numeric close code back to a reason, if known.
    pub fn from_code(code: u16) -> Option<Self> {
        match code {
            4000 => Some(Self::UnknownReason),
            4002 => Some(Self::MessageDecodeError),
            4006 => Some(Self::UnknownMessageType),
            4007 => Some(Self::NotIdentified),
            4008 => Some(Self::AlreadyIdentified),
            4009 => Some(Self::AuthenticationFailed),
            4010 => Some(Self::UnsupportedProtocolVersion),
            4011 => Some(Self::SessionInvalidated),
            4012 => Some(Self::UnsupportedFeature),
            4013 => Some(Self::SlowConsumer),
            4014 => Some(Self::RateLimited),
            4015 => Some(Self::ServerShutdown),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_roundtrip() {
        let hello = Hello {
            prismcast_version: "0.1.0".into(),
            protocol_version: crate::version::PROTOCOL_VERSION,
            min_protocol_version: crate::version::MIN_PROTOCOL_VERSION,
            authentication: Some(AuthChallenge {
                salt: "c2FsdA==".into(),
                challenge: "Y2hhbGxlbmdl".into(),
            }),
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert_eq!(hello, serde_json::from_str(&json).unwrap());

        let identify = Identify {
            protocol_version: 1,
            authentication: Some(AuthResponse::Challenge {
                response: "cmVzcG9uc2U=".into(),
            }),
            subscriptions: None,
            client: Some(ClientInfo {
                name: "prismcast-cli".into(),
                version: Some("0.1.0".into()),
            }),
        };
        let json = serde_json::to_string(&identify).unwrap();
        assert_eq!(identify, serde_json::from_str(&json).unwrap());

        let identified = Identified {
            negotiated_protocol_version: 1,
            session_id: Uuid::new_v4(),
            permissions: vec![Permission::Read, Permission::ControlScenes],
        };
        let json = serde_json::to_string(&identified).unwrap();
        assert_eq!(identified, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn optional_fields_are_omitted() {
        let hello = Hello {
            prismcast_version: "0.1.0".into(),
            protocol_version: 1,
            min_protocol_version: 1,
            authentication: None,
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert!(!json.contains("authentication"));

        let identify = Identify {
            protocol_version: 1,
            authentication: None,
            subscriptions: None,
            client: None,
        };
        let json = serde_json::to_string(&identify).unwrap();
        assert_eq!(json, r#"{"protocol_version":1}"#);
    }

    #[test]
    fn close_code_roundtrip() {
        for code in [
            CloseCode::UnknownReason,
            CloseCode::MessageDecodeError,
            CloseCode::UnknownMessageType,
            CloseCode::NotIdentified,
            CloseCode::AlreadyIdentified,
            CloseCode::AuthenticationFailed,
            CloseCode::UnsupportedProtocolVersion,
            CloseCode::SessionInvalidated,
            CloseCode::UnsupportedFeature,
            CloseCode::SlowConsumer,
            CloseCode::RateLimited,
            CloseCode::ServerShutdown,
        ] {
            assert_eq!(CloseCode::from_code(code.code()), Some(code));
        }
        assert_eq!(CloseCode::from_code(4001), None);
        assert_eq!(CloseCode::from_code(9999), None);
    }
}

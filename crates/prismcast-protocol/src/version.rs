//! Protocol versioning and negotiation.
//!
//! The protocol version is an integer that is bumped **only on breaking
//! changes** (removed/renamed requests, changed field meanings, changed
//! handshake flow). Additive changes — new requests, new event variants, new
//! optional fields — never bump it; they are discovered via the
//! `get_version` request's `available_requests` list.
//!
//! Negotiation follows the obs-websocket pattern (RES-007 §RPC versioning):
//! the server advertises its supported range in [`crate::handshake::Hello`],
//! the client requests a version in [`crate::handshake::Identify`], and the
//! server answers with the negotiated version in
//! [`crate::handshake::Identified`].

/// The highest protocol version this implementation supports.
pub const PROTOCOL_VERSION: u32 = 1;

/// The lowest protocol version this implementation can still serve.
pub const MIN_PROTOCOL_VERSION: u32 = 1;

/// Negotiates a protocol version: the client requests `requested`, the
/// server supports `[server_min, server_max]`.
///
/// Returns `Some(min(requested, server_max))` when that value is within the
/// server's supported range, `None` otherwise (the server must then reject
/// the session with [`crate::handshake::CloseCode::UnsupportedProtocolVersion`]).
pub fn negotiate(requested: u32, server_min: u32, server_max: u32) -> Option<u32> {
    let negotiated = requested.min(server_max);
    (negotiated >= server_min).then_some(negotiated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_picks_the_lower_compatible_version() {
        assert_eq!(negotiate(1, 1, 1), Some(1));
        // Client newer than server: serve the server's best.
        assert_eq!(negotiate(5, 1, 2), Some(2));
        // Client older than the server still supports.
        assert_eq!(negotiate(1, 1, 3), Some(1));
        // Client too old for the server: reject.
        assert_eq!(negotiate(1, 2, 3), None);
        // Nonsense input: reject.
        assert_eq!(negotiate(0, 1, 1), None);
    }

    #[test]
    fn current_version_is_in_supported_range() {
        assert_eq!(
            negotiate(PROTOCOL_VERSION, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION),
            Some(PROTOCOL_VERSION)
        );
    }
}

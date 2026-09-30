//! [`StreamingServiceBackend`]: streaming-service operations that are
//! independent of a running output (PLAN.md §10–§11).
//!
//! Covers the OBS "service" concept: config validation, ingest endpoint
//! discovery, and connectivity/auth probing (bandwidth test). The actual
//! media path of a streaming output is [`crate::OutputBackend`]'s job.

use serde::{Deserialize, Serialize};

use prismcast_core::{OutputKind, Result, Service};

/// A resolved ingest endpoint for a service (e.g. one Twitch ingest server).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestEndpoint {
    /// Human-readable label (`"EU: Frankfurt"`, `"primary"`, ...).
    pub name: String,
    /// Full ingest URL.
    pub url: String,
    /// Whether the provider recommends this endpoint.
    pub recommended: bool,
}

/// Result of a live service probe (connectivity / auth / bandwidth test).
///
/// Fields are `Option` because not every protocol can answer every question
/// (e.g. SRT has no provider endpoint list; WHIP auth is a bearer token).
/// `latency_ms`/`bandwidth_kbps` are `None` when the probe did not get that
/// far. Never contains credentials.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServiceProbe {
    /// The server accepted a connection.
    pub reachable: bool,
    /// Credentials were accepted, if the probe reached authentication.
    pub auth_ok: Option<bool>,
    /// Round-trip time in milliseconds, if measured.
    pub latency_ms: Option<u32>,
    /// Measured uplink bandwidth in kbit/s, if a bandwidth test ran.
    pub bandwidth_kbps: Option<u32>,
    /// Human-readable detail (server name, protocol version, ...).
    pub details: String,
}

/// Operations on a streaming service configuration, independent of any
/// running output.
///
/// One implementation exists per protocol family (`rtmp`, `srt`, `whip`),
/// selected by [`StreamingServiceBackend::protocol`]; the backend reads the
/// concrete [`Service`] it is given, so per-service state (URL, key,
/// settings) is never baked into the backend instance.
///
/// Threading matches [`crate::SourceBackend`]: probes may block on network
/// I/O and must run on the media control actor's thread, not the Tokio
/// runtime.
pub trait StreamingServiceBackend: Send {
    /// The output kind (protocol family) this backend serves.
    fn protocol(&self) -> OutputKind;

    /// Validates a service configuration without network I/O (URL shape,
    /// required settings, key presence). Returns
    /// [`prismcast_core::Error::InvalidInput`] on mismatch.
    fn validate(&self, service: &Service) -> Result<()>;

    /// Resolves the provider's ingest endpoints for the service, if the
    /// protocol/provider has a discoverable list. Returns an empty list when
    /// the protocol has none (custom RTMP, SRT).
    fn endpoints(&self, service: &Service) -> Result<Vec<IngestEndpoint>>;

    /// Probes connectivity (and optionally auth/bandwidth) against the
    /// configured server. A failed probe is reported inside
    /// [`ServiceProbe`] (`reachable: false`), not as an `Err`; `Err` is
    /// reserved for probes that could not be attempted at all.
    fn probe(&self, service: &Service) -> Result<ServiceProbe>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_endpoint_serde_roundtrip() {
        let endpoint = IngestEndpoint {
            name: "EU: Frankfurt".to_string(),
            url: "rtmps://fra.contribute.live-video.net/app".to_string(),
            recommended: true,
        };
        let json = serde_json::to_string(&endpoint).unwrap();
        assert_eq!(endpoint, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn service_probe_serde_roundtrip() {
        let probe = ServiceProbe {
            reachable: true,
            auth_ok: Some(true),
            latency_ms: Some(24),
            bandwidth_kbps: Some(10_000),
            details: "rtmp2 handshake ok".to_string(),
        };
        let json = serde_json::to_string(&probe).unwrap();
        assert_eq!(probe, serde_json::from_str(&json).unwrap());
        assert!(!json.contains("live_"), "probe must never carry secrets");
    }
}

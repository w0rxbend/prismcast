# ADR-0022: Native WebSocket TLS (wss://) and bind hardening

Status: accepted for WS-003 (server-side slice).

## Context

WS-001 shipped the native WebSocket server plaintext-only: `ws://` on a plain
`TcpListener`, loopback by default, with a mandatory credential (token or
password challenge-response) as the network gate. That is acceptable for
same-machine control, but two things block any wider deployment: tokens and
challenge responses traverse the network in the clear on `ws://`, and nothing
stops an operator from binding a non-loopback address without encryption.
The protocol document already described `wss://`; the server must now catch
up. Constraints: no axum in `prismcast-remote` (ADR/WS-001), the session
engine is transport-generic, and the build must stay free of the aws-lc-rs
C build.

## Decision

**(a) rustls with the ring provider, terminated in `prismcast-remote` at the
accept loop, before the WebSocket upgrade.** `rustls` 0.23 is pinned with
`default-features = false` and `features = ["ring", "std", "tls12", "logging"]`
so the pure-Rust ring provider is selected explicitly — rustls 0.23 otherwise
defaults to aws-lc-rs, whose C build is rejected for this workspace.
`tokio-rustls` 0.26 provides the async `TlsAcceptor`/`TlsConnector`; the
tokio-tungstenite *server* TLS feature is deliberately not used — TLS is
terminated on the accepted `TcpStream` inside the per-connection task
(`acceptor.accept` bounded by a 10 s timeout so a plaintext client on a
`wss://` port fails fast without stalling other accepts), and the resulting
stream feeds the existing `accept_hdr_async_with_config` upgrade. The frame
reader/writer become generic over `S: AsyncRead + AsyncWrite + Unpin + Send +
'static` so plaintext and TLS connections share one code path; no axum, no
new server machinery.

**(b) Provided certificates only — no production self-signed generation.**
`WsTlsConfig { cert_path, key_path }` points at operator-supplied PEM files
(leaf-first chain; PKCS#8 or PKCS#1 private key). Generating a self-signed
server certificate at runtime is rejected for now: it needs persistence
(re-generating per start breaks pinning and confuses clients) and a trust
UX story, and is an explicit follow-up. Test certificates are generated with
`rcgen` (dev-dependency only) into tempdirs at test time and are never
committed.

**(c) Bind policy: non-loopback binds require TLS.** `WsServer::bind` fails
with a typed `WsError::TlsRequired` when `config.bind` is not loopback and
`config.tls` is `None`. Loopback plaintext `ws://` stays valid — it is the
default and the common same-machine case. The `AuthConfig::AllowLocal`
rejection (credential mandatory on a network transport) is unchanged and
applies to `wss://` exactly as to `ws://`: TLS protects the channel, not
authorization.

**(d) Client trust model: native roots + optional extra CA + explicit
danger-insecure.** `ClientTlsConfig` builds client roots from the system
native store (`rustls-native-certs`; a partially unreadable store logs a
warning and continues with what loaded — never a hard fail), plus an optional
operator-provided CA bundle PEM for private/self-signed deployments, plus an
explicit `danger_accept_invalid_certs` switch that is warn-logged on use.
The CLI gains `--url` / `--tls-ca` / `--insecure` flags in later slices; this
slice lands the library surface only.

**(e) The obs-websocket adapter stays plaintext.** Upstream obs-websocket
has no TLS mode in its protocol contract; the ecosystem pattern is a
terminating reverse proxy in front of it. The bind-hardening policy (c)
applies to the native server only.

## Consequences

- `prismcast-remote` gains `rustls` (ring), `tokio-rustls`, `rustls-pemfile`,
  and `rustls-native-certs` dependencies; `rcgen` as a dev-dependency. No
  aws-lc-rs anywhere in the tree (cargo-deny enforced).
- The wire protocol is unchanged: `wss://` is the same JSON text framing and
  handshake after the TLS record layer; the protocol document's TLS wording
  is now accurate instead of aspirational.
- `remote.toml` is not extended (its parser rejects unknown keys; it is the
  auth file). TLS configuration is programmatic (`WsServerConfig::tls`) until
  a configuration-schema slice wires it.
- Runtime self-signed generation, CLI TLS flags, and ws_client TLS wiring are
  follow-ups on this ADR's foundation.

## Alternatives considered

Terminating TLS in tokio-tungstenite via its `rustls` server feature was
rejected: it couples TLS to the WS handshake helper, hides the
accept-timeout policy we need for plaintext-on-TLS-port robustness, and pulls
webpki root selection into a dependency feature instead of our explicit
trust model (d). A reverse-proxy-only stance (nginx/haproxy in front of
plaintext loopback) was rejected as the *only* answer: it offloads our core
security invariant onto deployment trivia and cannot be enforced by the
binary, whereas policy (c) is enforced at `bind`. native-tls (OpenSSL) was
rejected: it reintroduces a C dependency and platform-specific behavior the
rustls+ring stack avoids.

# ADR-0006: Unix domain socket IPC with length-prefixed MessagePack/CBOR and an explicit protocol version

## Context

Prismcast needs fast local control: the CLI (`prismcast`, studioctl-style, PLAN §25) and
out-of-process plugins (ADR-0009) must talk to the running application with low latency
(control p95 < 10 ms, PLAN §56), without initializing GTK or GStreamer in the client
(PLAN §25). Being Linux-only (ADR-0001) permits using Unix-domain-socket semantics
directly — filesystem permissions, `$XDG_RUNTIME_DIR` placement, systemd socket
activation — instead of a portable TCP loopback with its own authentication problem.

The wire format must be compact, schema-evolvable, and cheap to encode/decode in Rust and
in plugin processes written in other languages.

## Decision

1. **Primary local IPC is a Unix domain socket** at
   `$XDG_RUNTIME_DIR/prismcast/control.sock` (PLAN §21).
2. **Framing is length-prefixed**; each message is a **MessagePack** document (CBOR is
   the designated fallback if MessagePack proves problematic, per PLAN §21's "or").
   Both are binary, self-describing, and have mature cross-language implementations.
3. **Every connection begins with an explicit protocol version** handshake; mismatched
   versions are rejected with a typed error rather than silently mis-parsed.
4. The payload is the versioned protocol defined in `prismcast-protocol`: the same
   Command/Query/Event contract as every other frontend (ADR-0005). Protocol structs are
   distinct from domain structs (PLAN §75).
5. Access control uses filesystem permissions on the socket (PLAN §21); a D-Bus adapter
   may be added later for desktop integration, as an adapter, not a replacement.
6. The CLI speaks only this protocol; it must not initialize GTK or GStreamer
   (PLAN §25).

## Alternatives

- **TCP loopback + JSON.** Rejected as primary: requires port management and token auth
  even for local control, and JSON is slower/larger than a binary format for meter and
  statistics traffic. (WebSocket/JSON exists for remote clients, PLAN §22 — that is the
  network-facing path, not the local one.)
- **D-Bus as the primary IPC.** Rejected as primary: higher latency, session-bus
  lifecycle coupling, poor fit for high-frequency event streams, and awkward for
  non-desktop plugin processes. Retained as an optional later adapter (PLAN §21).
- **gRPC/Protobuf over the socket.** Rejected: schema toolchain and codegen overhead for
  marginal benefit; MessagePack/CBOR over a versioned handshake is sufficient and simpler
  for polyglot plugins.
- **Shared memory / zero-copy control channel.** Rejected: needless complexity at the
  control plane; the 10 ms p95 budget is comfortably met by a UDS round trip.

## Consequences

- The CLI, plugins, and systemd units share one local contract; socket-activation
  friendliness comes free (PLAN §21 advantages).
- Protocol evolution is governed: wire schema lives in `prismcast-protocol`, versioned,
  with golden serialization tests (PLAN §63).
- Message size limits and bounded channels apply (PLAN §75) to prevent a misbehaving
  client from exhausting memory.
- Two agents must never change the protocol schema concurrently (PLAN §74).

## Evidence

- PLAN.md §21 (IPC: Unix domain socket at `$XDG_RUNTIME_DIR/<app>/control.sock`;
  length-prefixed MessagePack or CBOR with explicit protocol version; advantages list;
  optional D-Bus adapter).
- PLAN.md §25 (CLI talks through IPC and must not initialize GTK/GStreamer).
- PLAN.md §56 (IPC command p95 < 10 ms), §63 (golden tests for protocol messages),
  §75 (no protocol structs reused as domain structs).

## Status

Accepted (2026-09-30)

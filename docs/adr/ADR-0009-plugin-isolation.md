# ADR-0009: Plugin isolation — staged registry → out-of-process IPC → WASM; no native .so ABI

## Context

Prismcast wants extensible sources, filters, encoders, services, outputs, automation, and
UI extensions (PLAN §1, §26). The tempting default for a Rust app is dynamically loaded
native `.so` plugins, but Rust has no stable ABI: a plugin compiled against a different
rustc or dependency graph is UB-adjacent, and a plugin crash takes down the broadcaster
mid-stream. PLAN §26 explicitly says: do not start with dynamically loaded native `.so`
Rust plugins; out-of-process plugins are safer than an unstable Rust dynamic ABI.
PLAN §55 adds: prefer stable message protocols over Rust ABI.

Plugin failure is an enumerated failure case ("plugin process dies", PLAN §61), which the
architecture must survive by construction, not by convention.

## Decision

Plugins are developed in **three isolation stages**, each building on the last
(PLAN §26):

1. **Stage 1 — built-in Rust registry.** Extension categories are defined as stable
   traits and manifests in `prismcast-plugin-sdk`: `SourceProvider`, `FilterProvider`,
   `OutputProvider`, `EncoderProvider`, `ServiceProvider`, `AutomationProvider`,
   `UIExtension`. First-party functionality ships through the same registry, so the
   extension interface is exercised from day one.
2. **Stage 2 — out-of-process plugins over IPC.** Third-party plugins run as separate
   processes speaking the versioned protocol (ADR-0006 framing/versioning discipline).
   A plugin process crash is contained: the host marks the plugin Failed and keeps
   running (PLAN §61). Capability negotiation and API versioning are part of the plugin
   handshake (PLAN §55).
3. **Stage 3 — WASM plugin ABI where applicable.** For sandboxed, CPU-bound logic
   (filters, automation), a WASM component model may be introduced; investigated, not
   committed (PLAN §55 lists WASM plugins, Rust native plugins, Lua scripting as
   follow-up investigation).

**Explicitly excluded**: no native `.so` plugin ABI. Not now, and not as a compatibility
promise later.

## Alternatives

- **Native dynamic loading (`libloading`, `.so`).** Rejected: unstable Rust ABI, unsafe
  failure containment, deployment fragility — exactly the "plugin ABI constraints" PLAN
  §79 refuses to inherit from OBS's ecosystem.
- **WASM-first.** Rejected as the starting point: the WASM component tooling for rich
  media data (frames, DMABUF) is immature; WASM is viable for logic plugins but not as
  the universal first mechanism.
- **Embedded scripting (Lua/Python) as the primary model.** Rejected as primary: fine as
  a later investigation (PLAN §55), but a scripting runtime does not cover encoder/source
  integration depth and adds a second security surface.
- **In-process Rust plugins behind a "stable C ABI" crate boundary.** Rejected: still
  shares fate with the host and imposes FFI ergonomics on plugin authors; IPC achieves
  real isolation at acceptable cost for control-plane and many data-plane uses.

## Consequences

- Plugin data-plane performance (e.g. video filters) is bounded by IPC transport; Stage 2
  design must include shared-memory/DMABUF buffer passing for frame data, or defer
  frame-rate plugins to Stage 3 WASM/in-tree. This is a known research area, not a
  resolved detail.
- The SDK's provider traits and manifests are the long-lived contract; they version
  independently of the host.
- Plugin processes get the same auth/permission discipline as remote clients
  (PLAN §24) — a plugin is not implicitly trusted with Admin.
- Host resilience is testable: killing a plugin process must degrade, not crash
  (PLAN §61).

## Evidence

- PLAN.md §26 (Plugin architecture: no native `.so` start; provider categories; the three
  stages; "Out-of-process plugins are safer than unstable Rust dynamic ABI").
- PLAN.md §55 (Phase 13: manifest, discovery, process, IPC, capability negotiation, API
  versioning; "Prefer stable message protocols over Rust ABI").
- PLAN.md §61 (plugin process death as a designed failure case), §79 (plugin ABI
  constraints deliberately not copied from OBS).

## Status

Accepted (2026-09-30)

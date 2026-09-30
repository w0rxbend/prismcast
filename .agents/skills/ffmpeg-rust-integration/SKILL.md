---
name: ffmpeg-rust-integration
description: Integrate FFmpeg or FFprobe subprocesses and libav Rust bindings for media probing, remuxing, codec research, hardware acceleration experiments, and recording validation in Prismcast.
---

# FFmpeg integration in Rust

Keep GStreamer as the primary media engine per PLAN.md. Use FFmpeg/FFprobe for bounded tools, output validation and justified specialized backends. A backend change requires the project's architecture decision workflow.

Inspect ffmpeg/ffprobe versions, build configuration, installed encoders/filters/protocols and runtime hardware. Match libav headers, runtime libraries and Rust binding support when using FFI. Consult the installed version's documentation; online FFmpeg documentation follows development revisions.

## Subprocess boundary

- Pass arguments individually through std::process::Command or tokio::process::Command; do not construct a shell string from filenames or settings.
- Consume FFprobe JSON into dedicated validated wire types. Preserve rational time bases and represent absent or unknown metadata explicitly. Cap capture size and duration; stdout/stderr can exhaust memory or deadlock a child if not drained.
- Give each process an owner, cancellation policy and deadline. Cancellation of the calling future alone may leave the child running. Reap children and distinguish spawn, exit, timeout, decode and verification errors.
- Protect credentials in command logging. Bound concurrent probes/transcodes. Keep blocking process waits off Tokio and the GTK thread.
- Make stream mapping explicit. Stream copy avoids re-encoding but does not guarantee container compatibility or frame-accurate cuts. Preserve source files, render to a fresh output and verify before replacing a destination through the persistence policy.
- Stop recording/remux processes gracefully when finalization is needed; killing a process can leave an incomplete container. Bound the graceful shutdown wait.

## libav boundary

Document ownership and cleanup of contexts, packets, frames, buffers and callback data. Use a maintained safe wrapper where suitable. Audit every unsafe operation and callback for lifetime, error, thread and unwinding assumptions.

Follow the version-matched send/receive API contract, including EAGAIN, draining and EOF. Rescale timestamps between stream and codec time bases and preserve ordering/reordering semantics. Hardware frames require explicit device/context and transfer handling; measure CPU transfers.

Validate recordings with probe output plus decoding checks and relevant playback tests. Synthetic fixtures should cover multiple audio tracks, VFR, keyframe boundaries, nonzero start times and truncated outputs. Use the installed ffmpeg skill for CLI command/filter details.

## Official references

- [FFmpeg documentation and versioned API links](https://ffmpeg.org/documentation.html)
- [FFmpeg CLI](https://ffmpeg.org/ffmpeg.html)
- [FFprobe](https://ffmpeg.org/ffprobe.html)
- [Rust process API](https://doc.rust-lang.org/std/process/struct.Command.html)
- [Tokio process](https://docs.rs/tokio/latest/tokio/process/)

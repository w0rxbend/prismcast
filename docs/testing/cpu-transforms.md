# CPU transform verification

Run `cargo test -p prismcast-compositor` for pure geometry and
`cargo test -p prismcast-media-gst transform_pixel` for actual native pixels.
GStreamer >=1.26 needs coreelements, videotestsrc/compositor (plugins-base),
and videocrop/videoflip (plugins-good). Tests run without a display or device.

Pure tests cover all nine anchors, normalized huge crops without overflow,
cardinal orientation before nonuniform canvas scaling, negative/zero scales,
raw subpixel placement before final native extent clamping, all bounds modes,
alignment, half-away rounding, nonfinite/resource failures and source defaults.

Native pad probes capture bounded RGBA frame samples from asymmetric SMPTE
bars. Sixteen crop+cardinal+signed-flip combinations map static original source
pixels to expected output locations, including vertical flips that move static
upper-bar pixels into a different image region. A combined90°+horizontal flip+
unequal scales case verifies the explicit transform order. Solid red pixels
separately verify bounds alignment/inner/outer/stretch edges and1×1 clamped
crop. Consecutive fresh matching frames prevent accepting old queued samples.

Other checks prove one shared source instance, complete placement-filter and
request-pad cleanup, once-per-item quantization diagnostics, and preservation
of an existing native graph after invalid transform input. Existing media owner,
canvas, queue and terminal-event regressions remain part of the full suite.
This CPU correctness prototype does not implement free-angle rotation,
advanced blends, source filters, or uninterrupted topology changes.

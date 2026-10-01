# Source toggle signal regression

Run on a real display in a separate process from other GTK tests:

```bash
cargo test -p prismcast-ui rapid_visible_and_locked_toggles_keep_final_signal_value -- --ignored --test-threads=1
```

This ignored display test initializes real GTK CheckButtons and connects the
same helper used by source placement rows. It toggles each control twice before
dispatching or refreshing any snapshot, then sends all four captured commands
to the real application actor. The committed scene item must retain the final
visibility and lock values. This catches callbacks that reuse the inverse of
the originally rendered snapshot for every signal. Snapshot rendering sets the
initial checkbox state before connecting signals, preventing feedback commands.

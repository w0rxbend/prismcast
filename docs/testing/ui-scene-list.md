# Scene list verification

UI-003 keeps scene order and current scene in committed application snapshots.
Row selection captures SceneId before queuing its message. Snapshot restoration
suppresses selection signals synchronously, preventing accidental scene changes.
Rename, remove and up/down actions dispatch existing core commands through the
root component; rejected commands use its existing error toast.

Manual display checks:

1. Start `cargo run -p prismcast-ui --bin prismcast` in a supported GTK session.
2. With no scenes, verify the empty hint and Add button. Add two scenes and
   select each by mouse and keyboard; verify the committed selection highlight.
3. Tab to Rename, enter a long name, press Enter, and verify ellipsis and tooltip.
   A blank or whitespace-only name must keep Rename disabled.
4. Move scenes up/down and verify committed order. Boundary moves must do nothing.
5. Remove a scene and verify its rows/items disappear after the core event.
6. Change scenes via another controller and verify selection/order synchronize.
7. Close the window and verify orderly process exit without GTK warnings.

The reorder boundary/stale-ID logic has a focused unit test. Compiler and test
checks do not establish display behavior. This task did not perform a real
window smoke test, although DISPLAY=:0 and WAYLAND_DISPLAY=wayland-0 are available.

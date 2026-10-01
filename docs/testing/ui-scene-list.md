# Scene list verification

UI-003 keeps scene order and current scene in committed application snapshots.
Row selection captures SceneId before queuing its message. Snapshot restoration
suppresses selection signals synchronously, preventing accidental scene changes.
Rename, remove and up/down actions dispatch existing core commands through the
root component; rejected commands use its existing error toast.

Interaction contract:

- Selection → `SetCurrentScene`; external controllers (CLI/WS) move the UI
  selection only via the event → snapshot refresh path.
- Rename opens an `adw::Dialog` with the current name preselected. Blank or
  whitespace-only names keep "Rename" disabled; Enter confirms. Duplicate
  names are not pre-judged — core rejections surface as toasts.
- Removal is gated by an `adw::AlertDialog` (default/close = Cancel, Remove is
  destructive-styled). The `Delete` key on a selected row opens the same
  confirmation, so no single keypress destroys a scene. Core rejections
  (last scene, scene referenced by studio mode or a scene source) surface as
  toasts from the shared dispatcher.
- Reordering uses up/down row buttons → `ReorderScene`; boundary buttons are
  insensitive. The list order always re-renders from the committed snapshot.
- With zero scenes the list shows an `adw::StatusPage` placeholder ("No
  Scenes / Add a scene to begin.") via `GtkListBox:placeholder`.
- Long names ellipsize (`max-width-chars` + end ellipsize); the full name is
  the row label tooltip.

Manual display checks:

1. Start `cargo run -p prismcast-ui --bin prismcast` in a supported GTK session.
2. With no scenes, verify the StatusPage empty state and Add button. Add two
   scenes and select each by mouse and keyboard; verify the committed
   selection highlight.
3. Rename via the row button: the current name is focused and preselected.
   Enter confirms; a blank name keeps Rename disabled. Enter a long name and
   verify ellipsis and tooltip.
4. Remove via the row button and via the Delete key: both must show the
   confirmation dialog; Cancel (and Esc) must do nothing. On a single-scene
   list, confirming Remove must produce a "cannot remove the last scene"
   toast and leave the scene intact.
5. Move scenes up/down and verify committed order. First/last rows must have
   the up/down button insensitive respectively.
6. Change scenes via another controller (CLI/WS) and verify selection/order
   synchronize without any local UI mutation.
7. Close the window and verify orderly process exit without GTK warnings.

The reorder boundary/stale-ID logic and the name-trimming validation have
focused unit tests. Compiler and test checks do not establish display
behavior. This task did not perform a real window smoke test, although
DISPLAY=:0 and WAYLAND_DISPLAY=wayland-0 are available.

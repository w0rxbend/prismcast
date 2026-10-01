# Scene list verification

UI-003 keeps scene order/current scene in committed application snapshots.
Selection captures SceneId before queuing; restoration guards suppress echo
commands. Rename, confirmed removal and up/down actions route existing core
Commands through the root; rejections appear in its toast.

Scene renaming uses a labeled “Scene name” EntryRow, focuses the editor and
preselects the existing name. Blank/whitespace input disables Rename, and Enter
uses the same validation as the button. Boundary reorder buttons are disabled.
Remove opens a confirmation with Cancel as default/close response because the
current core has no removal undo inverse. Delete on a selected scene opens the
same confirmation; it does not dispatch removal directly.

Manual display checks:

1. Add two scenes; select by mouse/keyboard and verify committed highlighting.
2. Rename: confirm focus/preselection, blank rejection, Enter submission,
   ellipsis and full-name tooltip for long names.
3. Check first/last reorder buttons are disabled; other moves update order.
4. Remove by row button and Delete; Cancel/Esc leave scene intact. Confirming
   removal on the last scene shows the core rejection toast.
5. Change scenes through another controller and check snapshot synchronization.
6. Close and verify orderly exit. Focus restoration after list rebuilds remains
   a later refinement.

Run the actual display signal regression separately from other GTK tests:

```bash
cargo test -p prismcast-ui scene_dialog_signals_validate_names_and_require_explicit_removal -- --ignored --test-threads=1
```

It exercises the production dialog builders and presentation helper: labeled
entry selection/focus, blank Enter rejection, valid Enter rename, Cancel default
and explicit Remove response. It complements the headless reorder boundary and
stale-ID test; it does not automate keyboard event synthesis.

# Source list verification

UI-004 separates placements in the selected scene from the shared source
registry. Visibility, lock and removal address SceneItemId in that scene;
rename addresses SourceId and affects every reference. Removing a placement
keeps its shared source. Shared deletion remains subject to core reference
checks and displays a rejection toast when it cannot proceed.

Add Source requires a selected scene. The dialog captures that SceneId when
opened. Creation dispatches AddSource, reads SourceId from the committed
SourceEvent::Added response, then dispatches AddSceneItem into the captured
scene even if selection changed meanwhile. If placement fails, the source
remains registered, a toast explains partial success, and the Shared sources
Place button allows recovery. No implicit deletion or rollback occurs.

Automated tests verify committed ID extraction and missing creation events,
captured target placement after current-scene switching, and shared source
survival after rejected placement into a removed scene.

Manual display checks:

1. Without a scene, Add Source shows guidance. Add a scene and a test pattern;
   verify one shared source and one placement appear.
2. Place the same source again, then in a second scene. Toggle visibility/lock
   on one placement and verify the other references stay independent.
3. Rename a placed source and verify its shared name changes across scenes.
4. Remove one placement, verify the source remains, and place it again.
5. Delete a referenced shared source and verify the error toast; remove all
   references and verify shared deletion succeeds.
6. Open Add Source, change selected scene externally, then submit; placement
   must use the scene captured at dialog opening. Remove that captured scene
   before submitting to exercise partial success and recover via Place.
7. Verify keyboard focus and Enter rename submission, long-name tooltips,
   empty hints, and snapshot synchronization after remote commands.

DISPLAY=:0 and WAYLAND_DISPLAY=wayland-0 are available, but no real window
smoke test was performed for this task.

//! End-to-end proof that the architecture works headless (CORE-001/002/003
//! acceptance): command in → events out → snapshot reflects state, with
//! multiple interchangeable controllers sharing one `AppHandle`.

use std::time::Duration;

use tokio::time::timeout;

use prismcast_app::{
    AppHandle, CommandResponse, CoreConfig, EventFilter, HandleError, Permission, Permissions,
    Query, QueryResponse, StreamEvent,
};
use prismcast_core::{
    Command, Error, Event, SceneEvent, SceneId, SceneItemId, SourceEvent, SourceId, SourceKind,
    Transform, Vec2,
};

const RECV_TIMEOUT: Duration = Duration::from_secs(5);

async fn dispatch_ok(handle: &AppHandle, command: Command) -> CommandResponse {
    handle
        .dispatch(command)
        .await
        .expect("dispatch should succeed")
}

async fn recv_event(stream: &mut prismcast_app::EventStream) -> (u64, Event) {
    match timeout(RECV_TIMEOUT, stream.recv())
        .await
        .expect("recv timed out")
    {
        Some(StreamEvent::Event { seq, event }) => (seq, event),
        other => panic!("expected event, got {other:?}"),
    }
}

fn added_scene_id(response: &CommandResponse) -> SceneId {
    match &response.events[0] {
        Event::Scene(SceneEvent::Added { scene_id, .. }) => *scene_id,
        other => panic!("expected SceneEvent::Added, got {other:?}"),
    }
}

fn added_source_id(response: &CommandResponse) -> SourceId {
    match &response.events[0] {
        Event::Source(SourceEvent::Added { source }) => source.id,
        other => panic!("expected SourceEvent::Added, got {other:?}"),
    }
}

fn added_item_id(response: &CommandResponse) -> SceneItemId {
    match &response.events[0] {
        Event::Scene(SceneEvent::ItemAdded { item, .. }) => item.id,
        other => panic!("expected SceneEvent::ItemAdded, got {other:?}"),
    }
}

fn move_to(x: f32, y: f32) -> Transform {
    Transform {
        position: Vec2::new(x, y),
        ..Transform::default()
    }
}

fn item_position(handle: &AppHandle, scene_id: SceneId, item_id: SceneItemId) -> Vec2 {
    handle
        .snapshot()
        .scene(scene_id)
        .and_then(|scene| scene.item(item_id))
        .map(|item| item.transform.position)
        .expect("item should exist in snapshot")
}

/// Acceptance: command in → events out → snapshot reflects state.
#[tokio::test]
async fn command_events_snapshot_flow_end_to_end() {
    let app = AppHandle::spawn(CoreConfig::default());
    let mut events = app.subscribe(EventFilter::all());

    let response = dispatch_ok(
        &app,
        Command::AddScene {
            name: "Main".into(),
        },
    )
    .await;
    let scene_id = added_scene_id(&response);
    // First scene also becomes current: two committed events in the reply.
    assert_eq!(response.events.len(), 2);

    // Events are broadcast with global sequence numbers in apply order.
    let (seq0, event0) = recv_event(&mut events).await;
    let (seq1, event1) = recv_event(&mut events).await;
    assert_eq!((seq0, seq1), (0, 1));
    assert!(matches!(event0, Event::Scene(SceneEvent::Added { .. })));
    assert!(matches!(
        event1,
        Event::Scene(SceneEvent::CurrentChanged { scene_id: id }) if id == scene_id
    ));

    // The snapshot reflects the committed state.
    let snapshot = app.snapshot();
    assert_eq!(snapshot.revision(), 1);
    assert_eq!(
        snapshot.scene(scene_id).map(|s| s.name.as_str()),
        Some("Main")
    );
    assert_eq!(snapshot.current_scene(), Some(scene_id));

    // Read-only queries resolve from the snapshot without the command queue.
    match app
        .query(&Query::GetScene { scene_id }, Permissions::read_only())
        .expect("query")
    {
        QueryResponse::Scene(Some(scene)) => assert_eq!(scene.name, "Main"),
        other => panic!("unexpected {other:?}"),
    }

    app.shutdown().await;
}

/// Acceptance: event ordering preserved per subscriber; two subscribers both
/// receive everything.
#[tokio::test]
async fn two_subscribers_receive_identical_ordered_streams() {
    let app = AppHandle::spawn(CoreConfig::default());
    let mut first = app.subscribe(EventFilter::all());
    let mut second = app.subscribe(EventFilter::all());

    let scene = added_scene_id(&dispatch_ok(&app, Command::AddScene { name: "s".into() }).await);
    for name in ["alpha", "beta", "gamma"] {
        dispatch_ok(
            &app,
            Command::AddSource {
                kind: SourceKind::Color,
                name: name.into(),
            },
        )
        .await;
    }
    dispatch_ok(&app, Command::SetCurrentScene { scene_id: scene }).await;

    // 2 (add scene) + 3 (sources) events; SetCurrentScene is a no-op (scene
    // already current) and emits none.
    let mut a = Vec::new();
    let mut b = Vec::new();
    for _ in 0..5 {
        a.push(recv_event(&mut first).await);
        b.push(recv_event(&mut second).await);
    }
    assert_eq!(a, b, "both subscribers see the identical stream");
    let seqs: Vec<u64> = a.iter().map(|(seq, _)| *seq).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3, 4], "global order preserved");

    app.shutdown().await;
}

/// Acceptance: slow-consumer policy end-to-end through the actor — drop
/// oldest, one coalesced `Lagged` notice, fast subscriber unaffected.
#[tokio::test]
async fn slow_consumer_policy_through_the_actor() {
    let app = AppHandle::spawn(CoreConfig::default());
    let mut slow = app.subscribe_with_capacity(EventFilter::all(), 2);
    let mut fast = app.subscribe(EventFilter::all());

    for index in 0..5 {
        dispatch_ok(
            &app,
            Command::AddSource {
                kind: SourceKind::Color,
                name: format!("src{index}"),
            },
        )
        .await;
        // Fast consumer drains as we go.
        let (seq, _) = recv_event(&mut fast).await;
        assert_eq!(seq, index as u64);
    }

    // Slow consumer: capacity 2, 5 events published → the queue holds only a
    // coalesced Lagged notice plus the newest event; 4 were dropped.
    let lagged = timeout(RECV_TIMEOUT, slow.recv())
        .await
        .expect("recv timed out")
        .expect("stream open");
    assert!(
        matches!(lagged, StreamEvent::Lagged { dropped: 4 }),
        "expected Lagged(4), got {lagged:?}"
    );
    let (seq, event) = recv_event(&mut slow).await;
    assert_eq!(seq, 4);
    assert!(matches!(event, Event::Source(SourceEvent::Added { .. })));

    // Queue is drained; shutdown closes the stream.
    app.shutdown().await;
    assert_eq!(
        timeout(RECV_TIMEOUT, slow.recv())
            .await
            .expect("recv timed out"),
        None
    );
}

/// Acceptance: unauthorized commands are rejected without state mutation.
#[tokio::test]
async fn unauthorized_command_rejected_without_state_mutation() {
    let app = AppHandle::spawn(CoreConfig::default());
    let mut events = app.subscribe(EventFilter::all());

    let scene = added_scene_id(
        &dispatch_ok(
            &app,
            Command::AddScene {
                name: "Main".into(),
            },
        )
        .await,
    );
    let revision_before = app.snapshot().revision();
    // Drain the AddScene events so the queue is empty.
    recv_event(&mut events).await;
    recv_event(&mut events).await;

    // A session with only ControlAudio may not rename scenes.
    let audio_only = Permissions::from_iter([Permission::ControlAudio]);
    let error = app
        .dispatch_with_permissions(
            Command::RenameScene {
                scene_id: scene,
                name: "Hijacked".into(),
            },
            audio_only,
        )
        .await
        .expect_err("must be rejected");
    assert!(matches!(error, HandleError::Core(Error::Unauthorized(_))));

    // No state mutation, no new snapshot revision, no event broadcast.
    let snapshot = app.snapshot();
    assert_eq!(snapshot.revision(), revision_before);
    assert_eq!(snapshot.scene(scene).map(|s| s.name.as_str()), Some("Main"));
    let unexpected = timeout(Duration::from_millis(200), events.recv()).await;
    assert!(
        unexpected.is_err(),
        "no event may be broadcast for rejected commands"
    );

    // Queries also require Read.
    assert!(matches!(
        app.query(&Query::ListScenes, Permissions::none()),
        Err(Error::Unauthorized(_))
    ));

    app.shutdown().await;
}

/// Acceptance: snapshot reads during command processing don't deadlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reads_and_writes_do_not_deadlock() {
    let app = AppHandle::spawn(CoreConfig::default());
    let scene = added_scene_id(&dispatch_ok(&app, Command::AddScene { name: "s".into() }).await);

    let mut readers = Vec::new();
    for _ in 0..4 {
        let app = app.clone();
        readers.push(tokio::spawn(async move {
            for _ in 0..200 {
                let snapshot = app.snapshot();
                std::hint::black_box(snapshot.revision());
                std::hint::black_box(
                    app.query(&Query::ListScenes, Permissions::read_only())
                        .expect("query"),
                );
                std::hint::black_box(
                    app.query(
                        &Query::GetScene { scene_id: scene },
                        Permissions::read_only(),
                    )
                    .expect("query"),
                );
                tokio::task::yield_now().await;
            }
        }));
    }
    let mut writers = Vec::new();
    for index in 0..2 {
        let app = app.clone();
        writers.push(tokio::spawn(async move {
            for step in 0..50 {
                app.dispatch(Command::RenameScene {
                    scene_id: scene,
                    name: format!("scene-{index}-{step}"),
                })
                .await
                .expect("write");
            }
        }));
    }

    timeout(Duration::from_secs(30), async {
        for task in readers {
            task.await.expect("reader");
        }
        for task in writers {
            task.await.expect("writer");
        }
    })
    .await
    .expect("deadlock detected: timed out");

    assert_eq!(app.snapshot().revision(), 101); // initial add + 100 renames
    app.shutdown().await;
}

/// PLAN §59: undo/redo roundtrip through the actor (move item, undo, redo).
#[tokio::test]
async fn undo_redo_roundtrip_via_actor() {
    let app = AppHandle::spawn(CoreConfig::default());
    let mut events = app.subscribe(EventFilter::all());

    let scene = added_scene_id(&dispatch_ok(&app, Command::AddScene { name: "s".into() }).await);
    let source = added_source_id(
        &dispatch_ok(
            &app,
            Command::AddSource {
                kind: SourceKind::Color,
                name: "cam".into(),
            },
        )
        .await,
    );
    let item = added_item_id(
        &dispatch_ok(
            &app,
            Command::AddSceneItem {
                scene_id: scene,
                source_id: source,
            },
        )
        .await,
    );

    // Move the item (reversible).
    dispatch_ok(
        &app,
        Command::SetSceneItemTransform {
            scene_id: scene,
            item_id: item,
            transform: move_to(10.0, 20.0),
        },
    )
    .await;
    assert_eq!(item_position(&app, scene, item), Vec2::new(10.0, 20.0));

    // Undo restores the pre-move transform and emits normal events.
    let undone = app.undo().await.expect("undo");
    assert_eq!(item_position(&app, scene, item), Vec2::new(0.0, 0.0));
    assert!(undone
        .events
        .iter()
        .any(|e| matches!(e, Event::Scene(SceneEvent::ItemUpdated { .. }))));

    // Redo reapplies the move.
    app.redo().await.expect("redo");
    assert_eq!(item_position(&app, scene, item), Vec2::new(10.0, 20.0));

    // Known limitation (ARCH-002): Add* are irreversible, so after undoing
    // the move there is nothing further to undo.
    app.undo().await.expect("undo the move again");
    let error = app.undo().await.expect_err("nothing left to undo");
    assert!(matches!(error, HandleError::Core(Error::InvalidInput(_))));

    // Undo/redo went through the broadcaster like any other command.
    let mut saw_updates = 0;
    while let Ok(Some(item)) = timeout(Duration::from_millis(100), events.recv()).await {
        if matches!(
            item,
            StreamEvent::Event {
                event: Event::Scene(SceneEvent::ItemUpdated { .. }),
                ..
            }
        ) {
            saw_updates += 1;
        }
    }
    assert_eq!(
        saw_updates, 4,
        "move + undo + redo + undo each emitted ItemUpdated"
    );

    app.shutdown().await;
}

/// PLAN §59: a drag gesture (begin/end transaction) is one undo entry.
#[tokio::test]
async fn transaction_group_coalesces_drag_into_one_undo_entry() {
    let app = AppHandle::spawn(CoreConfig::default());
    let scene = added_scene_id(&dispatch_ok(&app, Command::AddScene { name: "s".into() }).await);
    let source = added_source_id(
        &dispatch_ok(
            &app,
            Command::AddSource {
                kind: SourceKind::Color,
                name: "cam".into(),
            },
        )
        .await,
    );
    let item = added_item_id(
        &dispatch_ok(
            &app,
            Command::AddSceneItem {
                scene_id: scene,
                source_id: source,
            },
        )
        .await,
    );

    app.begin_transaction("drag item").await.expect("begin");
    for step in 1..=3 {
        dispatch_ok(
            &app,
            Command::SetSceneItemTransform {
                scene_id: scene,
                item_id: item,
                transform: move_to(step as f32, step as f32),
            },
        )
        .await;
    }
    app.end_transaction().await.expect("end");
    assert_eq!(item_position(&app, scene, item), Vec2::new(3.0, 3.0));

    // One undo reverses the whole drag.
    app.undo().await.expect("undo drag");
    assert_eq!(item_position(&app, scene, item), Vec2::new(0.0, 0.0));

    app.redo().await.expect("redo drag");
    assert_eq!(item_position(&app, scene, item), Vec2::new(3.0, 3.0));

    app.shutdown().await;
}

/// PLAN §67 (steps 2–8, domain side): sources created and composed, the item
/// moved by "GTK", "CLI", and "WS" controllers — three clones of one
/// `AppHandle` — and every subscriber observes every event.
#[tokio::test]
async fn plan67_multi_controller_scenario() {
    let app = AppHandle::spawn(CoreConfig::default());
    // Interchangeable controllers: same handle, three clones (PLAN §76).
    let gtk = app.clone();
    let cli = app.clone();
    let ws = app.clone();

    let mut gtk_events = gtk.subscribe(EventFilter::all());
    let mut cli_events = cli.subscribe(EventFilter::all());
    let mut ws_events = ws.subscribe(EventFilter::all());

    // Step 3: create two sources (GTK adds one, CLI adds one).
    let cam = added_source_id(
        &dispatch_ok(
            &gtk,
            Command::AddSource {
                kind: SourceKind::V4l2Camera,
                name: "camera".into(),
            },
        )
        .await,
    );
    let overlay = added_source_id(
        &dispatch_ok(
            &cli,
            Command::AddSource {
                kind: SourceKind::Color,
                name: "overlay".into(),
            },
        )
        .await,
    );

    // Step 4: compose them in one scene.
    let scene = added_scene_id(
        &dispatch_ok(
            &ws,
            Command::AddScene {
                name: "Program".into(),
            },
        )
        .await,
    );
    let cam_item = added_item_id(
        &dispatch_ok(
            &gtk,
            Command::AddSceneItem {
                scene_id: scene,
                source_id: cam,
            },
        )
        .await,
    );
    dispatch_ok(
        &cli,
        Command::AddSceneItem {
            scene_id: scene,
            source_id: overlay,
        },
    )
    .await;

    // Steps 5–7: move the camera item via GTK, CLI, then WebSocket.
    let mut moves = Vec::new();
    for (controller, x) in [(&gtk, 10.0), (&cli, 20.0), (&ws, 30.0)] {
        moves.push(
            dispatch_ok(
                controller,
                Command::SetSceneItemTransform {
                    scene_id: scene,
                    item_id: cam_item,
                    transform: move_to(x, 5.0),
                },
            )
            .await,
        );
    }

    // Step 8: every controller's subscription sees every event, identically.
    // Events: 2× source added, scene added + current changed, 2× item added,
    // 3× item updated = 9.
    let mut gtk_seen = Vec::new();
    let mut cli_seen = Vec::new();
    let mut ws_seen = Vec::new();
    for _ in 0..9 {
        gtk_seen.push(recv_event(&mut gtk_events).await);
        cli_seen.push(recv_event(&mut cli_events).await);
        ws_seen.push(recv_event(&mut ws_events).await);
    }
    assert_eq!(gtk_seen, cli_seen);
    assert_eq!(cli_seen, ws_seen);
    let seqs: Vec<u64> = gtk_seen.iter().map(|(seq, _)| *seq).collect();
    assert_eq!(seqs, (0..9).collect::<Vec<_>>(), "strict global ordering");

    // The final snapshot reflects all three remote moves.
    assert_eq!(item_position(&app, scene, cam_item), Vec2::new(30.0, 5.0));
    assert_eq!(app.snapshot().current_scene(), Some(scene));

    app.shutdown().await;
}

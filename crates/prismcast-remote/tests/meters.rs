//! Existing native meter wire schema, real session filtering and coalescing.
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_core::{Command, Event, SourceEvent, SourceId, SourceKind};
use prismcast_protocol::event::{MeterEvent, WireEvent};
use prismcast_protocol::handshake::Permission;
use prismcast_protocol::subscription::{EventCategory, Subscription, SubscriptionSet};
use prismcast_remote::auth::AuthConfig;
use prismcast_remote::ws_client::{WsClient, WsClientConfig};
use prismcast_remote::{WsServer, WsServerConfig};

const DEADLINE: Duration = Duration::from_secs(3);

async fn add_tone(app: &AppHandle, name: &str) -> SourceId {
    let source_id = app
        .dispatch(Command::AddSource {
            kind: SourceKind::TestPattern,
            name: name.into(),
        })
        .await
        .unwrap()
        .events
        .iter()
        .find_map(|event| match event {
            Event::Source(SourceEvent::Added { source }) => Some(source.id),
            _ => None,
        })
        .unwrap();
    app.dispatch(Command::SetSourceSettings {
        source_id,
        settings: serde_json::json!({"audio_test": true}),
    })
    .await
    .unwrap();
    source_id
}

async fn connect(addr: SocketAddr, subscriptions: Option<SubscriptionSet>) -> WsClient {
    WsClient::connect_with(
        addr,
        WsClientConfig {
            token: Some("meter-test".into()),
            subscriptions,
            ..WsClientConfig::default()
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn native_meters_require_opt_in_filter_sources_and_coalesce_latest() {
    let app = AppHandle::spawn(CoreConfig::default());
    let selected = add_tone(&app, "Selected").await;
    let other = add_tone(&app, "Other").await;
    let owner = app.attach_audio_owner().await.unwrap();
    let server = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            auth: AuthConfig::token("meter-test", vec![Permission::Read]),
            ..WsServerConfig::default()
        },
    )
    .await
    .unwrap();
    let mut defaults = connect(server.local_addr(), None).await;
    let mut meters = connect(
        server.local_addr(),
        Some(SubscriptionSet {
            entries: vec![Subscription::meters(vec![*selected.as_uuid()], Some(200))],
        }),
    )
    .await;
    let revision = app.snapshot().revision();
    for id in [other, selected] {
        owner
            .runtime
            .report_levels(revision, id, vec![-6.0; 2], vec![-9.0; 2])
            .await
            .unwrap();
    }
    let first = tokio::time::timeout(DEADLINE, meters.next_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.seq, 0);
    assert_eq!(first.category, EventCategory::Meter);
    assert_eq!(first.event.primary_entity(), Some(*selected.as_uuid()));
    assert_eq!(
        first.event,
        WireEvent::Meter(MeterEvent::Levels {
            source_id: *selected.as_uuid(),
            peak_dbfs: vec![-6.0; 2],
            rms_dbfs: vec![-9.0; 2],
        })
    );
    for peak in [-7.0, -8.0, -12.0] {
        owner
            .runtime
            .report_levels(revision, selected, vec![peak; 2], vec![peak - 3.0; 2])
            .await
            .unwrap();
    }
    let latest = tokio::time::timeout(DEADLINE, meters.next_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.seq, 1);
    assert!(matches!(latest.event, WireEvent::Meter(MeterEvent::Levels {
        source_id, peak_dbfs, rms_dbfs,
    }) if source_id == *selected.as_uuid() && peak_dbfs == vec![-12.0; 2] && rms_dbfs == vec![-15.0; 2]));
    assert_eq!(app.snapshot().revision(), revision);
    assert!(
        tokio::time::timeout(Duration::from_millis(80), defaults.next_event())
            .await
            .is_err()
    );
    meters.close().await;
    defaults.close().await;
    drop(owner);
    server.shutdown().await;
    app.shutdown().await;
}

#[tokio::test]
async fn disabled_source_discards_pending_meter_even_for_meter_only_client() {
    let app = AppHandle::spawn(CoreConfig::default());
    let source_id = add_tone(&app, "Tone").await;
    let owner = app.attach_audio_owner().await.unwrap();
    let server = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            auth: AuthConfig::token("meter-test", vec![Permission::Read]),
            ..WsServerConfig::default()
        },
    )
    .await
    .unwrap();
    let mut client = connect(
        server.local_addr(),
        Some(SubscriptionSet {
            entries: vec![Subscription::meters(vec![], Some(200))],
        }),
    )
    .await;
    let revision = app.snapshot().revision();
    owner
        .runtime
        .report_levels(revision, source_id, vec![-6.0; 2], vec![-9.0; 2])
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, client.next_event())
        .await
        .unwrap()
        .unwrap();
    owner
        .runtime
        .report_levels(revision, source_id, vec![-12.0; 2], vec![-15.0; 2])
        .await
        .unwrap();
    app.dispatch(Command::SetSourceEnabled {
        source_id,
        enabled: false,
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), client.next_event())
            .await
            .is_err()
    );
    assert!(app.subscribe_meters().borrow().levels.is_empty());
    client.close().await;
    drop(owner);
    server.shutdown().await;
    app.shutdown().await;
}

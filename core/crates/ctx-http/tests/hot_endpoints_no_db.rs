#![cfg(feature = "fault_injection")]

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

use ctx_core::models::{
    SessionHeadSnapshot, WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot,
    WorkspaceActiveSnapshotClientMessage, WorkspaceActiveSnapshotEvent,
    WorkspaceActiveSnapshotStreamMessage,
};
use ctx_daemon::test_support::HotEndpointManualHeadProbe;

mod common;

static FAILPOINT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn assert_hot_endpoints_with_failpoints(failpoints: &[&'static str]) {
    let _guard = FAILPOINT_LOCK.lock().await;
    ctx_store::fault_injection::clear_failpoints();
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    let server = common::spawn_http_server(app).await;
    let base = &server.base_url;
    let client = &server.client;

    let ws: ctx_core::models::Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let task: ctx_core::models::Task = client
        .post(format!("{base}/api/workspaces/{}/tasks", ws.id.0))
        .json(&json!({"title":"hot-endpoints"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let session = common::load_primary_session_http(client, base, &task).await;

    daemon
        .seed_hot_endpoint_caches_for_test(
            ws.id,
            task.id,
            session.id,
            50,
            10,
            true,
            Duration::from_secs(2),
        )
        .await
        .unwrap();

    ctx_store::fault_injection::clear_failpoints();
    for point in failpoints {
        ctx_store::fault_injection::set_failpoint(point, 10);
    }

    let snapshot: WorkspaceActiveSnapshot = client
        .get(format!(
            "{base}/api/workspaces/{}/active_snapshot?limit=5",
            ws.id.0
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot.workspace_id, ws.id);
    assert_eq!(snapshot.active.tasks.len(), 1);

    let heads: WorkspaceActiveHeadBatch = client
        .get(format!("{base}/api/workspaces/{}/active_heads", ws.id.0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(heads.workspace_id, ws.id);
    assert_eq!(heads.heads.len(), 1);

    let head_response = client
        .get(format!(
            "{base}/api/sessions/{}/head?limit=10&include_events=true",
            session.id.0
        ))
        .send()
        .await
        .unwrap();
    if failpoints.contains(&"ctx_store.get_session_head_snapshot") {
        assert_eq!(
            head_response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "include_events=true session detail reads should fail closed when the full store head path is unavailable"
        );
    } else {
        let head: SessionHeadSnapshot = head_response.json().await.unwrap();
        assert_eq!(head.session.id, session.id);
    }

    let ws_url = format!(
        "ws://{}/api/workspaces/{}/stream",
        base.trim_start_matches("http://"),
        ws.id.0
    );
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let WsMessage::Text(txt) = msg else {
        panic!("expected text frame, got {msg:?}");
    };
    let ready: WorkspaceActiveSnapshotStreamMessage = serde_json::from_str(&txt).unwrap();
    assert!(matches!(
        ready,
        WorkspaceActiveSnapshotStreamMessage::Event { ref event, .. }
            if matches!(event.as_ref(), WorkspaceActiveSnapshotEvent::Ready { .. })
    ));

    let subscribe = WorkspaceActiveSnapshotClientMessage::Subscribe {
        session_ids: vec![session.id],
        sessions: Vec::new(),
        task_ids: Vec::new(),
        foreground_session_id: None,
        scope: None,
        include_active_heads: true,
    };
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&subscribe).unwrap().into(),
        ))
        .await
        .unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let WsMessage::Text(txt) = msg else {
        panic!("expected text frame, got {msg:?}");
    };
    let message: WorkspaceActiveSnapshotStreamMessage = serde_json::from_str(&txt).unwrap();
    match message {
        WorkspaceActiveSnapshotStreamMessage::Snapshot {
            active_snapshot,
            active_heads,
            ..
        } => {
            assert_eq!(active_snapshot.workspace_id, ws.id);
            assert_eq!(active_snapshot.active.tasks.len(), 1);
            let Some(active_heads) = active_heads else {
                panic!("expected active heads in snapshot payload");
            };
            assert_eq!(active_heads.workspace_id, ws.id);
            assert_eq!(active_heads.heads.len(), 1);
        }
        other => panic!("expected snapshot payload, got {other:?}"),
    }

    ctx_store::fault_injection::clear_failpoints();
}

#[tokio::test]
async fn hot_endpoints_use_cache_when_db_unavailable() {
    assert_hot_endpoints_with_failpoints(&[
        "ctx_store.get_workspace_active_snapshot_state",
        "ctx_store.list_workspace_active_head_snapshots",
        "ctx_store.get_session_head_snapshot",
    ])
    .await;
}

#[tokio::test]
async fn hot_endpoints_snapshot_cache_handles_active_snapshot_failpoints() {
    assert_hot_endpoints_with_failpoints(&["ctx_store.get_workspace_active_snapshot_state"]).await;
}

#[tokio::test]
async fn hot_endpoints_heads_cache_handles_head_failpoints() {
    assert_hot_endpoints_with_failpoints(&[
        "ctx_store.list_workspace_active_head_snapshots",
        "ctx_store.get_session_head_snapshot",
    ])
    .await;
}

#[tokio::test]
async fn cold_workspace_active_endpoints_fail_closed_when_hydration_fails() {
    let _guard = FAILPOINT_LOCK.lock().await;
    ctx_store::fault_injection::clear_failpoints();
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let server = common::spawn_http_server(app).await;
    let base = &server.base_url;
    let client = &server.client;

    let ws: ctx_core::models::Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let task: ctx_core::models::Task = client
        .post(format!("{base}/api/workspaces/{}/tasks", ws.id.0))
        .json(&json!({"title":"cold-hydration"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let _session = common::load_primary_session_http(client, base, &task).await;

    ctx_store::fault_injection::set_failpoint("ctx_store.list_workspace_active_head_snapshots", 2);

    let snapshot = client
        .get(format!("{base}/api/workspaces/{}/active_snapshot", ws.id.0))
        .send()
        .await
        .unwrap();
    assert_eq!(
        snapshot.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );

    let heads = client
        .get(format!("{base}/api/workspaces/{}/active_heads", ws.id.0))
        .send()
        .await
        .unwrap();
    assert_eq!(heads.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);

    ctx_store::fault_injection::clear_failpoints();
}

#[tokio::test]
async fn publish_event_does_not_trigger_full_session_head_rebuilds() {
    let _guard = FAILPOINT_LOCK.lock().await;
    ctx_store::fault_injection::clear_failpoints();
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    let server = common::spawn_http_server(app).await;
    let base = &server.base_url;
    let client = &server.client;

    let ws: ctx_core::models::Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let task: ctx_core::models::Task = client
        .post(format!("{base}/api/workspaces/{}/tasks", ws.id.0))
        .json(&json!({"title":"hot-loop"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let session = common::load_primary_session_http(client, base, &task).await;

    daemon
        .ensure_workspace_active_snapshot_hydrated(ws.id)
        .await
        .unwrap();

    let event = daemon
        .append_hot_endpoint_delta_notice_for_test(session.id)
        .await
        .unwrap();

    ctx_store::fault_injection::clear_failpoints();
    ctx_store::fault_injection::set_failpoint("ctx_store.get_session_head_snapshot", 1);

    let last_event_seq = daemon
        .publish_hot_endpoint_event_and_active_head_seq_for_test(
            ws.id,
            event.clone(),
            Duration::from_millis(400),
        )
        .await
        .unwrap();
    assert_eq!(last_event_seq, event.seq);

    let manual_refresh = daemon
        .probe_hot_endpoint_manual_session_head_for_test(session.id)
        .await
        .unwrap();
    assert!(
        matches!(manual_refresh, HotEndpointManualHeadProbe::FailedClosed),
        "manual session head fetch should consume the one-shot failpoint because publish_event no longer rebuilds full heads"
    );

    ctx_store::fault_injection::clear_failpoints();
}

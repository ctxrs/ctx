use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio_tungstenite::{
    connect_async, tungstenite::Message as WsMessage, MaybeTlsStream, WebSocketStream,
};

use ctx_core::ids::{TurnId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    SessionEventType, WorkspaceActiveSnapshotEvent, WorkspaceActiveSnapshotStreamMessage,
    WorktreeVcsFreshness, WorktreeVcsSnapshot, WorktreeVcsStreamMessage,
};
use ctx_daemon::test_support::TestDaemon;

mod common;

type TestWsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn workspace_http_test_gate() -> &'static Arc<Semaphore> {
    static GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(Semaphore::new(4)))
}

fn worktree_id_strings(worktree_ids: &[WorktreeId]) -> Vec<String> {
    worktree_ids
        .iter()
        .map(|worktree_id| worktree_id.0.to_string())
        .collect()
}

fn worktree_vcs_snapshot_from_message(
    message: WorktreeVcsStreamMessage,
    worktree_id: WorktreeId,
) -> Option<WorktreeVcsSnapshot> {
    match message {
        WorktreeVcsStreamMessage::SummarySnapshot { snapshot, .. }
        | WorktreeVcsStreamMessage::DetailsSnapshot { snapshot, .. }
        | WorktreeVcsStreamMessage::UnavailableSnapshot { snapshot, .. }
            if snapshot.worktree_id == worktree_id =>
        {
            Some(snapshot)
        }
        _ => None,
    }
}

fn git_status_untracked_from_message(
    message: WorktreeVcsStreamMessage,
    worktree_id: WorktreeId,
) -> Option<i64> {
    worktree_vcs_snapshot_from_message(message, worktree_id)
        .map(|snapshot| snapshot.git_status.untracked)
}

async fn recv_vcs_stream_message(
    socket: &mut TestWsStream,
    timeout: Duration,
) -> Option<WorktreeVcsStreamMessage> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) = serde_json::from_str::<WorktreeVcsStreamMessage>(&txt) {
                return Some(message);
            }
        }
    }
    None
}

async fn recv_worktree_vcs_snapshot(
    socket: &mut TestWsStream,
    worktree_id: WorktreeId,
    timeout: Duration,
) -> Option<WorktreeVcsSnapshot> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining.min(Duration::from_millis(250));
        let Some(message) = recv_vcs_stream_message(socket, wait).await else {
            continue;
        };
        if let Some(snapshot) = worktree_vcs_snapshot_from_message(message, worktree_id) {
            return Some(snapshot);
        }
    }
    None
}

async fn open_workspace_vcs_stream(
    base: &str,
    workspace_id: WorkspaceId,
    summary_worktree_ids: &[WorktreeId],
    detail_worktree_ids: &[WorktreeId],
) -> TestWsStream {
    let ws_url =
        format!("{base}/api/workspaces/{}/vcs/stream", workspace_id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let ready = recv_vcs_stream_message(&mut socket, Duration::from_secs(2))
        .await
        .expect("expected vcs ready frame");
    assert!(matches!(ready, WorktreeVcsStreamMessage::Ready { .. }));

    let subscribe = json!({
        "type": "replace_subscription",
        "summary_worktree_ids": worktree_id_strings(summary_worktree_ids),
        "detail_worktree_ids": worktree_id_strings(detail_worktree_ids),
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();
    let subscribed = recv_vcs_stream_message(&mut socket, Duration::from_secs(2))
        .await
        .expect("expected vcs subscribed frame");
    assert!(matches!(
        subscribed,
        WorktreeVcsStreamMessage::Subscribed { .. }
    ));
    socket
}

async fn setup_with_root(
    repo: tempfile::TempDir,
) -> (
    tempfile::TempDir,
    common::FakeDaemonFixture,
    common::TestServer,
) {
    let permit = workspace_http_test_gate()
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await.with_resource_permit(permit);

    (repo, fixture, server)
}

async fn setup() -> (
    tempfile::TempDir,
    common::FakeDaemonFixture,
    common::TestServer,
) {
    setup_with_root(common::init_git_repo(&[("file.txt", "hello\n")]).await).await
}

async fn setup_git() -> (
    tempfile::TempDir,
    common::FakeDaemonFixture,
    common::TestServer,
) {
    setup().await
}

#[tokio::test]
async fn workspace_active_hydration_returns_500_for_store_open_failures_and_404_for_missing_workspaces(
) {
    let (repo, fixture, server) = setup().await;
    let daemon = &fixture.daemon;
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

    let missing = client
        .get(format!(
            "{base}/api/workspaces/{}/active_snapshot",
            uuid::Uuid::new_v4()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    daemon
        .workspace_active_snapshot_make_store_unopenable_for_test(ws.id)
        .await
        .unwrap();

    let broken_snapshot = client
        .get(format!("{base}/api/workspaces/{}/active_snapshot", ws.id.0))
        .send()
        .await
        .unwrap();
    assert_eq!(
        broken_snapshot.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );

    let broken_heads = client
        .get(format!("{base}/api/workspaces/{}/active_heads", ws.id.0))
        .send()
        .await
        .unwrap();
    assert_eq!(
        broken_heads.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
}

async fn decode_json_response<T: DeserializeOwned>(response: reqwest::Response) -> T {
    let status = response.status();
    let body = response.text().await.unwrap();
    serde_json::from_str(&body).unwrap_or_else(|err| {
        panic!("failed to decode JSON response (status {status}): {err}\nbody: {body}")
    })
}

async fn create_task_with_primary_worktree(
    client: &reqwest::Client,
    _daemon: &TestDaemon,
    base: &str,
    workspace_id: ctx_core::ids::WorkspaceId,
    _root_path: &std::path::Path,
    title: &str,
) -> ctx_core::models::Task {
    let task_id = ctx_core::ids::TaskId::new();
    let response = client
        .post(format!("{base}/api/workspaces/{}/tasks", workspace_id.0))
        .json(&json!({
            "id": task_id.0.to_string(),
            "title": title,
            "default_session": {
                "provider_id": "fake",
                "model_id": "fake-model",
            },
        }))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "create_task_with_primary_worktree failed for title {:?}: status {}",
        title,
        response.status()
    );
    let task: ctx_core::models::Task = decode_json_response(response).await;
    assert!(task.primary_worktree_id.is_some());
    assert!(task.primary_session_id.is_some());
    task
}

async fn create_session_with_request(
    client: &reqwest::Client,
    base: &str,
    task_id: ctx_core::ids::TaskId,
    request: Value,
) -> ctx_core::models::Session {
    let request = match request {
        Value::Object(map) => map,
        _ => panic!("session request must be a JSON object"),
    };
    assert!(
        request.contains_key("parent_session_id") && request.contains_key("relationship"),
        "test session creation through /api/tasks/:id/sessions must be an explicit child session"
    );

    let response = client
        .post(format!("{base}/api/tasks/{}/sessions", task_id.0))
        .json(&Value::Object(request))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "create_session_with_request failed for task {}: status {}",
        task_id.0,
        response.status()
    );
    decode_json_response(response).await
}

async fn create_primary_worktree_session(
    client: &reqwest::Client,
    base: &str,
    task_id: ctx_core::ids::TaskId,
) -> ctx_core::models::Session {
    let response = client
        .get(format!("{base}/api/tasks/{}/sessions", task_id.0))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "list sessions failed for task {}: status {}",
        task_id.0,
        response.status()
    );
    let sessions: Vec<ctx_core::models::Session> = decode_json_response(response).await;
    sessions
        .into_iter()
        .find(|session| session.parent_session_id.is_none() && session.relationship.is_none())
        .expect("task should have a primary session")
}

async fn create_child_worktree_session_with_request(
    client: &reqwest::Client,
    base: &str,
    task_id: ctx_core::ids::TaskId,
    request: Value,
) -> ctx_core::models::Session {
    create_session_with_request(client, base, task_id, request).await
}

async fn append_and_publish_event(
    daemon: &TestDaemon,
    session: &ctx_core::models::Session,
    event_type: SessionEventType,
    payload_json: Value,
) -> ctx_core::models::SessionEvent {
    daemon
        .workspace_active_snapshot_append_and_publish_event_for_test(
            session,
            None,
            None,
            event_type,
            payload_json,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn workspace_active_snapshot_includes_sessions() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task_active =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "active").await;

    let session = create_primary_worktree_session(client, base, task_active.id).await;

    let snapshot: ctx_core::models::WorkspaceActiveSnapshot = client
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
    assert_eq!(snapshot.active.tasks.len(), 1);
    let summary = &snapshot.active.tasks[0];
    assert_eq!(summary.task.id, task_active.id);
    assert_eq!(summary.primary_session.session.id, session.id);
    assert!(summary.primary_session_head.is_none());
    assert_eq!(snapshot.active.total_count, 1);
}

#[tokio::test]
async fn create_session_rejects_initial_prompt_without_client_ids() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task_active =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "active").await;
    let primary_session = create_primary_worktree_session(client, base, task_active.id).await;

    let resp = client
        .post(format!("{base}/api/tasks/{}/sessions", task_active.id.0))
        .json(&json!({
            "provider_id": "fake",
            "model_id": "fake-model",
            "parent_session_id": primary_session.id.0.to_string(),
            "relationship": "sub_agent",
            "initial_prompt": "hello"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    let summary: Value = client
        .get(format!(
            "{base}/api/telemetry/summary?metric=compat.payload_reject_count"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hit = summary
        .get("metrics")
        .and_then(Value::as_array)
        .map(|metrics| {
            metrics.iter().any(|metric| {
                let labels = metric.get("labels").and_then(Value::as_object);
                metric.get("name").and_then(Value::as_str) == Some("compat.payload_reject_count")
                    && labels
                        .and_then(|obj| obj.get("surface"))
                        .and_then(Value::as_str)
                        == Some("tasks.create_session")
                    && labels
                        .and_then(|obj| obj.get("issue"))
                        .and_then(Value::as_str)
                        == Some("missing_initial_ids")
                    && metric.get("sum").and_then(Value::as_f64).unwrap_or(0.0) >= 1.0
            })
        })
        .unwrap_or(false);
    assert!(
        hit,
        "expected compat.payload_reject_count telemetry for missing initial IDs"
    );
}

#[tokio::test]
async fn workspace_active_snapshot_includes_active_tasks_only() {
    let (repo, fixture, server) = setup_git().await;
    let state = &fixture.daemon;
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

    let task_active =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "active-task")
            .await;

    let _session_active = create_primary_worktree_session(client, base, task_active.id).await;

    let task_archived = create_task_with_primary_worktree(
        client,
        &state,
        base,
        ws.id,
        repo.path(),
        "archived-task",
    )
    .await;

    let _session_archived = create_primary_worktree_session(client, base, task_archived.id).await;

    let resp = client
        .post(format!("{base}/api/tasks/{}/archive", task_archived.id.0))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let snapshot: ctx_core::models::WorkspaceActiveSnapshot = client
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

    assert!(snapshot
        .active
        .tasks
        .iter()
        .any(|summary| summary.task.id == task_active.id));
    assert!(!snapshot
        .active
        .tasks
        .iter()
        .any(|summary| summary.task.id == task_archived.id));
}

#[tokio::test]
async fn workspace_active_heads_batch_strips_partials() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "active-heads")
            .await;

    let session = create_primary_worktree_session(client, base, task.id).await;

    state
        .workspace_active_snapshot_seed_completed_turn_with_partials_for_test(
            &session, "partial", "thinking",
        )
        .await
        .unwrap();

    let batch: ctx_core::models::WorkspaceActiveHeadBatch = client
        .get(format!("{base}/api/workspaces/{}/active_heads", ws.id.0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let head = batch
        .heads
        .iter()
        .find(|head| head.session.id == session.id)
        .expect("missing session head");
    assert_eq!(head.turns.len(), 1);
    assert!(head.turns[0].assistant_partial.is_none());
    assert!(head.turns[0].thought_partial.is_none());
    assert!(head
        .events
        .iter()
        .all(|event| { !matches!(event.event_type, SessionEventType::AssistantComplete) }));
}

#[tokio::test]
async fn session_snapshot_returns_summary_only() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "snapshot")
            .await;

    let session = create_primary_worktree_session(client, base, task.id).await;

    let snapshot: ctx_core::models::SessionSnapshot = client
        .get(format!(
            "{base}/api/sessions/{}/snapshot?limit=10",
            session.id.0
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(snapshot.summary.session.id, session.id);
    assert!(snapshot.head.is_none());
}

#[tokio::test]
async fn session_head_returns_head() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "head").await;

    let session = create_primary_worktree_session(client, base, task.id).await;

    let head: ctx_core::models::SessionHeadSnapshot = client
        .get(format!(
            "{base}/api/sessions/{}/head?limit=10",
            session.id.0
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(head.session.id, session.id);
}

#[tokio::test]
async fn workspace_stream_replays_from_after_seq() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "replay").await;

    let session = create_primary_worktree_session(client, base, task.id).await;
    let ev1 = append_and_publish_event(
        &state,
        &session,
        SessionEventType::Notice,
        json!({"msg":"one"}),
    )
    .await;
    let ev2 = append_and_publish_event(
        &state,
        &session,
        SessionEventType::Notice,
        json!({"msg":"two"}),
    )
    .await;
    let ev3 = append_and_publish_event(
        &state,
        &session,
        SessionEventType::Notice,
        json!({"msg":"three"}),
    )
    .await;

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let subscribe = json!({
        "type": "subscribe",
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": ev2.seq,
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    let mut seen_replay = false;
    let mut seen_old = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) =
                serde_json::from_str::<ctx_core::models::WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                match message {
                    ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event {
                        event, ..
                    } => {
                        let ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadDelta {
                            delta,
                            ..
                        } = event.as_ref()
                        else {
                            continue;
                        };
                        if delta.session_id != session.id {
                            continue;
                        }
                        if let Some(event) = delta.event.as_ref() {
                            if event.seq == ev3.seq {
                                seen_replay = true;
                            }
                            if event.seq <= ev2.seq {
                                seen_old = true;
                            }
                        }
                    }
                    ctx_core::models::WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
                        deltas,
                        ..
                    } => {
                        for delta in deltas {
                            if delta.session_id != session.id {
                                continue;
                            }
                            if let Some(event) = delta.event {
                                if event.seq == ev3.seq {
                                    seen_replay = true;
                                }
                                if event.seq <= ev2.seq {
                                    seen_old = true;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if seen_replay {
            break;
        }
    }

    assert!(seen_replay, "expected replay of newest event");
    assert!(!seen_old, "did not expect events at/before after_seq");
    assert!(ev1.seq < ev2.seq && ev2.seq < ev3.seq);
}

#[tokio::test]
async fn workspace_stream_reset_replay_waits_for_fresh_resume_cursor() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "replay").await;

    let session = create_primary_worktree_session(client, base, task.id).await;
    let _ev1 = append_and_publish_event(
        &state,
        &session,
        SessionEventType::Notice,
        json!({"msg":"one"}),
    )
    .await;
    let ev2 = append_and_publish_event(
        &state,
        &session,
        SessionEventType::Notice,
        json!({"msg":"two"}),
    )
    .await;
    let ev3 = append_and_publish_event(
        &state,
        &session,
        SessionEventType::Notice,
        json!({"msg":"three"}),
    )
    .await;

    fn message_contains_seq(
        message: &ctx_core::models::WorkspaceActiveSnapshotStreamMessage,
        session_id: ctx_core::ids::SessionId,
        seq: i64,
    ) -> bool {
        match message {
            ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event { event, .. } => {
                match event.as_ref() {
                    ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadDelta {
                        delta,
                        ..
                    } => {
                        delta.session_id == session_id
                            && delta.event.as_ref().map(|event| event.seq) == Some(seq)
                    }
                    _ => false,
                }
            }
            ctx_core::models::WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
                deltas, ..
            } => deltas.iter().any(|delta| {
                delta.session_id == session_id
                    && delta.event.as_ref().map(|event| event.seq) == Some(seq)
            }),
            _ => false,
        }
    }

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let initial_subscribe = json!({
        "type": "subscribe",
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": ev2.seq,
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(initial_subscribe.into()))
        .await
        .unwrap();

    let mut saw_initial_replay = false;
    let initial_deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < initial_deadline {
        let wait = initial_deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) =
                serde_json::from_str::<ctx_core::models::WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                if message_contains_seq(&message, session.id, ev3.seq) {
                    saw_initial_replay = true;
                    break;
                }
            }
        }
    }
    assert!(saw_initial_replay, "expected initial replay before reset");

    let reset_subscribe = json!({
        "type": "subscribe",
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "reset",
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(reset_subscribe.into()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let ev4 = append_and_publish_event(
        &state,
        &session,
        SessionEventType::Notice,
        json!({"msg":"four"}),
    )
    .await;

    let mut saw_reset_window_event = false;
    let reset_deadline = tokio::time::Instant::now() + Duration::from_millis(750);
    while tokio::time::Instant::now() < reset_deadline {
        let wait = reset_deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(Duration::from_millis(200));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) =
                serde_json::from_str::<ctx_core::models::WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                if message_contains_seq(&message, session.id, ev4.seq) {
                    saw_reset_window_event = true;
                    break;
                }
            }
        }
    }
    assert!(
        !saw_reset_window_event,
        "reset replay should stay quiet until the client resumes from a fresh head cursor"
    );

    let resume_subscribe = json!({
        "type": "subscribe",
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": ev3.seq,
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(resume_subscribe.into()))
        .await
        .unwrap();

    let mut saw_resumed_event = false;
    let resume_deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < resume_deadline {
        let wait = resume_deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) =
                serde_json::from_str::<ctx_core::models::WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                if message_contains_seq(&message, session.id, ev4.seq) {
                    saw_resumed_event = true;
                    break;
                }
            }
        }
    }
    assert!(
        saw_resumed_event,
        "expected explicit resume to replay the event that arrived during reset recovery"
    );
}

#[tokio::test]
async fn workspace_stream_replays_tool_events() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "tool-replay")
            .await;

    let session = create_primary_worktree_session(client, base, task.id).await;
    let ev1 = append_and_publish_event(
        &state,
        &session,
        SessionEventType::Notice,
        json!({"msg":"seed"}),
    )
    .await;
    let tool_call_id = "tool-1";
    let ev2 = append_and_publish_event(
        &state,
        &session,
        SessionEventType::ToolCall,
        json!({"tool_call_id": tool_call_id, "name": "fake_tool", "args": {}}),
    )
    .await;
    let ev3 = append_and_publish_event(
        &state,
        &session,
        SessionEventType::ToolResult,
        json!({"tool_call_id": tool_call_id, "result": "ok"}),
    )
    .await;

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let subscribe = json!({
        "type": "subscribe",
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": ev1.seq,
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    let mut saw_call = false;
    let mut saw_result = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) =
                serde_json::from_str::<ctx_core::models::WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                match message {
                    ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event {
                        event, ..
                    } => {
                        let ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadDelta {
                            delta,
                            ..
                        } = event.as_ref()
                        else {
                            continue;
                        };
                        if delta.session_id != session.id {
                            continue;
                        }
                        if let Some(event) = delta.event.as_ref() {
                            match event.event_type {
                                SessionEventType::ToolCall => saw_call = true,
                                SessionEventType::ToolResult => saw_result = true,
                                _ => {}
                            }
                        }
                    }
                    ctx_core::models::WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
                        deltas,
                        ..
                    } => {
                        for delta in deltas {
                            if delta.session_id != session.id {
                                continue;
                            }
                            if let Some(event) = delta.event {
                                match event.event_type {
                                    SessionEventType::ToolCall => saw_call = true,
                                    SessionEventType::ToolResult => saw_result = true,
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if saw_call && saw_result {
            break;
        }
    }

    assert!(saw_call, "expected tool_call event replay");
    assert!(saw_result, "expected tool_result event replay");
    assert!(ev1.seq < ev2.seq && ev2.seq < ev3.seq);
}

#[tokio::test]
async fn workspace_stream_under_load_no_gap_or_reset() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "load").await;

    let session = create_primary_worktree_session(client, base, task.id).await;

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let subscribe = json!({
        "type": "subscribe",
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": 1,
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;

    let total_events = 150usize;
    let mut last_seq = 0;
    for i in 0..total_events {
        let ev = append_and_publish_event(
            &state,
            &session,
            SessionEventType::Notice,
            json!({"msg": format!("load-{i}")}),
        )
        .await;
        last_seq = ev.seq;
    }

    let mut saw_reset = false;
    let mut saw_gap = false;
    let mut last_seen_seq = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let wait = remaining.min(Duration::from_millis(250));
        match tokio::time::timeout(wait, socket.next()).await {
            Ok(Some(Ok(WsMessage::Text(txt)))) => {
                if let Ok(message) = serde_json::from_str::<
                    ctx_core::models::WorkspaceActiveSnapshotStreamMessage,
                >(&txt)
                {
                    match message {
                        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::ResetRequired {
                            ..
                        } => {
                            saw_reset = true;
                            break;
                        }
                        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event {
                            event,
                            ..
                        } => match event.as_ref() {
                            ctx_core::models::WorkspaceActiveSnapshotEvent::SessionGap {
                                ..
                            } => {
                                saw_gap = true;
                                break;
                            }
                            ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadDelta {
                                delta,
                                ..
                            } if delta.session_id == session.id => {
                                last_seen_seq = last_seen_seq.max(delta.last_event_seq);
                            }
                            _ => {}
                        },
                        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
                            deltas,
                            ..
                        } => {
                            for delta in deltas {
                                if delta.session_id == session.id {
                                    last_seen_seq = last_seen_seq.max(delta.last_event_seq);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Some(Ok(WsMessage::Close(_)))) => {
                panic!("workspace stream closed under load");
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(err))) => panic!("workspace stream error: {err:?}"),
            Ok(None) => panic!("workspace stream closed under load"),
            Err(_) => {}
        }

        if last_seen_seq >= last_seq {
            break;
        }
    }

    assert!(!saw_reset, "unexpected reset_required under load");
    assert!(!saw_gap, "unexpected session_gap under load");
    assert!(
        last_seen_seq >= last_seq,
        "stream did not deliver events up to last seq"
    );
}

#[tokio::test]
async fn workspace_vcs_stream_emits_git_status_snapshot_on_change() {
    let (repo, fixture, server) = setup_git().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "git-status")
            .await;

    let session = create_primary_worktree_session(client, base, task.id).await;
    let worktree = state
        .workspace_active_snapshot_load_session_worktree_for_test(&session)
        .await
        .unwrap();
    let worktree_id = worktree.id;

    let mut socket = open_workspace_vcs_stream(base, ws.id, &[worktree_id], &[]).await;

    let mut saw_clean_snapshot = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) = serde_json::from_str::<WorktreeVcsStreamMessage>(&txt) {
                if let Some(untracked) = git_status_untracked_from_message(message, worktree_id) {
                    if untracked == 0 {
                        saw_clean_snapshot = true;
                        break;
                    }
                }
            }
        }
    }
    assert!(
        saw_clean_snapshot,
        "expected initial clean git status snapshot"
    );

    let file_path = std::path::Path::new(&worktree.root_path).join("git-status-live.txt");
    tokio::fs::write(&file_path, "change\n").await.unwrap();
    state
        .request_worktree_vcs_refresh_for_test(&worktree, true, false)
        .await
        .unwrap();

    let mut saw_untracked = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) = serde_json::from_str::<WorktreeVcsStreamMessage>(&txt) {
                if let Some(untracked) = git_status_untracked_from_message(message, worktree_id) {
                    if untracked >= 1 {
                        saw_untracked = true;
                        break;
                    }
                }
            }
        }
    }
    assert!(
        saw_untracked,
        "expected git status update after file change"
    );
}

#[tokio::test]
async fn workspace_vcs_stream_emits_git_status_snapshot_for_new_subscriber() {
    let (repo, fixture, server) = setup_git().await;
    let state = &fixture.daemon;
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

    let task = create_task_with_primary_worktree(
        client,
        &state,
        base,
        ws.id,
        repo.path(),
        "git-status-new-subscriber",
    )
    .await;

    let session_one = create_primary_worktree_session(client, base, task.id).await;
    let worktree_one = state
        .workspace_active_snapshot_load_session_worktree_for_test(&session_one)
        .await
        .unwrap();
    let worktree_one_id = worktree_one.id;

    let mut socket_one = open_workspace_vcs_stream(base, ws.id, &[worktree_one_id], &[]).await;

    let mut saw_initial = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket_one.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) = serde_json::from_str::<WorktreeVcsStreamMessage>(&txt) {
                if let Some(untracked) = git_status_untracked_from_message(message, worktree_one_id)
                {
                    if untracked == 0 {
                        saw_initial = true;
                        break;
                    }
                }
            }
        }
    }
    assert!(saw_initial, "expected initial git status snapshot");

    let session_two = create_primary_worktree_session(client, base, task.id).await;
    let worktree_two = state
        .workspace_active_snapshot_load_session_worktree_for_test(&session_two)
        .await
        .unwrap();
    let worktree_two_id = worktree_two.id;

    let mut socket_two = open_workspace_vcs_stream(base, ws.id, &[worktree_two_id], &[]).await;

    let mut saw_second = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket_two.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) = serde_json::from_str::<WorktreeVcsStreamMessage>(&txt) {
                if let Some(untracked) = git_status_untracked_from_message(message, worktree_two_id)
                {
                    if untracked == 0 {
                        saw_second = true;
                        break;
                    }
                }
            }
        }
    }
    assert!(
        saw_second,
        "expected git status snapshot for new subscriber"
    );
}

#[tokio::test]
async fn workspace_vcs_stream_delivers_summary_after_subscription() {
    let (repo, fixture, server) = setup_git().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "hydrate-vcs")
            .await;
    let session = create_primary_worktree_session(client, base, task.id).await;

    let worktree = state
        .workspace_active_snapshot_load_session_worktree_for_test(&session)
        .await
        .unwrap();
    tokio::fs::write(
        std::path::Path::new(&worktree.root_path).join("file.txt"),
        "hello\nchanged\n",
    )
    .await
    .unwrap();

    let mut socket = open_workspace_vcs_stream(base, ws.id, &[worktree.id], &[]).await;

    let mut hydrated_file_count = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining.min(Duration::from_millis(250));
        if let Some(snapshot) = recv_worktree_vcs_snapshot(&mut socket, worktree.id, wait).await {
            if snapshot.freshness == WorktreeVcsFreshness::Fresh {
                hydrated_file_count = snapshot.summary.file_count;
                break;
            }
        }
    }

    assert_eq!(
        hydrated_file_count,
        Some(1),
        "expected later worktree vcs event to include ready worktree vcs counts"
    );
}

#[tokio::test]
async fn workspace_vcs_stream_repeat_subscribe_preserves_ready_worktree_vcs_state() {
    let (repo, fixture, server) = setup_git().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "ready-vcs")
            .await;
    let session = create_primary_worktree_session(client, base, task.id).await;
    let worktree = state
        .workspace_active_snapshot_load_session_worktree_for_test(&session)
        .await
        .unwrap();

    let seeded = state
        .workspace_active_snapshot_seed_ready_vcs_summary_for_test(worktree.clone())
        .await
        .unwrap();
    assert_eq!(seeded.freshness, WorktreeVcsFreshness::Fresh);

    let mut socket = open_workspace_vcs_stream(base, ws.id, &[worktree.id], &[]).await;
    let initial_worktree =
        recv_worktree_vcs_snapshot(&mut socket, worktree.id, Duration::from_secs(2))
            .await
            .expect("missing worktree");
    assert_eq!(initial_worktree.freshness, WorktreeVcsFreshness::Fresh);

    let repeat = json!({
        "type": "replace_subscription",
        "summary_worktree_ids": worktree_id_strings(&[worktree.id]),
        "detail_worktree_ids": [],
    })
    .to_string();
    socket.send(WsMessage::Text(repeat.into())).await.unwrap();
    let repeated = recv_vcs_stream_message(&mut socket, Duration::from_secs(2))
        .await
        .expect("expected repeated subscription acknowledgement");
    assert!(matches!(
        repeated,
        WorktreeVcsStreamMessage::Subscribed { .. }
    ));
    let repeat_snapshot = recv_vcs_stream_message(&mut socket, Duration::from_millis(250)).await;
    assert!(
        repeat_snapshot.is_none(),
        "repeat subscribe should acknowledge demand without reseeding unchanged VCS snapshots"
    );

    let watcher_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if state
            .workspace_active_snapshot_worktree_has_vcs_watcher_for_test(worktree.id)
            .await
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < watcher_deadline,
            "repeat subscribe should register the worktree VCS watcher"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The regression this test guards is subscription warm-up downgrading an
    // already-ready snapshot. Keep the stability window shorter than the
    // filesystem watcher debounce so Linux watcher startup noise is tested by
    // watcher-specific coverage instead of this subscribe contract.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
    while tokio::time::Instant::now() < deadline {
        let current = state
            .worktree_vcs_snapshot(worktree.id)
            .await
            .expect("expected worktree snapshot to remain cached");
        assert_eq!(
            current.freshness,
            WorktreeVcsFreshness::Fresh,
            "repeat subscribe should not downgrade ready worktree vcs state"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn workspace_vcs_stream_subscribe_does_not_reemit_when_worktree_vcs_is_already_computing() {
    let (repo, fixture, server) = setup_git().await;
    let state = &fixture.daemon;
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

    let task = create_task_with_primary_worktree(
        client,
        &state,
        base,
        ws.id,
        repo.path(),
        "computing-vcs",
    )
    .await;
    let session = create_primary_worktree_session(client, base, task.id).await;
    let worktree = state
        .workspace_active_snapshot_load_session_worktree_for_test(&session)
        .await
        .unwrap();

    state
        .workspace_active_snapshot_seed_ready_vcs_summary_for_test(worktree.clone())
        .await
        .unwrap();
    tokio::fs::write(
        std::path::Path::new(&worktree.root_path).join("file.txt"),
        "hello\nchanged\n",
    )
    .await
    .unwrap();

    let _refresh_guard = state
        .workspace_active_snapshot_hold_vcs_refresh_lock_for_test(worktree.id)
        .await;
    state
        .request_worktree_vcs_refresh_for_test(&worktree, true, false)
        .await
        .unwrap();

    let seeded = state
        .worktree_vcs_snapshot(worktree.id)
        .await
        .expect("expected seeded worktree vcs snapshot");
    assert_eq!(seeded.freshness, WorktreeVcsFreshness::Stale);
    let seeded_rev = seeded.rev;

    let mut socket = open_workspace_vcs_stream(base, ws.id, &[worktree.id], &[]).await;
    let initial_worktree =
        recv_worktree_vcs_snapshot(&mut socket, worktree.id, Duration::from_secs(2))
            .await
            .expect("missing worktree");
    assert_eq!(initial_worktree.freshness, WorktreeVcsFreshness::Stale);
    assert_eq!(
        initial_worktree.rev, seeded_rev,
        "subscribe should reuse the in-flight computing snapshot instead of force-emitting again"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    while tokio::time::Instant::now() < deadline {
        let snapshot = state
            .worktree_vcs_snapshot(worktree.id)
            .await
            .expect("expected worktree vcs snapshot to remain present");
        assert_eq!(
            snapshot.rev, seeded_rev,
            "subscribe should not bump the worktree vcs rev while the summary refresh is already in flight"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn worktree_vcs_summary_refresh_reloads_live_inventory_before_ready_publish() {
    let (repo, fixture, server) = setup_git().await;
    let state = &fixture.daemon;
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

    let task = create_task_with_primary_worktree(
        client,
        &state,
        base,
        ws.id,
        repo.path(),
        "summary-refresh-live-inventory",
    )
    .await;
    let session = create_primary_worktree_session(client, base, task.id).await;
    let worktree = state
        .workspace_active_snapshot_load_session_worktree_for_test(&session)
        .await
        .unwrap();

    let first_path = std::path::Path::new(&worktree.root_path).join("first.txt");
    let second_path = std::path::Path::new(&worktree.root_path).join("second.txt");
    tokio::fs::write(&first_path, "first\n").await.unwrap();

    state.mark_worktree_vcs_active_for_test(worktree.id).await;
    state
        .workspace_active_snapshot_mark_vcs_pane_open_for_test(worktree.id)
        .await;
    state
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .unwrap();

    tokio::fs::remove_file(&first_path).await.unwrap();
    tokio::fs::write(&second_path, "second\n").await.unwrap();
    state
        .request_worktree_vcs_refresh_for_test(&worktree, true, true)
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let snapshot = state
            .worktree_vcs_snapshot(worktree.id)
            .await
            .expect("expected worktree vcs snapshot");
        if snapshot.freshness == WorktreeVcsFreshness::Fresh {
            let touched_paths = snapshot
                .touched_files
                .items
                .iter()
                .map(|item| item.path.as_str())
                .collect::<Vec<_>>();
            assert!(
                touched_paths.contains(&"second.txt"),
                "summary refresh should recompute live inventory before publishing ready state"
            );
            assert!(
                !touched_paths.contains(&"first.txt"),
                "ready snapshot should not publish stale touched-file inventory"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    panic!("timed out waiting for ready worktree vcs snapshot");
}

#[tokio::test]
async fn worktree_vcs_emit_and_summary_refresh_share_refresh_lock() {
    let (repo, fixture, server) = setup_git().await;
    let state = &fixture.daemon;
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

    let task = create_task_with_primary_worktree(
        client,
        &state,
        base,
        ws.id,
        repo.path(),
        "shared-refresh-lock",
    )
    .await;
    let session = create_primary_worktree_session(client, base, task.id).await;
    let worktree = state
        .workspace_active_snapshot_load_session_worktree_for_test(&session)
        .await
        .unwrap();

    state.mark_worktree_vcs_active_for_test(worktree.id).await;

    let refresh_guard = state
        .workspace_active_snapshot_hold_vcs_refresh_lock_for_test(worktree.id)
        .await;

    let emit_state = state.clone();
    let emit_worktree = worktree.clone();
    let emit_handle = tokio::spawn(async move {
        emit_state
            .emit_worktree_vcs_snapshot_for_worktree(&emit_worktree, false)
            .await
    });

    let summary_state = state.clone();
    let summary_worktree = worktree.clone();
    let summary_handle = tokio::spawn(async move {
        summary_state
            .refresh_worktree_vcs_summary_for_test(summary_worktree)
            .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !emit_handle.is_finished(),
        "emit path should wait on the shared per-worktree refresh lock"
    );
    assert!(
        !summary_handle.is_finished(),
        "summary refresh should wait on the shared per-worktree refresh lock"
    );

    drop(refresh_guard);

    emit_handle.await.unwrap().unwrap();
    summary_handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn worktree_vcs_activity_eviction_drops_refresh_lock() {
    let (repo, fixture, server) = setup_git().await;
    let state = &fixture.daemon;
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

    let task = create_task_with_primary_worktree(
        client,
        &state,
        base,
        ws.id,
        repo.path(),
        "refresh-lock-eviction",
    )
    .await;
    let session = create_primary_worktree_session(client, base, task.id).await;
    let worktree = state
        .workspace_active_snapshot_load_session_worktree_for_test(&session)
        .await
        .unwrap();

    state
        .workspace_active_snapshot_verify_vcs_refresh_lock_eviction_for_test(worktree.id)
        .await
        .unwrap();
}

#[tokio::test]
async fn workspace_stream_emits_gap_when_replay_exceeds_daemon_head_window() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "gap").await;

    let session = create_primary_worktree_session(client, base, task.id).await;
    assert!(
        state
            .workspace_active_snapshot_task_contains_sessions_for_test(task.id, &[session.id])
            .await
            .unwrap(),
        "expected session to be stored"
    );

    for _ in 0..65 {
        append_and_publish_event(
            &state,
            &session,
            SessionEventType::Notice,
            json!({"msg":"spam"}),
        )
        .await;
    }

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let subscribe = json!({
        "type": "subscribe",
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": 1,
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    let mut seen_gap = false;
    let mut seen_seed_follows = false;
    let mut seen_seed_after_gap = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event {
                event, ..
            }) =
                serde_json::from_str::<ctx_core::models::WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                match event.as_ref() {
                    ctx_core::models::WorkspaceActiveSnapshotEvent::SessionGap {
                        session_id,
                        seed_follows,
                        ..
                    } if *session_id == session.id => {
                        seen_gap = true;
                        seen_seed_follows = *seed_follows;
                    }
                    ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadSeed {
                        head,
                        ..
                    } if seen_gap && head.session.id == session.id => {
                        seen_seed_after_gap = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(
        seen_gap,
        "expected session_gap when replay exceeds daemon head window"
    );
    assert!(
        seen_seed_follows,
        "expected replay session_gap to declare the paired session_head_seed"
    );
    assert!(
        seen_seed_after_gap,
        "expected session_head_seed after daemon head-window replay gap"
    );
}

#[tokio::test]
async fn workspace_active_snapshot_stream_pushes_updates() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let ready_msg = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let WsMessage::Text(txt) = ready_msg {
        let message: ctx_core::models::WorkspaceActiveSnapshotStreamMessage =
            serde_json::from_str(&txt).unwrap();
        match message {
            ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event { event, .. } => {
                match event.as_ref() {
                    ctx_core::models::WorkspaceActiveSnapshotEvent::Ready { .. } => {}
                    other => panic!("expected ready, got {other:?}"),
                }
            }
            other => panic!("expected ready, got {other:?}"),
        }
    } else {
        panic!("expected ready text frame");
    }

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "live").await;

    let _session = create_primary_worktree_session(client, base, task.id).await;

    let mut saw_upsert = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event {
                event, ..
            }) =
                serde_json::from_str::<ctx_core::models::WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                if let ctx_core::models::WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
                    task: summary,
                    ..
                } = event.as_ref()
                {
                    if summary.task.id == task.id {
                        saw_upsert = true;
                        break;
                    }
                }
            }
        }
    }
    assert!(saw_upsert);
}

#[tokio::test]
async fn workspace_stream_session_updates_emit_task_delta_without_full_active_task_upsert() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "delta").await;
    let session = create_primary_worktree_session(client, base, task.id).await;

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let subscribe = json!({
        "type": "subscribe",
        "scope": "active",
        "include_active_heads": true,
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    let mut saw_snapshot = false;
    let snapshot_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < snapshot_deadline {
        let remaining = snapshot_deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(WorkspaceActiveSnapshotStreamMessage::Snapshot {
                active_snapshot, ..
            }) = serde_json::from_str::<WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                if active_snapshot
                    .active
                    .tasks
                    .iter()
                    .any(|summary| summary.task.id == task.id)
                {
                    saw_snapshot = true;
                    break;
                }
            }
        }
    }
    assert!(
        saw_snapshot,
        "expected initial active snapshot after subscribe"
    );

    state
        .workspace_active_snapshot_append_and_publish_event_for_test(
            &session,
            None,
            Some(TurnId::new()),
            SessionEventType::UserMessage,
            json!({
                "message_id": uuid::Uuid::new_v4().to_string(),
                "content": "hello from test",
            }),
        )
        .await
        .unwrap();

    let mut saw_summary_delta = false;
    let mut saw_task_delta = false;
    let mut saw_upsert = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(WorkspaceActiveSnapshotStreamMessage::Event { event, .. }) =
                serde_json::from_str::<WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                match event.as_ref() {
                    WorkspaceActiveSnapshotEvent::SessionSummaryDelta { delta, .. }
                        if delta.task_id == task.id && delta.session_id == session.id =>
                    {
                        saw_summary_delta = true;
                    }
                    WorkspaceActiveSnapshotEvent::TaskDelta { delta, .. }
                        if delta.task.id == task.id
                            && matches!(delta.kind, ctx_core::models::TaskDeltaKind::Updated) =>
                    {
                        assert!(
                            delta.task.has_active_session,
                            "session-driven task delta must preserve has_active_session"
                        );
                        saw_task_delta = true;
                    }
                    WorkspaceActiveSnapshotEvent::ActiveTaskUpsert { task: summary, .. }
                        if summary.task.id == task.id =>
                    {
                        saw_upsert = true;
                    }
                    _ => {}
                }
            }
        }
        if saw_summary_delta && saw_task_delta && saw_upsert {
            break;
        }
    }

    assert!(
        saw_summary_delta,
        "expected session_summary_delta for active task session"
    );
    assert!(
        saw_task_delta,
        "expected task_delta update instead of a full task upsert"
    );
    assert!(
        !saw_upsert,
        "session updates should not emit a full active_task_upsert after the roster is already hydrated"
    );
}

#[tokio::test]
async fn workspace_stream_archived_task_upsert_has_no_snapshot_payload() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "archive me")
            .await;

    let _session = create_primary_worktree_session(client, base, task.id).await;

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let ready_msg = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let WsMessage::Text(txt) = ready_msg {
        let message: ctx_core::models::WorkspaceActiveSnapshotStreamMessage =
            serde_json::from_str(&txt).unwrap();
        match message {
            ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event { event, .. } => {
                match event.as_ref() {
                    ctx_core::models::WorkspaceActiveSnapshotEvent::Ready { .. } => {}
                    other => panic!("expected ready, got {other:?}"),
                }
            }
            other => panic!("expected ready, got {other:?}"),
        }
    } else {
        panic!("expected ready text frame");
    }

    let subscribe = json!({ "type": "subscribe" }).to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    let resp = client
        .post(format!("{base}/api/tasks/{}/archive", task.id.0))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let task_id = task.id.0.to_string();
    let mut archived_event: Option<Value> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            let value: Value = serde_json::from_str(&txt).unwrap();
            let event = match value.get("event") {
                Some(event) => event,
                None => continue,
            };
            if event.get("type").and_then(|value| value.as_str()) != Some("archived_task_upsert") {
                continue;
            }
            let event_task_id = event
                .get("task")
                .and_then(|value| value.get("task"))
                .and_then(|value| value.get("id"))
                .and_then(|value| value.as_str());
            if event_task_id == Some(task_id.as_str()) {
                archived_event = Some(value);
                break;
            }
        }
    }

    let archived_event = archived_event.expect("expected archived_task_upsert event");
    assert_eq!(
        archived_event.get("type").and_then(|value| value.as_str()),
        Some("event")
    );
    assert!(archived_event.get("active_snapshot").is_none());
    assert!(archived_event.get("active_heads").is_none());
    let event = archived_event.get("event").expect("missing event");
    assert_eq!(
        event.get("type").and_then(|value| value.as_str()),
        Some("archived_task_upsert")
    );
    assert!(event.get("snapshot_rev").is_none());
    assert!(event.get("snapshot").is_none());
}

#[tokio::test]
async fn workspace_active_snapshot_stream_filters_session_head_deltas() {
    let (repo, fixture, server) = setup().await;
    let state = &fixture.daemon;
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

    let task =
        create_task_with_primary_worktree(client, &state, base, ws.id, repo.path(), "live").await;

    let session_a = create_primary_worktree_session(client, base, task.id).await;
    let session_b = create_child_worktree_session_with_request(
        client,
        base,
        task.id,
        json!({
            "provider_id": "fake",
            "model_id": "fake-model",
            "parent_session_id": session_a.id.0.to_string(),
            "relationship": "secondary",
        }),
    )
    .await;
    assert_ne!(
        session_a.id, session_b.id,
        "filter coverage requires a distinct unsubscribed session"
    );
    assert!(
        state
            .workspace_active_snapshot_task_contains_sessions_for_test(
                task.id,
                &[session_a.id, session_b.id],
            )
            .await
            .unwrap(),
        "expected sessions to be stored"
    );

    let ws_url = format!("{base}/api/workspaces/{}/stream", ws.id.0).replace("http://", "ws://");
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let subscribe = json!({
        "type": "subscribe",
        "sessions": [{
            "session_id": session_a.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": 0,
            },
        }],
    })
    .to_string();
    socket
        .send(WsMessage::Text(subscribe.into()))
        .await
        .unwrap();

    client
        .post(format!("{base}/api/sessions/{}/messages", session_a.id.0))
        .json(&json!({"content":"hello a"}))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/api/sessions/{}/messages", session_b.id.0))
        .json(&json!({"content":"hello b"}))
        .send()
        .await
        .unwrap();

    let mut seen_a = false;
    let mut seen_b = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let wait = remaining.min(Duration::from_millis(250));
        let next = tokio::time::timeout(wait, socket.next()).await;
        if let Ok(Some(Ok(WsMessage::Text(txt)))) = next {
            if let Ok(message) =
                serde_json::from_str::<ctx_core::models::WorkspaceActiveSnapshotStreamMessage>(&txt)
            {
                match message {
                    ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event {
                        event, ..
                    } => {
                        let ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadDelta {
                            delta,
                            ..
                        } = event.as_ref()
                        else {
                            continue;
                        };
                        if delta.session_id == session_a.id {
                            seen_a = true;
                        }
                        if delta.session_id == session_b.id {
                            seen_b = true;
                        }
                    }
                    ctx_core::models::WorkspaceActiveSnapshotStreamMessage::HeadsBatch {
                        deltas,
                        ..
                    } => {
                        for delta in deltas {
                            if delta.session_id == session_a.id {
                                seen_a = true;
                            }
                            if delta.session_id == session_b.id {
                                seen_b = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if seen_a {
            break;
        }
    }

    assert!(seen_a, "expected delta for subscribed session");
    assert!(!seen_b, "did not expect delta for unsubscribed session");
}

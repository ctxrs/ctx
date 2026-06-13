mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ctx_core::models::{SessionHeadSnapshot, WorkspaceActiveHeadBatch};

#[tokio::test]
async fn session_head_rehydrates_after_cache_eviction() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_session_for_test(true, true)
        .await
        .unwrap();
    let session = &seed.session;

    let baseline = daemon
        .cache_rehydration_full_head_for_test(session.id, 60, true)
        .await
        .unwrap();
    daemon
        .cache_rehydration_seed_replay_head_cache_for_test(baseline.clone())
        .await;
    assert!(daemon
        .cache_rehydration_replay_session_head_cached_for_test(session.id)
        .await
        .is_some());

    daemon
        .cache_rehydration_cleanup_session_for_test(session.id)
        .await;
    assert!(daemon
        .cache_rehydration_replay_session_head_cached_for_test(session.id)
        .await
        .is_none());

    let app = fixture.router();
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/sessions/{}/head?include_events=true&limit=60",
            session.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let (status, head): (StatusCode, SessionHeadSnapshot) = common::oneshot_json(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(head.session.id, session.id);
    assert_eq!(head.last_event_seq, baseline.last_event_seq);
}

#[tokio::test]
async fn session_head_min_event_seq_bypasses_stale_active_snapshot_cache() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_session_for_test(false, false)
        .await
        .unwrap();
    let session = &seed.session;

    let cached_head = daemon
        .cache_rehydration_full_head_for_test(session.id, 60, true)
        .await
        .unwrap();
    daemon
        .cache_rehydration_seed_replay_head_cache_for_test(cached_head.clone())
        .await;

    daemon
        .cache_rehydration_seed_completed_notice_for_test(
            session,
            seed.task.id,
            serde_json::json!({ "kind": "cache_boundary", "message": "newer than cache" }),
            None,
        )
        .await
        .unwrap();
    let full_head = daemon
        .cache_rehydration_full_head_for_test(session.id, 60, true)
        .await
        .unwrap();
    assert!(
        full_head.last_event_seq > cached_head.last_event_seq,
        "test setup needs a stale active snapshot cache"
    );

    let app = fixture.router();
    let stale_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/sessions/{}/head?include_events=true&limit=60",
            session.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let (stale_status, stale_head): (StatusCode, SessionHeadSnapshot) =
        common::oneshot_json(&app, stale_req).await;
    assert_eq!(stale_status, StatusCode::OK);
    assert_eq!(stale_head.last_event_seq, cached_head.last_event_seq);

    let repair_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/sessions/{}/head?include_events=true&limit=60&min_event_seq={}",
            session.id.0, full_head.last_event_seq
        ))
        .body(Body::empty())
        .unwrap();
    let (repair_status, repair_head): (StatusCode, SessionHeadSnapshot) =
        common::oneshot_json(&app, repair_req).await;
    assert_eq!(repair_status, StatusCode::OK);
    assert_eq!(repair_head.session.id, session.id);
    assert_eq!(repair_head.last_event_seq, full_head.last_event_seq);
}

#[tokio::test]
async fn include_events_session_heads_bypass_compact_cache() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_session_for_test(false, true)
        .await
        .unwrap();
    let session = &seed.session;

    daemon
        .cache_rehydration_seed_completed_notice_for_test(
            session,
            seed.task.id,
            serde_json::json!({ "kind": "cache_boundary", "message": "persist me" }),
            None,
        )
        .await
        .unwrap();

    let full_head = daemon
        .cache_rehydration_full_head_for_test(session.id, 60, true)
        .await
        .unwrap();
    assert!(
        !full_head.events.is_empty(),
        "full session head should include the persisted event tail"
    );

    let compact_head = daemon
        .cache_rehydration_active_head_for_test(session.id)
        .await
        .unwrap()
        .expect("compact active head");
    assert!(compact_head.events.is_empty());
    daemon
        .cache_rehydration_seed_compact_head_cache_for_test(compact_head)
        .await;

    let app = fixture.router();
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/sessions/{}/head?include_events=true&limit=60",
            session.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let (status, head): (StatusCode, SessionHeadSnapshot) = common::oneshot_json(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(head.session.id, session.id);
    assert_eq!(head.last_event_seq, full_head.last_event_seq);
    assert_eq!(head.events.len(), full_head.events.len());
    assert_eq!(
        head.events.last().map(|event| event.seq),
        full_head.events.last().map(|event| event.seq)
    );
}

#[tokio::test]
async fn include_events_session_heads_use_hydrated_replay_cache_when_store_cannot_open() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_session_for_test(false, false)
        .await
        .unwrap();
    let session = &seed.session;

    daemon
        .cache_rehydration_seed_completed_notice_for_test(
            session,
            seed.task.id,
            serde_json::json!({ "kind": "hydrated_cache", "message": "persist me" }),
            Some("persist me"),
        )
        .await
        .unwrap();

    let full_head = daemon
        .cache_rehydration_full_head_for_test(session.id, 60, true)
        .await
        .unwrap();
    assert!(
        !full_head.events.is_empty(),
        "full head should include persisted events for replay-capable caching"
    );
    daemon
        .cache_rehydration_seed_replay_head_cache_for_test(full_head.clone())
        .await;

    daemon
        .cache_rehydration_make_workspace_store_unopenable_for_test(seed.workspace.id)
        .await
        .unwrap();

    let app = fixture.router();
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/sessions/{}/head?include_events=true&limit=60",
            session.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let (status, head): (StatusCode, SessionHeadSnapshot) = common::oneshot_json(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(head.session.id, session.id);
    assert_eq!(head.last_event_seq, full_head.last_event_seq);
    assert_eq!(head.events.len(), full_head.events.len());
}

#[tokio::test]
async fn archiving_task_invalidates_cached_replay_session_head() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;

    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;
    let (task, session) =
        common::create_task_with_session(&app, workspace.id.0, "task", "fake", "model").await;

    let full_head = daemon
        .cache_rehydration_full_head_for_test(session.id, 60, true)
        .await
        .unwrap();
    daemon
        .cache_rehydration_seed_replay_head_cache_for_test(full_head)
        .await;
    assert!(daemon
        .cache_rehydration_replay_session_head_cached_for_test(session.id)
        .await
        .is_some());

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/tasks/{}/archive", task.id.0))
        .body(Body::empty())
        .unwrap();
    let (status, _body) = common::oneshot_bytes(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        daemon
            .cache_rehydration_replay_session_head_cached_for_test(session.id)
            .await
            .is_none(),
        "task archive should invalidate replay-capable cached heads for the task's sessions"
    );
}

#[tokio::test]
async fn non_primary_store_backed_head_is_purged_on_workspace_cleanup() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_session_for_test(false, false)
        .await
        .unwrap();
    let session = &seed.session;

    let head = daemon
        .cache_rehydration_full_head_for_test(session.id, 60, false)
        .await
        .unwrap();
    daemon
        .cache_rehydration_seed_compact_head_cache_for_test(head)
        .await;
    assert!(daemon
        .cache_rehydration_session_head_for_read_cached_for_test(session.id)
        .await
        .is_some());
    assert!(
        daemon
            .cache_rehydration_replay_session_head_cached_for_test(session.id)
            .await
            .is_none(),
        "event-stripped reads should not populate the replay-capable session-head cache"
    );

    daemon
        .cache_rehydration_cleanup_workspace_for_test(seed.workspace.id)
        .await;
    daemon
        .cache_rehydration_delete_workspace_rows_for_test(seed.workspace.id)
        .await
        .unwrap();

    assert!(daemon
        .cache_rehydration_session_head_for_read_cached_for_test(session.id)
        .await
        .is_none());

    let app = fixture.router();
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/sessions/{}/head?include_events=false&limit=60",
            session.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let (status, _body) = common::oneshot_bytes(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/attachments",
            seed.workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let (status, _body) = common::oneshot_bytes(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn session_read_routes_return_500_when_workspace_store_cannot_open() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_session_for_test(false, false)
        .await
        .unwrap();
    let session = &seed.session;

    daemon
        .cache_rehydration_cleanup_session_for_test(session.id)
        .await;
    daemon
        .cache_rehydration_make_workspace_store_unopenable_for_test(seed.workspace.id)
        .await
        .unwrap();

    let app = fixture.router();
    let turn_id = ctx_core::ids::TurnId::new();
    let routes = [
        format!(
            "/api/sessions/{}/head?include_events=false&limit=60",
            session.id.0
        ),
        format!(
            "/api/sessions/{}/snapshot?include_events=false&limit=60",
            session.id.0
        ),
        format!("/api/sessions/{}/state", session.id.0),
        format!("/api/sessions/{}/events?tail=1", session.id.0),
        format!("/api/sessions/{}/history?limit=60", session.id.0),
        format!("/api/sessions/{}/turns/{}/tools", session.id.0, turn_id.0),
        format!("/api/workspaces/{}/attachments", seed.workspace.id.0),
        format!("/api/workspaces/{}/tasks", seed.workspace.id.0),
    ];
    for route in routes {
        let req = Request::builder()
            .method("GET")
            .uri(route)
            .body(Body::empty())
            .unwrap();
        let (status, _body) = common::oneshot_bytes(&app, req).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{}/messages", session.id.0))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"content":"hello from blocked workspace store"}"#,
        ))
        .unwrap();
    let (status, _body) = common::oneshot_bytes(&app, req).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn delete_in_progress_workspace_and_session_reads_return_404() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_session_for_test(false, false)
        .await
        .unwrap();
    let session = &seed.session;

    daemon
        .cache_rehydration_begin_workspace_delete_for_test(seed.workspace.id)
        .await;

    let app = fixture.router();
    for route in [
        format!(
            "/api/sessions/{}/head?include_events=false&limit=60",
            session.id.0
        ),
        format!(
            "/api/sessions/{}/head?include_events=true&limit=60",
            session.id.0
        ),
        format!("/api/workspaces/{}/attachments", seed.workspace.id.0),
        format!("/api/workspaces/{}/active_heads", seed.workspace.id.0),
    ] {
        let req = Request::builder()
            .method("GET")
            .uri(route)
            .body(Body::empty())
            .unwrap();
        let (status, _body) = common::oneshot_bytes(&app, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    daemon
        .cache_rehydration_finish_workspace_delete_for_test(seed.workspace.id)
        .await;
}

#[tokio::test]
async fn include_events_false_subagent_heads_fall_back_to_store_after_cold_delta() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_primary_and_subagent_for_test()
        .await
        .unwrap();
    let subagent = &seed.subagent;
    let turn = daemon
        .cache_rehydration_seed_completed_notice_for_test(
            subagent,
            seed.task.id,
            serde_json::json!({"msg":"subagent durable history"}),
            Some("subagent answer"),
        )
        .await
        .unwrap();
    daemon
        .cache_rehydration_publish_cold_running_delta_for_test(subagent, &turn)
        .await;
    assert!(
        daemon
            .cache_rehydration_replay_session_head_cached_for_test(subagent.id)
            .await
            .is_none(),
        "cold subagent delta should not synthesize an in-memory head"
    );

    let app = fixture.router();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/sessions/{}/head?limit=60", subagent.id.0))
        .body(Body::empty())
        .unwrap();
    let (status, head): (StatusCode, SessionHeadSnapshot) = common::oneshot_json(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(head.session.id, subagent.id);
    assert_eq!(head.last_event_seq, turn.event.seq);
    assert_eq!(head.messages.len(), 1);
    assert_eq!(head.messages[0].content, "subagent answer");
}

#[tokio::test]
async fn unarchive_repopulates_active_heads_for_hydrated_workspace() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_session_for_test(false, true)
        .await
        .unwrap();
    let workspace = &seed.workspace;
    let task = &seed.task;
    let session = &seed.session;
    daemon
        .cache_rehydration_seed_completed_notice_for_test(
            session,
            task.id,
            serde_json::json!({"note": "seed"}),
            None,
        )
        .await
        .unwrap();
    let active_summary = daemon
        .cache_rehydration_active_task_summary_for_test(task.id)
        .await
        .unwrap();
    let head = daemon
        .cache_rehydration_full_head_for_test(session.id, 60, true)
        .await
        .unwrap();
    daemon
        .cache_rehydration_hydrate_snapshot_for_test(
            workspace.id,
            1,
            0,
            vec![active_summary],
            vec![head],
        )
        .await;

    let app = fixture.router();

    let heads_req = Request::builder()
        .method("GET")
        .uri(format!("/api/workspaces/{}/active_heads", workspace.id.0))
        .body(Body::empty())
        .unwrap();
    let (heads_status, heads): (StatusCode, WorkspaceActiveHeadBatch) =
        common::oneshot_json(&app, heads_req).await;
    assert_eq!(heads_status, StatusCode::OK);
    assert_eq!(heads.heads.len(), 1);
    assert_eq!(heads.heads[0].session.id, session.id);

    let archive_req = Request::builder()
        .method("POST")
        .uri(format!("/api/tasks/{}/archive", task.id.0))
        .body(Body::empty())
        .unwrap();
    let (archive_status, _): (StatusCode, serde_json::Value) =
        common::oneshot_json(&app, archive_req).await;
    assert_eq!(archive_status, StatusCode::OK);

    let unarchive_req = Request::builder()
        .method("POST")
        .uri(format!("/api/tasks/{}/unarchive", task.id.0))
        .body(Body::empty())
        .unwrap();
    let (unarchive_status, _): (StatusCode, serde_json::Value) =
        common::oneshot_json(&app, unarchive_req).await;
    assert_eq!(unarchive_status, StatusCode::OK);
    let durable_head = daemon
        .cache_rehydration_active_head_for_test(session.id)
        .await
        .unwrap();
    assert!(durable_head.is_some());
    let active_task = daemon
        .cache_rehydration_active_task_summary_cached_for_test(workspace.id, task.id)
        .await;
    assert!(active_task.is_some());

    let heads_req = Request::builder()
        .method("GET")
        .uri(format!("/api/workspaces/{}/active_heads", workspace.id.0))
        .body(Body::empty())
        .unwrap();
    let (heads_status, heads): (StatusCode, WorkspaceActiveHeadBatch) =
        common::oneshot_json(&app, heads_req).await;
    assert_eq!(heads_status, StatusCode::OK);
    assert_eq!(heads.heads.len(), 1);
    assert_eq!(heads.heads[0].session.id, session.id);
}

#[tokio::test]
async fn unarchive_replaces_stale_session_head_cache_before_workspace_hydration() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_session_for_test(false, true)
        .await
        .unwrap();
    let workspace = &seed.workspace;
    let task = &seed.task;
    let session = &seed.session;
    daemon
        .cache_rehydration_seed_completed_notice_for_test(
            session,
            task.id,
            serde_json::json!({"note": "seed"}),
            None,
        )
        .await
        .unwrap();

    let app = fixture.router();

    let archive_req = Request::builder()
        .method("POST")
        .uri(format!("/api/tasks/{}/archive", task.id.0))
        .body(Body::empty())
        .unwrap();
    let (archive_status, _): (StatusCode, serde_json::Value) =
        common::oneshot_json(&app, archive_req).await;
    assert_eq!(archive_status, StatusCode::OK);

    let archived_head_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/sessions/{}/head?include_events=true&limit=60",
            session.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let (head_status, _head): (StatusCode, SessionHeadSnapshot) =
        common::oneshot_json(&app, archived_head_req).await;
    assert_eq!(head_status, StatusCode::OK);
    assert!(daemon
        .cache_rehydration_replay_session_head_cached_for_test(session.id)
        .await
        .is_some());
    assert!(
        daemon
            .cache_rehydration_workspace_needs_hydration_for_test(workspace.id)
            .await
    );

    let unarchive_req = Request::builder()
        .method("POST")
        .uri(format!("/api/tasks/{}/unarchive", task.id.0))
        .body(Body::empty())
        .unwrap();
    let (unarchive_status, _): (StatusCode, serde_json::Value) =
        common::oneshot_json(&app, unarchive_req).await;
    assert_eq!(unarchive_status, StatusCode::OK);

    assert!(daemon
        .cache_rehydration_replay_session_head_cached_for_test(session.id)
        .await
        .is_none());
    assert!(daemon
        .cache_rehydration_session_head_for_read_cached_for_test(session.id)
        .await
        .is_some());
}

#[tokio::test]
async fn include_events_false_primary_heads_fall_back_to_store_after_cold_delta() {
    let fixture = common::fake_daemon_fixture("http://localhost").await;
    let daemon = &fixture.daemon;
    let seed = daemon
        .seed_cache_rehydration_session_for_test(true, false)
        .await
        .unwrap();
    let primary = &seed.session;
    let turn = daemon
        .cache_rehydration_seed_completed_notice_for_test(
            primary,
            seed.task.id,
            serde_json::json!({"msg":"primary durable history"}),
            Some("primary answer"),
        )
        .await
        .unwrap();
    daemon
        .cache_rehydration_publish_cold_running_delta_for_test(primary, &turn)
        .await;
    assert!(
        daemon
            .cache_rehydration_replay_session_head_cached_for_test(primary.id)
            .await
            .is_none(),
        "cold primary delta should stay unservable until the store-backed head is loaded"
    );

    let app = fixture.router();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/sessions/{}/head?limit=60", primary.id.0))
        .body(Body::empty())
        .unwrap();
    let (status, head): (StatusCode, SessionHeadSnapshot) = common::oneshot_json(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(head.session.id, primary.id);
    assert_eq!(head.last_event_seq, turn.event.seq);
    assert_eq!(head.messages.len(), 1);
    assert_eq!(head.messages[0].content, "primary answer");

    let cached = daemon
        .cache_rehydration_session_head_for_read_cached_for_test(primary.id)
        .await
        .expect("store-backed read should hydrate the compact per-session head cache");
    assert_eq!(cached.last_event_seq, turn.event.seq);
    assert_eq!(cached.messages.len(), 1);
    assert_eq!(cached.messages[0].content, "primary answer");
    assert!(
        daemon
            .cache_rehydration_replay_session_head_cached_for_test(primary.id)
            .await
            .is_none(),
        "include_events=false reads must not seed replay history from an event-stripped head"
    );
}

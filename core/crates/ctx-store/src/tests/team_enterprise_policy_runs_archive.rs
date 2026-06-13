use super::{setup_session_fixture, sqlite_url};
use chrono::Utc;
use ctx_core::ids::{MessageId, RunId, SessionId, TaskId, TurnId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    ArchiveVisibility, AuditActor, AuditActorKind, AuditEvent, AuditEventKind, Message,
    MessageDelivery, MessageRole, RetentionPolicyRef, RunArchiveIngestScope, RunArchiveState,
    RunRecord, RunStatus, SessionEventType,
};
use sqlx::{Row, SqlitePool};

#[tokio::test]
async fn runs_archive_audit_migration_backfills_existing_run_ids() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    std::fs::File::create(&db_path).unwrap();
    let pool = SqlitePool::connect(&sqlite_url(&db_path)).await.unwrap();

    sqlx::query(r#"CREATE TABLE workspaces (id TEXT PRIMARY KEY NOT NULL)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"CREATE TABLE tasks (id TEXT PRIMARY KEY NOT NULL)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"CREATE TABLE worktrees (id TEXT PRIMARY KEY NOT NULL)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"CREATE TABLE sessions (
            id TEXT PRIMARY KEY NOT NULL,
            task_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            worktree_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TABLE session_turns (
            turn_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL,
            run_id TEXT,
            status TEXT NOT NULL,
            started_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TABLE session_events (
            seq INTEGER PRIMARY KEY,
            session_id TEXT NOT NULL,
            run_id TEXT,
            event_type TEXT NOT NULL,
            created_at TEXT NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TABLE messages (
            id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL,
            run_id TEXT,
            created_at TEXT NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TABLE subagent_invocation_children (
            child_session_id TEXT NOT NULL,
            run_id TEXT
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let workspace_id = WorkspaceId::new().0.to_string();
    let task_id = TaskId::new().0.to_string();
    let worktree_id = WorktreeId::new().0.to_string();
    let session_id = SessionId::new().0.to_string();
    let turn_id = TurnId::new().0.to_string();
    let run_id = RunId::new().0.to_string();

    sqlx::query("INSERT INTO workspaces (id) VALUES (?)")
        .bind(&workspace_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks (id) VALUES (?)")
        .bind(&task_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO worktrees (id) VALUES (?)")
        .bind(&worktree_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO sessions (id, task_id, workspace_id, worktree_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&session_id)
    .bind(&task_id)
    .bind(&workspace_id)
    .bind(&worktree_id)
    .bind("2026-04-24T09:00:00Z")
    .bind("2026-04-24T09:06:00Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO session_turns (turn_id, session_id, run_id, status, started_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&turn_id)
    .bind(&session_id)
    .bind(&run_id)
    .bind("completed")
    .bind("2026-04-24T09:01:00Z")
    .bind("2026-04-24T09:05:00Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO session_events (seq, session_id, run_id, event_type, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(1_i64)
    .bind(&session_id)
    .bind(&run_id)
    .bind("done")
    .bind("2026-04-24T09:05:00Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO messages (id, session_id, run_id, created_at) VALUES (?, ?, ?, ?)")
        .bind(MessageId::new().0.to_string())
        .bind(&session_id)
        .bind(&run_id)
        .bind("2026-04-24T09:04:00Z")
        .execute(&pool)
        .await
        .unwrap();

    for migration_sql in [
        include_str!("../../migrations/0069_org_policy_and_run_grants.sql"),
        include_str!("../../migrations/0070_runs_archive_audit.sql"),
    ] {
        for statement in migration_sql
            .split(";\n\n")
            .map(str::trim)
            .filter(|statement| !statement.is_empty())
        {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
    }

    let row = sqlx::query(
        r#"SELECT session_id, task_id, workspace_id, worktree_id, status, archive_state,
                  archive_visibility, started_at, completed_at
           FROM runs
           WHERE id = ?"#,
    )
    .bind(&run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.try_get::<String, _>("session_id").unwrap(), session_id);
    assert_eq!(row.try_get::<String, _>("task_id").unwrap(), task_id);
    assert_eq!(
        row.try_get::<String, _>("workspace_id").unwrap(),
        workspace_id
    );
    assert_eq!(
        row.try_get::<String, _>("worktree_id").unwrap(),
        worktree_id
    );
    assert_eq!(row.try_get::<String, _>("status").unwrap(), "completed");
    assert_eq!(row.try_get::<String, _>("archive_state").unwrap(), "active");
    assert_eq!(
        row.try_get::<String, _>("archive_visibility").unwrap(),
        "local_only"
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("started_at")
            .unwrap()
            .as_deref(),
        Some("2026-04-24T09:01:00Z")
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("completed_at")
            .unwrap()
            .as_deref(),
        Some("2026-04-24T09:05:00Z")
    );

    let audit_table_exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'run_audit_events')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audit_table_exists, 1);

    pool.close().await;
}

#[tokio::test]
async fn run_record_round_trip_keeps_archive_state_separate_from_visibility() {
    let fixture = setup_session_fixture().await;
    let now = Utc::now();
    let run_id = RunId::new();
    let account_id = ctx_core::ids::AccountId::new();
    let org_id = ctx_core::ids::OrgId::new();

    let initial = RunRecord {
        id: run_id,
        session_id: fixture.session_id,
        task_id: fixture.task_id,
        workspace_id: fixture.workspace_id,
        worktree_id: fixture.worktree_id,
        parent_run_id: None,
        account_id: Some(account_id),
        org_id: Some(org_id),
        run_grant_id: None,
        status: RunStatus::Running,
        archive_state: RunArchiveState::Active,
        archive_visibility: ArchiveVisibility::OrgTranscript,
        retention_policy: Some(RetentionPolicyRef {
            policy_key: "team-default".into(),
            legal_hold_key: Some("hold-77".into()),
        }),
        created_at: now,
        started_at: Some(now),
        completed_at: None,
        archived_at: None,
        updated_at: now,
    };

    fixture.store.upsert_run(initial.clone()).await.unwrap();

    let mut archived = initial.clone();
    archived.status = RunStatus::Completed;
    archived.archive_state = RunArchiveState::Archived;
    archived.completed_at = Some(now);
    archived.archived_at = Some(now);
    archived.updated_at = now;
    fixture.store.upsert_run(archived.clone()).await.unwrap();

    let stored = fixture.store.get_run(run_id).await.unwrap().unwrap();
    assert_eq!(stored.archive_state, RunArchiveState::Archived);
    assert_eq!(stored.archive_visibility, ArchiveVisibility::OrgTranscript);
    assert_eq!(
        stored
            .retention_policy
            .as_ref()
            .map(|policy| policy.policy_key.as_str()),
        Some("team-default")
    );
    assert_eq!(
        stored
            .retention_policy
            .as_ref()
            .and_then(|policy| policy.legal_hold_key.as_deref()),
        Some("hold-77")
    );

    let audit = AuditEvent {
        id: uuid::Uuid::new_v4().to_string(),
        workspace_id: fixture.workspace_id,
        task_id: Some(fixture.task_id),
        session_id: Some(fixture.session_id),
        run_id: Some(run_id),
        account_id: Some(account_id),
        org_id: Some(org_id),
        actor: AuditActor {
            kind: AuditActorKind::Account,
            account_id: Some(account_id),
            org_id: Some(org_id),
            membership_role: Some("admin".into()),
        },
        event_kind: AuditEventKind::HistoryAccessed,
        archive_visibility: Some(ArchiveVisibility::OrgTranscript),
        retention_policy: stored.retention_policy.clone(),
        payload_json: serde_json::json!({ "surface": "desktop", "action": "view" }),
        created_at: now,
    };
    fixture
        .store
        .append_run_audit_event(audit.clone())
        .await
        .unwrap();

    let audit_events = fixture.store.list_run_audit_events(run_id).await.unwrap();
    assert_eq!(audit_events, vec![audit]);
}

#[tokio::test]
async fn run_archive_ingest_batch_gates_visibility_and_normalizes_payloads() {
    let fixture = setup_session_fixture().await;
    let now = Utc::now();
    let run_id = RunId::new();
    let account_id = ctx_core::ids::AccountId::new();
    let org_id = ctx_core::ids::OrgId::new();
    let turn_id = TurnId::new();

    fixture
        .store
        .upsert_run(RunRecord {
            id: run_id,
            session_id: fixture.session_id,
            task_id: fixture.task_id,
            workspace_id: fixture.workspace_id,
            worktree_id: fixture.worktree_id,
            parent_run_id: None,
            account_id: Some(account_id),
            org_id: Some(org_id),
            run_grant_id: None,
            status: RunStatus::Completed,
            archive_state: RunArchiveState::Archived,
            archive_visibility: ArchiveVisibility::OrgEvidence,
            retention_policy: Some(RetentionPolicyRef {
                policy_key: "team-30d".into(),
                legal_hold_key: None,
            }),
            created_at: now,
            started_at: Some(now),
            completed_at: Some(now),
            archived_at: Some(now),
            updated_at: now,
        })
        .await
        .unwrap();

    fixture
        .store
        .insert_message(Message {
            id: MessageId::new(),
            session_id: fixture.session_id,
            task_id: fixture.task_id,
            run_id: Some(run_id),
            turn_id: Some(turn_id),
            turn_sequence: Some(1),
            order_seq: None,
            role: MessageRole::Assistant,
            content: "Touched /home/fixture/src/ctx/.env with sk-12345678901234567890".into(),
            attachments: vec![],
            delivery: MessageDelivery::Immediate,
            delivered_at: None,
            created_at: now,
        })
        .await
        .unwrap();

    fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({
                "kind": "archive_evidence",
                "absolute_path": "/home/fixture/src/ctx/secret.txt",
                "provider_session_ref": "provider-thread-secret",
                "api_key": "sk-abcdefghijklmnopqrstuvwxyz",
                "pty_byte_stream": "raw terminal bytes"
            }),
        )
        .await
        .unwrap();
    fixture
        .store
        .flush_active_snapshot_head_projection_queue()
        .await
        .unwrap();

    fixture
        .store
        .append_run_audit_event(AuditEvent {
            id: uuid::Uuid::new_v4().to_string(),
            workspace_id: fixture.workspace_id,
            task_id: Some(fixture.task_id),
            session_id: Some(fixture.session_id),
            run_id: Some(run_id),
            account_id: Some(account_id),
            org_id: Some(org_id),
            actor: AuditActor {
                kind: AuditActorKind::Account,
                account_id: Some(account_id),
                org_id: Some(org_id),
                membership_role: Some("admin".into()),
            },
            event_kind: AuditEventKind::ArchiveVisibilityChanged,
            archive_visibility: Some(ArchiveVisibility::OrgEvidence),
            retention_policy: Some(RetentionPolicyRef {
                policy_key: "team-30d".into(),
                legal_hold_key: None,
            }),
            payload_json: serde_json::json!({
                "old_path": "/tmp/raw/worktree",
                "credential": "secret-token",
                "provider_response_id": "resp_private"
            }),
            created_at: now,
        })
        .await
        .unwrap();

    let batch = fixture
        .store
        .build_run_archive_ingest_batch(run_id, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(batch.scope, RunArchiveIngestScope::Evidence);
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.session_events.len(), 1);
    assert_eq!(batch.audit_events.len(), 1);
    assert!(batch.to.session_event_seq > batch.from.session_event_seq);
    assert!(batch.to.audit_event_seq > batch.from.audit_event_seq);

    let serialized = serde_json::to_string(&batch).unwrap();
    for forbidden in [
        "/home/fixture",
        "/tmp/raw",
        "sk-abcdefghijklmnopqrstuvwxyz",
        "sk-12345678901234567890",
        "provider-thread-secret",
        "resp_private",
        "raw terminal bytes",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "archive ingest batch leaked {forbidden}: {serialized}"
        );
    }
    assert!(serialized.contains("[redacted:absolute_path]"));
    assert!(serialized.contains("[redacted:provider_ref]"));
    assert!(serialized.contains("[redacted:secret]"));
    assert!(serialized.contains("[redacted:pty_stream]"));

    let cursor = fixture
        .store
        .acknowledge_run_archive_ingest_batch(&batch)
        .await
        .unwrap();
    assert_eq!(cursor.watermark, batch.to);
    assert_eq!(
        cursor.last_batch_id.as_deref(),
        Some(batch.idempotency_key.as_str())
    );

    fixture
        .store
        .acknowledge_run_archive_ingest_batch(&batch)
        .await
        .unwrap();
    assert!(fixture
        .store
        .build_run_archive_ingest_batch(run_id, 100)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn run_archive_ingest_batch_omits_non_org_visible_runs() {
    let fixture = setup_session_fixture().await;
    let now = Utc::now();
    let run_id = RunId::new();

    fixture
        .store
        .upsert_run(RunRecord {
            id: run_id,
            session_id: fixture.session_id,
            task_id: fixture.task_id,
            workspace_id: fixture.workspace_id,
            worktree_id: fixture.worktree_id,
            parent_run_id: None,
            account_id: Some(ctx_core::ids::AccountId::new()),
            org_id: Some(ctx_core::ids::OrgId::new()),
            run_grant_id: None,
            status: RunStatus::Completed,
            archive_state: RunArchiveState::Archived,
            archive_visibility: ArchiveVisibility::AccountPrivate,
            retention_policy: None,
            created_at: now,
            started_at: Some(now),
            completed_at: Some(now),
            archived_at: Some(now),
            updated_at: now,
        })
        .await
        .unwrap();

    assert!(fixture
        .store
        .build_run_archive_ingest_batch(run_id, 100)
        .await
        .unwrap()
        .is_none());
}

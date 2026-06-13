use ctx_core::ids::{RunId, WorkspaceId};
use ctx_core::models::{RunArchiveIngestBatch, RunArchiveIngestCursor};
use ctx_store::Store;

#[derive(Debug)]
pub enum RunArchiveIngestError {
    AcknowledgementConflict(&'static str),
    Internal(anyhow::Error),
}

pub async fn build_run_archive_ingest_batch(
    store: &Store,
    workspace_id: WorkspaceId,
    run_id: RunId,
    max_items: u32,
) -> Result<Option<RunArchiveIngestBatch>, RunArchiveIngestError> {
    let batch = store
        .build_run_archive_ingest_batch(run_id, max_items)
        .await
        .map_err(RunArchiveIngestError::Internal)?;
    Ok(batch.filter(|batch| batch.run.workspace_id == workspace_id))
}

pub async fn acknowledge_run_archive_ingest_batch(
    store: &Store,
    run_id: RunId,
    max_items: u32,
    batch: RunArchiveIngestBatch,
) -> Result<RunArchiveIngestCursor, RunArchiveIngestError> {
    let cursor = store
        .get_run_archive_ingest_cursor(run_id)
        .await
        .map_err(RunArchiveIngestError::Internal)?;
    let current_watermark = cursor
        .as_ref()
        .map(|cursor| cursor.watermark)
        .unwrap_or_default();
    if batch.from != current_watermark {
        return Err(RunArchiveIngestError::AcknowledgementConflict(
            "archive ingest acknowledgement is stale for the current cursor",
        ));
    }
    let Some(mut expected_batch) = store
        .build_run_archive_ingest_batch_after(run_id, batch.from, max_items, cursor.is_none())
        .await
        .map_err(RunArchiveIngestError::Internal)?
    else {
        return Err(RunArchiveIngestError::AcknowledgementConflict(
            "archive ingest acknowledgement does not match an available batch",
        ));
    };
    expected_batch.created_at = batch.created_at;
    if expected_batch != batch {
        return Err(RunArchiveIngestError::AcknowledgementConflict(
            "archive ingest acknowledgement does not match the current batch",
        ));
    }
    store
        .acknowledge_run_archive_ingest_batch(&batch)
        .await
        .map_err(RunArchiveIngestError::Internal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ctx_core::ids::{AccountId, MessageId, OrgId, RunId, TurnId, WorkspaceId};
    use ctx_core::models::{
        ArchiveVisibility, AuditActor, AuditActorKind, AuditEvent, AuditEventKind,
        ExecutionEnvironment, Message, MessageDelivery, MessageRole, RetentionPolicyRef,
        RunArchiveState, RunRecord, RunStatus, SessionEventType, VcsKind,
    };

    struct RunArchiveFixture {
        _dir: tempfile::TempDir,
        store: Store,
        workspace_id: WorkspaceId,
        run_id: RunId,
    }

    async fn setup_org_visible_run_archive_fixture() -> RunArchiveFixture {
        let dir = tempfile::tempdir().expect("create tempdir");
        let db_path = dir.path().join("db.sqlite");
        let store = Store::open(&db_path).await.expect("open store");
        let workspace = store
            .create_workspace("test".into(), "/tmp/test".into(), VcsKind::Git)
            .await
            .expect("create workspace");
        let task = store
            .create_task(workspace.id, "archive run".into(), None)
            .await
            .expect("create task");
        let worktree = store
            .create_worktree(workspace.id, "/tmp/test".into(), "deadbeef".into(), None)
            .await
            .expect("create worktree");
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                "fake".into(),
                "fake".into(),
                "implementer".into(),
                None,
                None,
                None,
            )
            .await
            .expect("create session");

        let now = Utc::now();
        let run_id = RunId::new();
        let account_id = AccountId::new();
        let org_id = OrgId::new();
        let turn_id = TurnId::new();
        store
            .upsert_run(RunRecord {
                id: run_id,
                session_id: session.id,
                task_id: task.id,
                workspace_id: workspace.id,
                worktree_id: worktree.id,
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
            .expect("upsert run");
        store
            .insert_message(Message {
                id: MessageId::new(),
                session_id: session.id,
                task_id: task.id,
                run_id: Some(run_id),
                turn_id: Some(turn_id),
                turn_sequence: Some(1),
                order_seq: None,
                role: MessageRole::Assistant,
                content: "archive payload".into(),
                attachments: vec![],
                delivery: MessageDelivery::Immediate,
                delivered_at: None,
                created_at: now,
            })
            .await
            .expect("insert message");
        store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::Notice,
                serde_json::json!({"kind": "archive_evidence"}),
            )
            .await
            .expect("append session event");
        store
            .flush_active_snapshot_head_projection_queue()
            .await
            .expect("flush head projection");
        store
            .append_run_audit_event(AuditEvent {
                id: uuid::Uuid::new_v4().to_string(),
                workspace_id: workspace.id,
                task_id: Some(task.id),
                session_id: Some(session.id),
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
                retention_policy: None,
                payload_json: serde_json::json!({"kind": "archive"}),
                created_at: now,
            })
            .await
            .expect("append audit event");

        RunArchiveFixture {
            _dir: dir,
            store,
            workspace_id: workspace.id,
            run_id,
        }
    }

    #[tokio::test]
    async fn build_filters_batches_for_other_workspaces() {
        let fixture = setup_org_visible_run_archive_fixture().await;

        let batch =
            build_run_archive_ingest_batch(&fixture.store, WorkspaceId::new(), fixture.run_id, 100)
                .await
                .expect("build batch");

        assert!(batch.is_none());
    }

    #[tokio::test]
    async fn acknowledgement_accepts_current_batch_and_rejects_stale_replay() {
        let fixture = setup_org_visible_run_archive_fixture().await;
        let batch = build_run_archive_ingest_batch(
            &fixture.store,
            fixture.workspace_id,
            fixture.run_id,
            100,
        )
        .await
        .expect("build batch")
        .expect("batch");

        let cursor = acknowledge_run_archive_ingest_batch(
            &fixture.store,
            fixture.run_id,
            100,
            batch.clone(),
        )
        .await
        .expect("acknowledge batch");
        assert_eq!(cursor.watermark, batch.to);

        let error =
            acknowledge_run_archive_ingest_batch(&fixture.store, fixture.run_id, 100, batch)
                .await
                .expect_err("stale batch must conflict");
        assert!(matches!(
            error,
            RunArchiveIngestError::AcknowledgementConflict(
                "archive ingest acknowledgement is stale for the current cursor"
            )
        ));
    }
}

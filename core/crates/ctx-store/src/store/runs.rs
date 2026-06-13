use super::*;

mod archive_ingest;
mod rows;

use self::rows::{build_audit_event_from_row, build_run_record_from_row};

impl Store {
    pub async fn upsert_run(&self, run: RunRecord) -> Result<RunRecord> {
        let (retention_policy_key, retention_legal_hold_key) = run
            .retention_policy
            .as_ref()
            .map(|retention| {
                (
                    Some(retention.policy_key.clone()),
                    retention.legal_hold_key.clone(),
                )
            })
            .unwrap_or((None, None));

        self.query(
            r#"INSERT INTO runs (
                   id,
                   session_id,
                   task_id,
                   workspace_id,
                   worktree_id,
                   parent_run_id,
                   account_id,
                   org_id,
                   run_grant_id,
                   status,
                   archive_state,
                   archive_visibility,
                   retention_policy_key,
                   retention_legal_hold_key,
                   created_at,
                   started_at,
                   completed_at,
                   archived_at,
                   updated_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                   session_id = excluded.session_id,
                   task_id = excluded.task_id,
                   workspace_id = excluded.workspace_id,
                   worktree_id = excluded.worktree_id,
                   parent_run_id = excluded.parent_run_id,
                   account_id = excluded.account_id,
                   org_id = excluded.org_id,
                   run_grant_id = excluded.run_grant_id,
                   status = excluded.status,
                   archive_state = excluded.archive_state,
                   archive_visibility = excluded.archive_visibility,
                   retention_policy_key = excluded.retention_policy_key,
                   retention_legal_hold_key = excluded.retention_legal_hold_key,
                   created_at = excluded.created_at,
                   started_at = excluded.started_at,
                   completed_at = excluded.completed_at,
                   archived_at = excluded.archived_at,
                   updated_at = excluded.updated_at"#,
        )
        .bind(run.id.0.to_string())
        .bind(run.session_id.0.to_string())
        .bind(run.task_id.0.to_string())
        .bind(run.workspace_id.0.to_string())
        .bind(run.worktree_id.0.to_string())
        .bind(run.parent_run_id.map(|id| id.0.to_string()))
        .bind(run.account_id.map(|id| id.0.to_string()))
        .bind(run.org_id.map(|id| id.0.to_string()))
        .bind(run.run_grant_id.map(|id| id.0.to_string()))
        .bind(run.status.as_str())
        .bind(run.archive_state.as_str())
        .bind(run.archive_visibility.as_str())
        .bind(retention_policy_key)
        .bind(retention_legal_hold_key)
        .bind(run.created_at.to_rfc3339())
        .bind(run.started_at.map(|dt| dt.to_rfc3339()))
        .bind(run.completed_at.map(|dt| dt.to_rfc3339()))
        .bind(run.archived_at.map(|dt| dt.to_rfc3339()))
        .bind(run.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(run)
    }

    pub async fn get_run(&self, run_id: RunId) -> Result<Option<RunRecord>> {
        let row = self
            .query(
                r#"SELECT id, session_id, task_id, workspace_id, worktree_id, parent_run_id,
                          account_id, org_id, run_grant_id, status, archive_state,
                          archive_visibility, retention_policy_key, retention_legal_hold_key,
                          created_at, started_at, completed_at, archived_at, updated_at
                   FROM runs
                   WHERE id = ?"#,
            )
            .bind(run_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(build_run_record_from_row).transpose()
    }

    pub async fn update_run_status(
        &self,
        run_id: RunId,
        status: RunStatus,
        completed_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        let now = chrono::Utc::now();
        self.query(
            r#"UPDATE runs
               SET status = ?,
                   completed_at = COALESCE(?, completed_at),
                   updated_at = ?
               WHERE id = ?"#,
        )
        .bind(status.as_str())
        .bind(completed_at.map(|value| value.to_rfc3339()))
        .bind(now.to_rfc3339())
        .bind(run_id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn append_run_audit_event(&self, event: AuditEvent) -> Result<AuditEvent> {
        let (retention_policy_key, retention_legal_hold_key) = event
            .retention_policy
            .as_ref()
            .map(|retention| {
                (
                    Some(retention.policy_key.clone()),
                    retention.legal_hold_key.clone(),
                )
            })
            .unwrap_or((None, None));

        let _write_guard = self.write_gate.lock().await;
        let mut tx = self.pool.begin().await?;
        let ingest_seq: i64 = self
            .query_scalar(
                r#"SELECT next_seq
                   FROM run_archive_ingest_sequence
                   WHERE id = 1"#,
            )
            .fetch_one(&mut *tx)
            .await?;
        self.query(
            r#"UPDATE run_archive_ingest_sequence
               SET next_seq = ?
               WHERE id = 1"#,
        )
        .bind(ingest_seq + 1)
        .execute(&mut *tx)
        .await?;

        self.query(
            r#"INSERT INTO run_audit_events (
                   id,
                   workspace_id,
                   task_id,
                   session_id,
                   run_id,
                   account_id,
                   org_id,
                   actor_kind,
                   actor_account_id,
                   actor_org_id,
                   actor_membership_role,
                   event_kind,
                   archive_visibility,
                   retention_policy_key,
                   retention_legal_hold_key,
                   payload_json,
                   created_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&event.id)
        .bind(event.workspace_id.0.to_string())
        .bind(event.task_id.map(|id| id.0.to_string()))
        .bind(event.session_id.map(|id| id.0.to_string()))
        .bind(event.run_id.map(|id| id.0.to_string()))
        .bind(event.account_id.map(|id| id.0.to_string()))
        .bind(event.org_id.map(|id| id.0.to_string()))
        .bind(event.actor.kind.as_str())
        .bind(event.actor.account_id.map(|id| id.0.to_string()))
        .bind(event.actor.org_id.map(|id| id.0.to_string()))
        .bind(event.actor.membership_role.clone())
        .bind(event.event_kind.as_str())
        .bind(
            event
                .archive_visibility
                .map(|visibility| visibility.as_str()),
        )
        .bind(retention_policy_key)
        .bind(retention_legal_hold_key)
        .bind(event.payload_json.to_string())
        .bind(event.created_at.to_rfc3339())
        .execute(&mut *tx)
        .await?;

        self.query(
            r#"INSERT INTO run_audit_event_ingest_sequences (
                   audit_event_id,
                   run_id,
                   ingest_seq,
                   created_at
               )
               VALUES (?, ?, ?, ?)"#,
        )
        .bind(&event.id)
        .bind(event.run_id.map(|id| id.0.to_string()))
        .bind(ingest_seq)
        .bind(event.created_at.to_rfc3339())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(event)
    }

    pub async fn list_run_audit_events(&self, run_id: RunId) -> Result<Vec<AuditEvent>> {
        let rows = self
            .query(
                r#"SELECT id, workspace_id, task_id, session_id, run_id, account_id, org_id,
                          actor_kind, actor_account_id, actor_org_id, actor_membership_role,
                          event_kind, archive_visibility, retention_policy_key,
                          retention_legal_hold_key, payload_json, created_at
                   FROM run_audit_events
                   WHERE run_id = ?
                   ORDER BY created_at ASC, id ASC"#,
            )
            .bind(run_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter().map(build_audit_event_from_row).collect()
    }
}

use super::rows::{
    build_message_from_row, build_run_archive_ingest_cursor_from_row,
    build_sequenced_audit_event_from_row, build_session_event_from_row, SequencedAuditEvent,
};
use super::*;

impl Store {
    pub async fn get_run_archive_ingest_cursor(
        &self,
        run_id: RunId,
    ) -> Result<Option<RunArchiveIngestCursor>> {
        let row = self
            .query(
                r#"SELECT run_id, workspace_id, org_id, archive_visibility,
                          retention_policy_key, retention_legal_hold_key,
                          last_session_event_seq, last_audit_event_seq, last_batch_id,
                          last_synced_at, updated_at
                   FROM run_archive_ingest_cursors
                   WHERE run_id = ?"#,
            )
            .bind(run_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(build_run_archive_ingest_cursor_from_row)
            .transpose()
    }

    pub async fn build_run_archive_ingest_batch(
        &self,
        run_id: RunId,
        max_items: u32,
    ) -> Result<Option<RunArchiveIngestBatch>> {
        let cursor = self.get_run_archive_ingest_cursor(run_id).await?;
        let from = cursor
            .as_ref()
            .map(|cursor| cursor.watermark)
            .unwrap_or_default();
        self.build_run_archive_ingest_batch_after(run_id, from, max_items, cursor.is_none())
            .await
    }

    pub async fn build_run_archive_ingest_batch_after(
        &self,
        run_id: RunId,
        from: RunArchiveIngestWatermark,
        max_items: u32,
        force_run_snapshot: bool,
    ) -> Result<Option<RunArchiveIngestBatch>> {
        self.flush_event_log_for_reads().await;

        let Some(run) = self.get_run(run_id).await? else {
            return Ok(None);
        };
        let scope = RunArchiveIngestScope::from_visibility(run.archive_visibility);
        if !scope.is_cloud_visible() || run.org_id.is_none() {
            return Ok(None);
        }
        let Some(session) = self.get_session(run.session_id).await? else {
            return Ok(None);
        };

        let limit = i64::from(max_items.max(1));
        let raw_events = self
            .list_run_session_events_after(run.session_id, run.id, from.session_event_seq, limit)
            .await?;
        let raw_audit_events = self
            .list_run_audit_events_after(run.id, from.audit_event_seq, limit)
            .await?;

        if !force_run_snapshot && raw_events.is_empty() && raw_audit_events.is_empty() {
            return Ok(None);
        }

        let mut normalization = RunArchiveNormalizationStats::default();
        let mut to = from;
        let mut session_events = Vec::with_capacity(raw_events.len());
        for event in raw_events {
            to.session_event_seq = to.session_event_seq.max(event.seq);
            if let Some((normalized, stats)) = normalize_session_event_for_archive(&event, scope) {
                normalization.merge(stats);
                session_events.push(normalized);
            }
        }

        let mut audit_events = Vec::with_capacity(raw_audit_events.len());
        for sequenced in raw_audit_events {
            to.audit_event_seq = to.audit_event_seq.max(sequenced.ingest_seq);
            let normalized_payload = normalize_archive_json(&sequenced.event.payload_json);
            normalization.merge(normalized_payload.stats);
            audit_events.push(RunArchiveIngestAuditEvent {
                ingest_seq: sequenced.ingest_seq,
                id: sequenced.event.id,
                workspace_id: sequenced.event.workspace_id,
                task_id: sequenced.event.task_id,
                session_id: sequenced.event.session_id,
                run_id: sequenced.event.run_id,
                account_id: sequenced.event.account_id,
                org_id: sequenced.event.org_id,
                actor: sequenced.event.actor,
                event_kind: sequenced.event.event_kind,
                archive_visibility: sequenced.event.archive_visibility,
                retention_policy: sequenced.event.retention_policy,
                payload_json: normalized_payload.value,
                created_at: sequenced.event.created_at,
            });
        }

        let messages = if scope.includes_transcript() {
            let raw_messages = self.list_run_messages(run.session_id, run.id).await?;
            let mut out = Vec::with_capacity(raw_messages.len());
            for message in raw_messages {
                let normalized = normalize_archive_text(&message.content);
                normalization.merge(normalized.stats);
                out.push(RunArchiveIngestMessage {
                    id: message.id,
                    session_id: message.session_id,
                    task_id: message.task_id,
                    run_id: run.id,
                    turn_id: message.turn_id,
                    turn_sequence: message.turn_sequence,
                    role: message.role,
                    content: normalized.text,
                    created_at: message.created_at,
                });
            }
            out
        } else {
            Vec::new()
        };

        let idempotency_key = format!(
            "run-archive:{}:{}-{}:{}-{}:{}",
            run.id.0,
            from.session_event_seq + 1,
            to.session_event_seq,
            from.audit_event_seq + 1,
            to.audit_event_seq,
            scope.as_str()
        );

        Ok(Some(RunArchiveIngestBatch {
            idempotency_key,
            run: RunArchiveIngestRun {
                id: run.id,
                session_id: run.session_id,
                task_id: run.task_id,
                workspace_id: run.workspace_id,
                worktree_id: run.worktree_id,
                parent_run_id: run.parent_run_id,
                account_id: run.account_id,
                org_id: run.org_id,
                run_grant_id: run.run_grant_id,
                status: run.status,
                archive_state: run.archive_state,
                archive_visibility: run.archive_visibility,
                retention_policy: run.retention_policy,
                provider_id: session.provider_id,
                model_id: session.model_id,
                execution_environment: session.execution_environment.as_str().to_string(),
                created_at: run.created_at,
                started_at: run.started_at,
                completed_at: run.completed_at,
                archived_at: run.archived_at,
                updated_at: run.updated_at,
            },
            scope,
            from,
            to,
            messages,
            session_events,
            audit_events,
            normalization,
            created_at: Utc::now(),
        }))
    }

    pub async fn acknowledge_run_archive_ingest_batch(
        &self,
        batch: &RunArchiveIngestBatch,
    ) -> Result<RunArchiveIngestCursor> {
        let now = Utc::now();
        let retention_policy = batch.run.retention_policy.as_ref();
        self.query(
            r#"INSERT INTO run_archive_ingest_cursors (
                   run_id,
                   workspace_id,
                   org_id,
                   archive_visibility,
                   retention_policy_key,
                   retention_legal_hold_key,
                   last_session_event_seq,
                   last_audit_event_seq,
                   last_batch_id,
                   last_synced_at,
                   updated_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(run_id) DO UPDATE SET
                   workspace_id = excluded.workspace_id,
                   org_id = excluded.org_id,
                   archive_visibility = excluded.archive_visibility,
                   retention_policy_key = excluded.retention_policy_key,
                   retention_legal_hold_key = excluded.retention_legal_hold_key,
                   last_session_event_seq = MAX(
                       run_archive_ingest_cursors.last_session_event_seq,
                       excluded.last_session_event_seq
                   ),
                   last_audit_event_seq = MAX(
                       run_archive_ingest_cursors.last_audit_event_seq,
                       excluded.last_audit_event_seq
                   ),
                   last_batch_id = excluded.last_batch_id,
                   last_synced_at = excluded.last_synced_at,
                   updated_at = excluded.updated_at"#,
        )
        .bind(batch.run.id.0.to_string())
        .bind(batch.run.workspace_id.0.to_string())
        .bind(batch.run.org_id.map(|id| id.0.to_string()))
        .bind(batch.run.archive_visibility.as_str())
        .bind(retention_policy.map(|policy| policy.policy_key.as_str()))
        .bind(retention_policy.and_then(|policy| policy.legal_hold_key.as_deref()))
        .bind(batch.to.session_event_seq)
        .bind(batch.to.audit_event_seq)
        .bind(&batch.idempotency_key)
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;

        self.get_run_archive_ingest_cursor(batch.run.id)
            .await?
            .with_context(|| "run archive ingest cursor missing after acknowledgement".to_string())
    }

    async fn list_run_session_events_after(
        &self,
        session_id: SessionId,
        run_id: RunId,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<SessionEvent>> {
        let rows = self
            .query(
                r#"SELECT seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
                   FROM session_events
                   WHERE session_id = ?
                     AND run_id = ?
                     AND transient = 0
                     AND seq > ?
                   ORDER BY seq ASC
                   LIMIT ?"#,
            )
            .bind(session_id.0.to_string())
            .bind(run_id.0.to_string())
            .bind(after_seq)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter().map(build_session_event_from_row).collect()
    }

    async fn list_run_messages(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Vec<Message>> {
        let rows = self
            .query(
                r#"SELECT id, session_id, task_id, run_id, turn_id, turn_sequence, order_seq,
                          role, content, attachments_json, delivery, delivered_at, created_at
                   FROM messages
                   WHERE session_id = ? AND run_id = ?
                   ORDER BY created_at ASC, turn_sequence ASC, order_seq ASC"#,
            )
            .bind(session_id.0.to_string())
            .bind(run_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter().map(build_message_from_row).collect()
    }

    async fn list_run_audit_events_after(
        &self,
        run_id: RunId,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<SequencedAuditEvent>> {
        let rows = self
            .query(
                r#"SELECT s.ingest_seq, e.id, e.workspace_id, e.task_id, e.session_id, e.run_id,
                          e.account_id, e.org_id, e.actor_kind, e.actor_account_id,
                          e.actor_org_id, e.actor_membership_role, e.event_kind,
                          e.archive_visibility, e.retention_policy_key,
                          e.retention_legal_hold_key, e.payload_json, e.created_at
                   FROM run_audit_events AS e
                   JOIN run_audit_event_ingest_sequences AS s
                     ON s.audit_event_id = e.id
                   WHERE e.run_id = ?
                     AND s.ingest_seq > ?
                   ORDER BY s.ingest_seq ASC
                   LIMIT ?"#,
            )
            .bind(run_id.0.to_string())
            .bind(after_seq)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter()
            .map(build_sequenced_audit_event_from_row)
            .collect()
    }
}

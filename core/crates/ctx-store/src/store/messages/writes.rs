impl Store {
    pub async fn insert_message(&self, mut message: Message) -> Result<Message> {
        if matches!(message.delivery, MessageDelivery::Immediate) && message.delivered_at.is_none()
        {
            message.delivered_at = Some(Utc::now());
        }
        let attachments_json = if message.attachments.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&message.attachments)
                    .context("serializing message attachments")?,
            )
        };
        let id = message.id.0.to_string();
        let session_id = message.session_id.0.to_string();
        let task_id = message.task_id.0.to_string();
        let run_id = message.run_id.map(|r| r.0.to_string());
        let turn_id = message.turn_id.map(|t| t.0.to_string());
        let role = message_role_to_str(&message.role);
        let delivery = message_delivery_to_str(&message.delivery);
        let delivered_at = message.delivered_at.map(|d| d.to_rfc3339());
        let created_at = message.created_at.to_rfc3339();
        let write_bytes = bytes_str(&id)
            + bytes_str(&session_id)
            + bytes_str(&task_id)
            + bytes_opt_str(run_id.as_deref())
            + bytes_opt_str(turn_id.as_deref())
            + bytes_opt_i64(message.turn_sequence)
            + bytes_opt_i64(message.order_seq)
            + bytes_str(role)
            + bytes_str(&message.content)
            + bytes_opt_str(attachments_json.as_deref())
            + bytes_str(delivery)
            + bytes_opt_str(delivered_at.as_deref())
            + bytes_str(&created_at);
        let (message_rows_affected, session_snapshot_write_bytes) = {
            let _write_guard = self.write_gate.lock().await;
            let mut tx = self.pool.begin().await?;
            let message_rows_affected = sqlx::query(
                r#"INSERT INTO messages (id, session_id, task_id, run_id, turn_id, turn_sequence, order_seq, role, content, attachments_json, delivery, delivered_at, created_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .bind(&id)
            .bind(&session_id)
            .bind(&task_id)
            .bind(run_id)
            .bind(turn_id)
            .bind(message.turn_sequence)
            .bind(message.order_seq)
            .bind(role)
            .bind(&message.content)
            .bind(attachments_json)
            .bind(delivery)
            .bind(delivered_at)
            .bind(&created_at)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            let is_assistant = matches!(message.role, MessageRole::Assistant);
            sqlx::query(
                r#"UPDATE tasks
                   SET last_activity_at = CASE
                         WHEN last_activity_at IS NULL OR last_activity_at < ? THEN ?
                         ELSE last_activity_at
                       END,
                       last_assistant_message_at = CASE
                         WHEN ? = 1 AND (last_assistant_message_at IS NULL OR last_assistant_message_at < ?)
                           THEN ?
                         ELSE last_assistant_message_at
                       END
                   WHERE id = ?"#,
            )
            .bind(&created_at)
            .bind(&created_at)
            .bind(if is_assistant { 1 } else { 0 })
            .bind(&created_at)
            .bind(&created_at)
            .bind(message.task_id.0.to_string())
            .execute(&mut *tx)
            .await?;
            let session_snapshot_write_bytes =
                if matches!(message.role, MessageRole::Assistant | MessageRole::User) {
                    let session_snapshot_id = message.session_id.0.to_string();
                    let session_snapshot_created_at = message.created_at.to_rfc3339();
                    let session_snapshot_content = derive_message_preview(&message.content);
                    let now = Utc::now().to_rfc3339();
                    let ensure_write_bytes =
                        bytes_str(&session_snapshot_id) + I64_BYTES + (bytes_str(&now) * 2);
                    sqlx::query(
                        r#"INSERT INTO session_snapshot_summaries (
                                session_id, running_turn_count, created_at, updated_at
                           )
                           VALUES (?, 0, ?, ?)
                           ON CONFLICT(session_id) DO NOTHING"#,
                    )
                    .bind(&session_snapshot_id)
                    .bind(&now)
                    .bind(&now)
                    .execute(&mut *tx)
                    .await?;
                    let update_write_bytes = bytes_str(&session_snapshot_created_at) * 2
                        + bytes_str(&session_snapshot_content);
                    sqlx::query(
                        r#"UPDATE session_snapshot_summaries
                           SET last_message_at = CASE
                                 WHEN last_message_at IS NULL OR last_message_at < ? THEN ?
                                 ELSE last_message_at
                               END,
                               last_message_preview = CASE
                                 WHEN last_message_at IS NULL OR last_message_at < ? THEN ?
                                 ELSE last_message_preview
                               END,
                               projection_rev = projection_rev + 1,
                               updated_at = ?
                           WHERE session_id = ?"#,
                    )
                    .bind(&session_snapshot_created_at)
                    .bind(&session_snapshot_created_at)
                    .bind(&session_snapshot_created_at)
                    .bind(&session_snapshot_content)
                    .bind(&session_snapshot_created_at)
                    .bind(&session_snapshot_id)
                    .execute(&mut *tx)
                    .await?;
                    ensure_write_bytes + update_write_bytes
                } else {
                    0
                };
            if let Err(err) =
                crate::fault_injection::maybe_fail("ctx_store.insert_message.after_insert")
            {
                return Err(anyhow::anyhow!(
                    "database is locked (fault injection): {err}"
                ));
            }
            tx.commit().await?;
            (message_rows_affected, session_snapshot_write_bytes)
        };

        record_write(
            WriteMetricTable::Messages,
            message_rows_affected,
            write_bytes,
        );
        if session_snapshot_write_bytes > 0 {
            record_write(
                WriteMetricTable::SessionSnapshotSummaries,
                1,
                session_snapshot_write_bytes,
            );
        }
        self.schedule_active_snapshot_head_refresh(message.session_id, None)
            .await?;
        Ok(message)
    }

    pub(super) async fn ensure_session_snapshot_summary(
        &self,
        session_id: SessionId,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let session_id = session_id.0.to_string();
        let write_bytes = bytes_str(&session_id) + I64_BYTES + (bytes_str(&now) * 2);
        let result = self
            .query(
                r#"INSERT INTO session_snapshot_summaries (
                    session_id, running_turn_count, created_at, updated_at
               )
               VALUES (?, 0, ?, ?)
               ON CONFLICT(session_id) DO NOTHING"#,
            )
            .bind(&session_id)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
        record_write(
            WriteMetricTable::SessionSnapshotSummaries,
            result.rows_affected(),
            write_bytes,
        );
        Ok(())
    }

    pub(super) async fn ensure_session_snapshot_summary_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        session_id: SessionId,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"INSERT INTO session_snapshot_summaries (
                    session_id, running_turn_count, created_at, updated_at
               )
               VALUES (?, 0, ?, ?)
               ON CONFLICT(session_id) DO NOTHING"#,
        )
        .bind(session_id.0.to_string())
        .bind(&now)
        .bind(&now)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub(super) async fn update_session_snapshot_last_event_seq_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        session_id: SessionId,
        seq: i64,
    ) -> Result<()> {
        self.ensure_session_snapshot_summary_tx(tx, session_id)
            .await?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"UPDATE session_snapshot_summaries
               SET last_event_seq = ?,
                   projection_rev = projection_rev + 1,
                   updated_at = ?
               WHERE session_id = ?"#,
        )
        .bind(seq)
        .bind(&now)
        .bind(session_id.0.to_string())
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub(super) async fn refresh_session_turn_summary(&self, session_id: SessionId) -> Result<()> {
        self.ensure_session_snapshot_summary(session_id).await?;
        let projection = self.summarize_session_turn_projection(session_id).await?;
        let last_status = projection
            .last_status
            .as_ref()
            .map(session_turn_status_to_str)
            .map(str::to_string);
        let last_seq = projection.last_seq;
        let running_count = projection.running_turn_count;
        let now = Utc::now().to_rfc3339();
        let session_id = session_id.0.to_string();
        let write_bytes = bytes_opt_str(last_status.as_deref())
            + bytes_opt_i64(last_seq)
            + I64_BYTES
            + bytes_str(&now);
        let result = self
            .query(
                r#"UPDATE session_snapshot_summaries
               SET last_turn_status = ?,
                   last_turn_seq = ?,
                   running_turn_count = ?,
                   projection_rev = projection_rev + 1,
                   updated_at = ?
               WHERE session_id = ?"#,
            )
            .bind(last_status)
            .bind(last_seq)
            .bind(running_count)
            .bind(&now)
            .bind(&session_id)
            .execute(&self.pool)
            .await?;
        record_write(
            WriteMetricTable::SessionSnapshotSummaries,
            result.rows_affected(),
            write_bytes,
        );
        Ok(())
    }

    pub(super) async fn refresh_session_turn_summary_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        session_id: SessionId,
    ) -> Result<()> {
        self.ensure_session_snapshot_summary_tx(tx, session_id)
            .await?;
        let projection = self
            .summarize_session_turn_projection_tx(tx, session_id)
            .await?;
        let last_status = projection
            .last_status
            .as_ref()
            .map(session_turn_status_to_str)
            .map(str::to_string);
        let last_seq = projection.last_seq;
        let running_count = projection.running_turn_count;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"UPDATE session_snapshot_summaries
               SET last_turn_status = ?,
                   last_turn_seq = ?,
                   running_turn_count = ?,
                   projection_rev = projection_rev + 1,
                   updated_at = ?
               WHERE session_id = ?"#,
        )
        .bind(last_status)
        .bind(last_seq)
        .bind(running_count)
        .bind(&now)
        .bind(session_id.0.to_string())
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

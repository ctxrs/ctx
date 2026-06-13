impl Store {
    async fn is_task_primary_session(
        &self,
        task_id: TaskId,
        session_id: SessionId,
    ) -> Result<bool> {
        let primary_session_id: Option<String> = self
            .query_scalar(r#"SELECT primary_session_id FROM tasks WHERE id = ?"#)
            .bind(task_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?
            .flatten();
        Ok(primary_session_id
            .as_deref()
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .map(SessionId)
            == Some(session_id))
    }

    pub(super) async fn load_active_snapshot_head_projection(
        &self,
        session_id: SessionId,
    ) -> Result<Option<ActiveSnapshotHeadProjection>> {
        let row = self
            .query(
                r#"SELECT head_rev, last_event_seq, turns_json, tool_summaries_json,
                          messages_json, has_more_turns, head_window_json, summary_checkpoint_json
                   FROM session_active_snapshot_heads
                   WHERE session_id = ?"#,
            )
            .bind(session_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };

        let turns_json: String = row.try_get("turns_json")?;
        let tool_summaries_json: String = row.try_get("tool_summaries_json")?;
        let messages_json: String = row.try_get("messages_json")?;
        let head_window_json: String = row.try_get("head_window_json")?;
        let summary_checkpoint_json: Option<String> = row.try_get("summary_checkpoint_json")?;

        let parsed = (|| -> Result<ActiveSnapshotHeadProjection> {
            let turns: Vec<SessionTurn> = serde_json::from_str(&turns_json)
                .context("deserializing active snapshot head turns")?;
            let tool_summaries: Vec<SessionTurnToolSummary> =
                serde_json::from_str(&tool_summaries_json)
                    .context("deserializing active snapshot head tool summaries")?;
            let messages: Vec<Message> = serde_json::from_str(&messages_json)
                .context("deserializing active snapshot head messages")?;
            let head_window: SessionHeadWindow = serde_json::from_str(&head_window_json)
                .context("deserializing active snapshot head window")?;
            let summary_checkpoint: Option<SessionSummaryCheckpoint> = summary_checkpoint_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .context("deserializing session summary checkpoint")?;

            Ok(ActiveSnapshotHeadProjection {
                head_rev: row.try_get("head_rev")?,
                last_event_seq: row.try_get("last_event_seq")?,
                turns,
                tool_summaries,
                messages,
                has_more_turns: row.try_get::<i64, _>("has_more_turns")? != 0,
                head_window,
                summary_checkpoint,
            })
        })();

        match parsed {
            Ok(projection) => Ok(Some(projection)),
            Err(err) => {
                tracing::warn!(
                    session_id = %session_id.0,
                    "discarding malformed active snapshot head projection: {err:#}"
                );
                Ok(None)
            }
        }
    }

    pub(super) async fn load_session_head_materialization(
        &self,
        session_id: SessionId,
        kind: SessionHeadKind,
    ) -> Result<Option<SessionHeadMaterialization>> {
        let timing_enabled = snapshot_timing_enabled();
        let acquire_start = timing_enabled.then(Instant::now);
        let mut conn = self.pool.acquire().await?;
        let acquire_ms = acquire_start
            .map(|start| start.elapsed())
            .unwrap_or_default();
        let query_start = timing_enabled.then(Instant::now);
        let row = self
            .query(
                r#"SELECT head_rev, last_event_seq, turns_json, tool_summaries_json, events_json,
                      messages_json, has_more_turns, head_window_json
               FROM session_head_materializations
               WHERE session_id = ? AND head_kind = ?"#,
            )
            .bind(session_id.0.to_string())
            .bind(session_head_kind_to_str(kind))
            .fetch_optional(&mut *conn)
            .await?;
        let query_ms = query_start.map(|start| start.elapsed()).unwrap_or_default();

        let Some(row) = row else {
            if timing_enabled {
                info!(
                    target: "ctx_store.snapshot_timing",
                    snapshot = "session_snapshot",
                    session_id = %session_id.0,
                    head_kind = session_head_kind_to_str(kind),
                    hit = false,
                    acquire_ms = acquire_ms.as_millis(),
                    query_ms = query_ms.as_millis(),
                    parse_ms = 0,
                );
            }
            return Ok(None);
        };

        let turns_json: String = row.try_get("turns_json")?;
        let tool_summaries_json: String = row.try_get("tool_summaries_json")?;
        let events_json: String = row.try_get("events_json")?;
        let messages_json: String = row.try_get("messages_json")?;
        let head_window_json: String = row.try_get("head_window_json")?;

        let parse_start = timing_enabled.then(Instant::now);
        let turns: Vec<SessionTurn> = match serde_json::from_str(&turns_json) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let tool_summaries: Vec<SessionTurnToolSummary> =
            match serde_json::from_str(&tool_summaries_json) {
                Ok(value) => value,
                Err(_) => return Ok(None),
            };
        let events: Vec<SessionEvent> = match serde_json::from_str(&events_json) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let messages: Vec<Message> = match serde_json::from_str(&messages_json) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let head_window: SessionHeadWindow = match serde_json::from_str(&head_window_json) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let parse_ms = parse_start.map(|start| start.elapsed()).unwrap_or_default();

        let has_more_turns: i64 = row.try_get("has_more_turns")?;

        if timing_enabled {
            info!(
                target: "ctx_store.snapshot_timing",
                snapshot = "session_snapshot",
                session_id = %session_id.0,
                head_kind = session_head_kind_to_str(kind),
                hit = true,
                turns = turns.len(),
                tool_summaries = tool_summaries.len(),
                events = events.len(),
                messages = messages.len(),
                acquire_ms = acquire_ms.as_millis(),
                query_ms = query_ms.as_millis(),
                parse_ms = parse_ms.as_millis(),
            );
        }

        Ok(Some(SessionHeadMaterialization {
            head_rev: row.try_get("head_rev")?,
            last_event_seq: row.try_get("last_event_seq")?,
            turns,
            tool_summaries,
            events,
            messages,
            has_more_turns: has_more_turns != 0,
            head_window,
        }))
    }

    pub(super) async fn upsert_active_snapshot_head_projection(
        &self,
        session_id: SessionId,
        head: &ActiveSnapshotHeadProjection,
    ) -> Result<()> {
        let turns_json =
            serde_json::to_string(&head.turns).context("serializing active head turns")?;
        let tool_summaries_json = serde_json::to_string(&head.tool_summaries)
            .context("serializing active head tool summaries")?;
        let messages_json =
            serde_json::to_string(&head.messages).context("serializing active head messages")?;
        let head_window_json =
            serde_json::to_string(&head.head_window).context("serializing active head window")?;
        let summary_checkpoint_json = head
            .summary_checkpoint
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serializing session summary checkpoint")?;
        let now = Utc::now().to_rfc3339();
        let session_id = session_id.0.to_string();
        let write_bytes = bytes_str(&session_id)
            + I64_BYTES
            + I64_BYTES
            + bytes_str(&turns_json)
            + bytes_str(&tool_summaries_json)
            + bytes_str(&messages_json)
            + BOOL_BYTES
            + bytes_str(&head_window_json)
            + bytes_opt_str(summary_checkpoint_json.as_deref())
            + bytes_str(&now)
            + bytes_str(&now);

        let _write_guard = self.write_gate.lock().await;
        let result = self
            .query(
                r#"INSERT INTO session_active_snapshot_heads (
                    session_id, head_rev, last_event_seq, turns_json, tool_summaries_json,
                    messages_json, has_more_turns, head_window_json, summary_checkpoint_json,
                    created_at, updated_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(session_id) DO UPDATE SET
                   head_rev = excluded.head_rev,
                   last_event_seq = excluded.last_event_seq,
                   turns_json = excluded.turns_json,
                   tool_summaries_json = excluded.tool_summaries_json,
                   messages_json = excluded.messages_json,
                   has_more_turns = excluded.has_more_turns,
                   head_window_json = excluded.head_window_json,
                   summary_checkpoint_json = excluded.summary_checkpoint_json,
                   updated_at = excluded.updated_at"#,
            )
            .bind(&session_id)
            .bind(head.head_rev)
            .bind(head.last_event_seq)
            .bind(turns_json)
            .bind(tool_summaries_json)
            .bind(messages_json)
            .bind(if head.has_more_turns { 1 } else { 0 })
            .bind(head_window_json)
            .bind(summary_checkpoint_json)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
        record_write(
            WriteMetricTable::SessionActiveSnapshotHeads,
            result.rows_affected(),
            write_bytes,
        );
        Ok(())
    }

    pub(super) async fn update_active_snapshot_head_last_event_seq(
        &self,
        session_id: SessionId,
        last_event_seq: i64,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let session_id_str = session_id.0.to_string();
        let write_bytes = I64_BYTES + bytes_str(&now);
        let rows_affected = {
            let _write_guard = self.write_gate.lock().await;
            self.query(
                r#"UPDATE session_active_snapshot_heads
               SET last_event_seq = ?,
                   updated_at = ?
               WHERE session_id = ?"#,
            )
            .bind(last_event_seq)
            .bind(&now)
            .bind(&session_id_str)
            .execute(&self.pool)
            .await?
            .rows_affected()
        };
        record_write(
            WriteMetricTable::SessionActiveSnapshotHeads,
            rows_affected,
            write_bytes,
        );
        if rows_affected == 0 {
            self.refresh_active_snapshot_head(session_id, Some(last_event_seq))
                .await?;
        }
        Ok(())
    }

    pub(super) async fn refresh_active_snapshot_head(
        &self,
        session_id: SessionId,
        last_event_seq: Option<i64>,
    ) -> Result<()> {
        let Some(session) = self.get_session(session_id).await? else {
            return Ok(());
        };
        if !matches!(
            self.session_head_kind_for_task(session.task_id).await?,
            SessionHeadKind::Active
        ) {
            let _write_guard = self.write_gate.lock().await;
            self.query(r#"DELETE FROM session_active_snapshot_heads WHERE session_id = ?"#)
                .bind(session_id.0.to_string())
                .execute(&self.pool)
                .await?;
            return Ok(());
        }
        if !self
            .is_task_primary_session(session.task_id, session.id)
            .await?
        {
            let _write_guard = self.write_gate.lock().await;
            self.query(r#"DELETE FROM session_active_snapshot_heads WHERE session_id = ?"#)
                .bind(session_id.0.to_string())
                .execute(&self.pool)
                .await?;
            return Ok(());
        }

        let last_event_seq = match last_event_seq {
            Some(seq) => seq,
            None => self.session_last_event_seq(session_id).await?,
        };
        let projection_rev = self.get_session_projection_rev(session_id).await?;
        let limits = session_head_limits(SessionHeadKind::Active, ACTIVE_SNAPSHOT_HEAD_LIMIT);
        let head = self
            .build_session_head(&session, limits, false, last_event_seq, projection_rev)
            .await?;
        let projection = ActiveSnapshotHeadProjection::from_head(&head);
        self.upsert_active_snapshot_head_projection(session_id, &projection)
            .await?;
        Ok(())
    }

    pub(super) async fn schedule_active_snapshot_head_refresh(
        &self,
        session_id: SessionId,
        last_event_seq: Option<i64>,
    ) -> Result<()> {
        self.active_head_projection
            .enqueue(session_id, last_event_seq)
            .await
    }

    pub async fn flush_active_snapshot_head_projection_queue(&self) -> Result<()> {
        self.active_head_projection.flush().await
    }

    pub(super) async fn delete_active_snapshot_heads_for_task(
        &self,
        task_id: TaskId,
    ) -> Result<()> {
        let _write_guard = self.write_gate.lock().await;
        self.query(
            r#"DELETE FROM session_active_snapshot_heads
               WHERE session_id IN (SELECT id FROM sessions WHERE task_id = ?)"#,
        )
        .bind(task_id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(super) async fn refresh_active_snapshot_heads_for_task(
        &self,
        task_id: TaskId,
    ) -> Result<()> {
        let sessions = self.list_active_sessions_for_task(task_id).await?;
        for session in sessions {
            self.refresh_active_snapshot_head(session.id, None).await?;
        }
        Ok(())
    }
}

impl Store {
    pub async fn insert_session_turn(&self, turn: SessionTurn) -> Result<SessionTurn> {
        let metrics_json = turn
            .metrics_json
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serializing turn metrics")?;
        let failure_json = turn
            .failure
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serializing turn failure")?;
        let turn_id = turn.turn_id.0.to_string();
        let session_id = turn.session_id.0.to_string();
        let run_id = turn.run_id.map(|r| r.0.to_string());
        let user_message_id = turn.user_message_id.map(|m| m.0.to_string());
        let status = session_turn_status_to_str(&turn.status);
        let started_at = turn.started_at.to_rfc3339();
        let updated_at = turn.updated_at.to_rfc3339();
        let write_bytes = bytes_str(&turn_id)
            + bytes_str(&session_id)
            + bytes_opt_str(run_id.as_deref())
            + bytes_opt_str(user_message_id.as_deref())
            + bytes_str(status)
            + bytes_opt_i64(turn.start_seq)
            + bytes_opt_i64(turn.end_seq)
            + bytes_str(&started_at)
            + bytes_str(&updated_at)
            + bytes_opt_str(turn.assistant_partial.as_deref())
            + bytes_opt_str(turn.thought_partial.as_deref())
            + bytes_opt_str(metrics_json.as_deref())
            + bytes_opt_str(failure_json.as_deref())
            + (I64_BYTES * 5);
        let result = self
            .query(
                r#"INSERT INTO session_turns (
                    turn_id,
                    session_id,
                    run_id,
                    user_message_id,
                    status,
                    start_seq,
                    end_seq,
                    started_at,
                    updated_at,
                    assistant_partial,
                    thought_partial,
                    metrics_json,
                    failure_json,
                    tool_total,
                    tool_pending,
                    tool_running,
                    tool_completed,
                    tool_failed
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .bind(&turn_id)
            .bind(&session_id)
            .bind(run_id)
            .bind(user_message_id)
            .bind(status)
            .bind(turn.start_seq)
            .bind(turn.end_seq)
            .bind(&started_at)
            .bind(&updated_at)
            .bind(turn.assistant_partial.as_deref())
            .bind(turn.thought_partial.as_deref())
            .bind(metrics_json)
            .bind(failure_json)
            .bind(turn.tool_total)
            .bind(turn.tool_pending)
            .bind(turn.tool_running)
            .bind(turn.tool_completed)
            .bind(turn.tool_failed)
            .execute(&self.pool)
            .await?;
        record_write(
            WriteMetricTable::SessionTurns,
            result.rows_affected(),
            write_bytes,
        );
        self.refresh_session_turn_summary(turn.session_id).await?;
        self.schedule_active_snapshot_head_refresh(turn.session_id, None)
            .await?;
        Ok(turn)
    }

    pub async fn get_session_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Option<SessionTurn>> {
        let row = self.query(
                r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                      start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                      metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
               FROM session_turns
               WHERE session_id = ? AND turn_id = ?"#,
        )
        .bind(session_id.0.to_string())
        .bind(turn_id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| build_session_turn_from_row(r).ok()))
    }

    pub async fn get_session_turn_by_id(&self, turn_id: TurnId) -> Result<Option<SessionTurn>> {
        let row = self
            .query(
                r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                      start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                      metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
               FROM session_turns
               WHERE turn_id = ?"#,
            )
            .bind(turn_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.and_then(|r| build_session_turn_from_row(r).ok()))
    }

    pub async fn get_running_turn_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionTurn>> {
        let row = self
            .query(
                r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                          start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                          metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
                   FROM session_turns
                   WHERE session_id = ? AND status IN ('queued', 'running')
                   ORDER BY start_seq DESC
                   LIMIT 1"#,
            )
            .bind(session_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.and_then(|r| build_session_turn_from_row(r).ok()))
    }

    pub async fn get_latest_turn_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionTurn>> {
        let row = self
            .query(
                r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                          start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                          metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
                   FROM session_turns
                   WHERE session_id = ?
                   ORDER BY start_seq DESC
                   LIMIT 1"#,
            )
            .bind(session_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.and_then(|r| build_session_turn_from_row(r).ok()))
    }

    pub async fn get_latest_turn_for_run(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Option<SessionTurn>> {
        let row = self
            .query(
                r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                          start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                          metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
                   FROM session_turns
                   WHERE session_id = ? AND run_id = ?
                   ORDER BY start_seq DESC
                   LIMIT 1"#,
            )
            .bind(session_id.0.to_string())
            .bind(run_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.and_then(|r| build_session_turn_from_row(r).ok()))
    }

    pub async fn delete_session_turn(&self, session_id: SessionId, turn_id: TurnId) -> Result<()> {
        self.query(r#"DELETE FROM session_turns WHERE session_id = ? AND turn_id = ?"#)
            .bind(session_id.0.to_string())
            .bind(turn_id.0.to_string())
            .execute(&self.pool)
            .await?;
        self.refresh_session_turn_summary(session_id).await?;
        self.schedule_active_snapshot_head_refresh(session_id, None)
            .await?;
        Ok(())
    }

    pub async fn update_session_turn_partial(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        assistant_partial: Option<&str>,
        thought_partial: Option<&str>,
        updated_at: DateTime<Utc>,
    ) -> Result<()> {
        if assistant_partial.is_none() && thought_partial.is_none() {
            return Ok(());
        }
        let updated_at = updated_at.to_rfc3339();
        let write_bytes = bytes_opt_str(assistant_partial)
            + bytes_opt_str(thought_partial)
            + bytes_str(&updated_at);
        let result = self
            .query(
                r#"UPDATE session_turns
               SET assistant_partial = COALESCE(?, assistant_partial),
                   thought_partial = COALESCE(?, thought_partial),
                   updated_at = ?
               WHERE session_id = ? AND turn_id = ?"#,
            )
            .bind(assistant_partial.map(|s| s.to_string()))
            .bind(thought_partial.map(|s| s.to_string()))
            .bind(&updated_at)
            .bind(session_id.0.to_string())
            .bind(turn_id.0.to_string())
            .execute(&self.pool)
            .await?;
        record_write(
            WriteMetricTable::SessionTurns,
            result.rows_affected(),
            write_bytes,
        );
        Ok(())
    }

    pub async fn update_session_turn_status(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        status: SessionTurnStatus,
        end_seq: Option<i64>,
        metrics_json: Option<&serde_json::Value>,
        updated_at: DateTime<Utc>,
    ) -> Result<()> {
        let metrics_json = metrics_json
            .map(serde_json::to_string)
            .transpose()
            .context("serializing turn metrics")?;
        let status = session_turn_status_to_str(&status);
        let updated_at = updated_at.to_rfc3339();
        let write_bytes = bytes_str(status)
            + bytes_opt_i64(end_seq)
            + bytes_opt_str(metrics_json.as_deref())
            + bytes_str(&updated_at);
        let result = self
            .query(
                r#"UPDATE session_turns
               SET status = ?,
                   end_seq = COALESCE(?, end_seq),
                   metrics_json = COALESCE(?, metrics_json),
                   updated_at = ?
               WHERE session_id = ? AND turn_id = ?"#,
            )
            .bind(status)
            .bind(end_seq)
            .bind(metrics_json)
            .bind(&updated_at)
            .bind(session_id.0.to_string())
            .bind(turn_id.0.to_string())
            .execute(&self.pool)
            .await?;
        record_write(
            WriteMetricTable::SessionTurns,
            result.rows_affected(),
            write_bytes,
        );
        self.refresh_session_turn_summary(session_id).await?;
        self.schedule_active_snapshot_head_refresh(session_id, None)
            .await?;
        Ok(())
    }

    pub async fn update_session_turn_tool_counts(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        deltas: SessionTurnToolCountDeltas,
        updated_at: DateTime<Utc>,
    ) -> Result<()> {
        let updated_at = updated_at.to_rfc3339();
        let write_bytes = (I64_BYTES * 5) + bytes_str(&updated_at);
        let result = self
            .query(
                r#"UPDATE session_turns
               SET tool_total = tool_total + ?,
                   tool_pending = tool_pending + ?,
                   tool_running = tool_running + ?,
                   tool_completed = tool_completed + ?,
                   tool_failed = tool_failed + ?,
                   updated_at = ?
               WHERE session_id = ? AND turn_id = ?"#,
            )
            .bind(deltas.total)
            .bind(deltas.pending)
            .bind(deltas.running)
            .bind(deltas.completed)
            .bind(deltas.failed)
            .bind(&updated_at)
            .bind(session_id.0.to_string())
            .bind(turn_id.0.to_string())
            .execute(&self.pool)
            .await?;
        record_write(
            WriteMetricTable::SessionTurns,
            result.rows_affected(),
            write_bytes,
        );
        self.schedule_active_snapshot_head_refresh(session_id, None)
            .await?;
        Ok(())
    }
}

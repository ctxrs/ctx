impl Store {
    pub async fn list_session_turns_page_by_seq(
        &self,
        session_id: SessionId,
        before_seq: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<SessionTurn>> {
        let limit = limit.unwrap_or(50).clamp(1, 500) as i64;
        let rows = if let Some(before_seq) = before_seq {
            self.query(
                r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                          start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                          metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
                   FROM session_turns
                   WHERE session_id = ? AND start_seq < ?
                   ORDER BY start_seq DESC
                   LIMIT ?"#,
            )
            .bind(session_id.0.to_string())
            .bind(before_seq)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            self.query(
                r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                          start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                          metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
                   FROM session_turns
                   WHERE session_id = ?
                   ORDER BY start_seq DESC
                   LIMIT ?"#,
            )
            .bind(session_id.0.to_string())
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            if let Ok(turn) = build_session_turn_from_row(r) {
                out.push(turn);
            }
        }
        out.reverse();
        Ok(out)
    }

    pub async fn list_session_turns_by_statuses(
        &self,
        statuses: &[SessionTurnStatus],
    ) -> Result<Vec<SessionTurn>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = String::from(
            r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                      start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                      metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
               FROM session_turns
               WHERE status IN ("#,
        );
        for i in 0..statuses.len() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
        }
        sql.push_str(") ORDER BY updated_at ASC");
        let sql = self.rewrite_sql(&sql);
        let mut query = sqlx::query(sql.as_ref());
        for status in statuses {
            query = query.bind(session_turn_status_to_str(status));
        }
        let rows = query.fetch_all(&self.pool).await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            if let Ok(turn) = build_session_turn_from_row(r) {
                out.push(turn);
            }
        }
        Ok(out)
    }

    pub async fn get_session_snapshot(
        &self,
        session_id: SessionId,
        _limit: u32,
        _include_events: bool,
    ) -> Result<Option<SessionSnapshot>> {
        let summary = match self.get_session_snapshot_summary(session_id).await? {
            Some(summary) => summary,
            None => return Ok(None),
        };
        let state = self.get_session_state(session_id).await?;
        Ok(Some(SessionSnapshot {
            summary,
            head: None,
            state: Some(state),
        }))
    }

    pub async fn get_session_history_page(
        &self,
        session_id: SessionId,
        before_seq: Option<i64>,
        limit: u32,
    ) -> Result<Option<SessionHistoryPage>> {
        if self.get_session(session_id).await?.is_none() {
            return Ok(None);
        }
        let limit = limit.clamp(1, 200) as i64;
        let rows = if let Some(before_seq) = before_seq {
            self.query(
                r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                          start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                          metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
                   FROM session_turns
                   WHERE session_id = ? AND start_seq < ?
                   ORDER BY start_seq DESC
                   LIMIT ?"#,
            )
            .bind(session_id.0.to_string())
            .bind(before_seq)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await?
        } else {
            self.query(
                r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                          start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                          metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
                   FROM session_turns
                   WHERE session_id = ?
                   ORDER BY start_seq DESC
                   LIMIT ?"#,
            )
            .bind(session_id.0.to_string())
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await?
        };

        let mut has_more = false;
        let mut turns = Vec::with_capacity(rows.len());
        for r in rows {
            if turns.len() as i64 >= limit {
                has_more = true;
                break;
            }
            if let Ok(turn) = build_session_turn_from_row(r) {
                turns.push(turn);
            }
        }
        turns.reverse();

        let next_cursor = if has_more {
            turns.first().and_then(|t| t.start_seq)
        } else {
            None
        };

        let turn_ids: Vec<TurnId> = turns.iter().map(|t| t.turn_id).collect();
        let messages = self.list_messages_for_turns(session_id, &turn_ids).await?;

        Ok(Some(SessionHistoryPage {
            session_id,
            turns,
            messages,
            next_cursor,
            has_more,
        }))
    }
}

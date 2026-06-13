use super::*;

use ctx_core::session_projection::{resolve_turn_terminal_state, TurnTerminalState};

#[derive(Debug, Clone)]
pub(super) struct SessionTurnSummaryProjection {
    pub(super) last_status: Option<SessionTurnStatus>,
    pub(super) last_seq: Option<i64>,
    pub(super) running_turn_count: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TurnToolCounts {
    total: i64,
    pending: i64,
    running: i64,
    completed: i64,
    failed: i64,
}

#[derive(Debug, Clone, PartialEq)]
struct RepairedTurnProjection {
    status: SessionTurnStatus,
    end_seq: Option<i64>,
    metrics_json: Option<Value>,
    failure: Option<SessionTurnFailure>,
    updated_at: DateTime<Utc>,
    tool_counts: TurnToolCounts,
}

fn is_non_terminal_status(status: &SessionTurnStatus) -> bool {
    matches!(
        status,
        SessionTurnStatus::Queued | SessionTurnStatus::Starting | SessionTurnStatus::Running
    )
}

fn summarize_tool_counts<'a>(
    tools: impl IntoIterator<Item = &'a SessionTurnTool>,
) -> TurnToolCounts {
    let mut counts = TurnToolCounts::default();
    for tool in tools {
        counts.total += 1;
        match tool.status.as_deref() {
            Some("running") | Some("in_progress") => counts.running += 1,
            Some("completed") | Some("complete") | Some("ok") | Some("succeeded") => {
                counts.completed += 1;
            }
            Some("failed") | Some("error") => counts.failed += 1,
            _ => counts.pending += 1,
        }
    }
    counts
}

fn is_tool_before_terminal(tool: &SessionTurnTool, terminal_seq: Option<i64>) -> bool {
    terminal_seq.is_none_or(|seq| tool.first_event_seq.is_none_or(|tool_seq| tool_seq <= seq))
}

fn decode_session_event_row(row: SqliteRow) -> Result<SessionEvent> {
    let id: String = row.try_get("id")?;
    let session_id: String = row.try_get("session_id")?;
    let created_at: String = row.try_get("created_at")?;
    let run_id: Option<String> = row.try_get("run_id")?;
    let turn_id: Option<String> = row.try_get("turn_id")?;
    let payload_json: String = row.try_get("payload_json")?;
    let transient: i64 = row.try_get("transient")?;
    Ok(SessionEvent {
        seq: row.try_get("seq")?,
        id: SessionEventId(uuid::Uuid::parse_str(&id)?),
        session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
        run_id: run_id
            .as_deref()
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .map(RunId),
        turn_id: turn_id
            .as_deref()
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .map(TurnId),
        event_type: parse_session_event_type(row.try_get::<String, _>("event_type")?.as_str()),
        payload_json: serde_json::from_str(&payload_json)
            .context("parsing session event payload")?,
        transient: transient != 0,
        created_at: parse_dt(&created_at)?,
    })
}

fn build_repaired_turn_projection(
    turn: &SessionTurn,
    terminal: &TurnTerminalState,
    tools: &[SessionTurnTool],
) -> RepairedTurnProjection {
    let terminal_seq = terminal.end_seq.or(turn.end_seq);
    let relevant_tools = || {
        tools
            .iter()
            .filter(move |tool| is_tool_before_terminal(tool, terminal_seq))
    };
    let tool_counts = summarize_tool_counts(relevant_tools());
    let updated_at = std::iter::once(terminal.updated_at)
        .chain(relevant_tools().map(|tool| tool.updated_at))
        .max()
        .unwrap_or(turn.updated_at);
    RepairedTurnProjection {
        status: terminal.status.clone(),
        end_seq: terminal.end_seq.or(turn.end_seq),
        metrics_json: terminal.metrics.clone().or(turn.metrics_json.clone()),
        failure: terminal.failure.clone(),
        updated_at,
        tool_counts,
    }
}

fn turn_projection_changed(turn: &SessionTurn, repaired: &RepairedTurnProjection) -> bool {
    turn.status != repaired.status
        || turn.end_seq != repaired.end_seq
        || turn.metrics_json != repaired.metrics_json
        || turn.failure != repaired.failure
        || turn.tool_total != repaired.tool_counts.total
        || turn.tool_pending != repaired.tool_counts.pending
        || turn.tool_running != repaired.tool_counts.running
        || turn.tool_completed != repaired.tool_counts.completed
        || turn.tool_failed != repaired.tool_counts.failed
}

async fn list_session_events_for_turn_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<Vec<SessionEvent>> {
    let rows = sqlx::query(
        r#"SELECT seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
           FROM session_events
           WHERE session_id = ? AND turn_id = ?
           ORDER BY seq ASC"#,
    )
    .bind(session_id.0.to_string())
    .bind(turn_id.0.to_string())
    .fetch_all(&mut **tx)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(decode_session_event_row(row)?);
    }
    Ok(out)
}

async fn list_turn_tools_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<Vec<SessionTurnTool>> {
    let rows = sqlx::query(
        r#"SELECT session_id, tool_call_id, turn_id, tool_kind, provider_tool_name, title, subtitle, status, input_json,
                  output_text, order_seq, first_event_seq, input_truncated, input_original_bytes,
                  output_truncated, output_original_bytes, created_at, updated_at
           FROM session_turn_tools
           WHERE session_id = ? AND turn_id = ?
           ORDER BY created_at ASC"#,
    )
    .bind(session_id.0.to_string())
    .bind(turn_id.0.to_string())
    .fetch_all(&mut **tx)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if let Ok(tool) = build_session_turn_tool_from_row(row) {
            out.push(tool);
        }
    }
    out.sort_by(compare_tool_order);
    Ok(out)
}

async fn list_session_turns_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    session_id: SessionId,
) -> Result<Vec<SessionTurn>> {
    let rows = sqlx::query(
        r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                  start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                  metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
           FROM session_turns
           WHERE session_id = ?
           ORDER BY COALESCE(start_seq, -1) DESC, started_at DESC, turn_id DESC"#,
    )
    .bind(session_id.0.to_string())
    .fetch_all(&mut **tx)
    .await?;

    let mut turns = Vec::with_capacity(rows.len());
    for row in rows {
        if let Ok(turn) = build_session_turn_from_row(row) {
            turns.push(turn);
        }
    }
    Ok(turns)
}

impl Store {
    pub(super) async fn summarize_session_turn_projection(
        &self,
        session_id: SessionId,
    ) -> Result<SessionTurnSummaryProjection> {
        let rows = self
            .query(
                r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                          start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                          metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
                   FROM session_turns
                   WHERE session_id = ?
                   ORDER BY COALESCE(start_seq, -1) DESC, started_at DESC, turn_id DESC"#,
            )
            .bind(session_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        let mut turns = Vec::with_capacity(rows.len());
        for row in rows {
            if let Ok(turn) = build_session_turn_from_row(row) {
                turns.push(turn);
            }
        }

        let mut last_status = None;
        let mut last_seq = None;
        let mut running_turn_count = 0_i64;

        for (idx, turn) in turns.iter().enumerate() {
            let effective_status = if is_non_terminal_status(&turn.status) {
                self.list_session_events_for_turn(session_id, turn.turn_id, false)
                    .await
                    .ok()
                    .and_then(|events| resolve_turn_terminal_state(&events))
                    .map(|terminal| terminal.status)
                    .unwrap_or_else(|| turn.status.clone())
            } else {
                turn.status.clone()
            };

            if idx == 0 {
                last_status = Some(effective_status.clone());
                last_seq = turn.start_seq;
            }
            if matches!(
                effective_status,
                SessionTurnStatus::Starting | SessionTurnStatus::Running
            ) {
                running_turn_count += 1;
            }
        }

        Ok(SessionTurnSummaryProjection {
            last_status,
            last_seq,
            running_turn_count,
        })
    }

    pub(super) async fn summarize_session_turn_projection_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        session_id: SessionId,
    ) -> Result<SessionTurnSummaryProjection> {
        let turns = list_session_turns_tx(tx, session_id).await?;
        let mut last_status = None;
        let mut last_seq = None;
        let mut running_turn_count = 0_i64;

        for (idx, turn) in turns.iter().enumerate() {
            let effective_status = if is_non_terminal_status(&turn.status) {
                list_session_events_for_turn_tx(tx, session_id, turn.turn_id)
                    .await
                    .ok()
                    .and_then(|events| resolve_turn_terminal_state(&events))
                    .map(|terminal| terminal.status)
                    .unwrap_or_else(|| turn.status.clone())
            } else {
                turn.status.clone()
            };

            if idx == 0 {
                last_status = Some(effective_status.clone());
                last_seq = turn.start_seq;
            }
            if matches!(
                effective_status,
                SessionTurnStatus::Starting | SessionTurnStatus::Running
            ) {
                running_turn_count += 1;
            }
        }

        Ok(SessionTurnSummaryProjection {
            last_status,
            last_seq,
            running_turn_count,
        })
    }

    pub async fn repair_session_turn_projection_from_events(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<()> {
        let Some(turn) = self.get_session_turn(session_id, turn_id).await? else {
            return Ok(());
        };

        let events = self
            .list_session_events_for_turn(session_id, turn_id, false)
            .await?;
        let terminal = resolve_turn_terminal_state(&events);
        let Some(terminal) = terminal.as_ref() else {
            return Ok(());
        };
        let tools = self.list_turn_tools(session_id, turn_id).await?;
        let repaired = build_repaired_turn_projection(&turn, terminal, &tools);
        let changed = turn_projection_changed(&turn, &repaired);

        if !changed {
            return Ok(());
        }

        let repaired_status_str = session_turn_status_to_str(&repaired.status);
        let repaired_metrics_json = repaired
            .metrics_json
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serializing repaired turn metrics")?;
        let repaired_failure_json = repaired
            .failure
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serializing repaired turn failure")?;
        let repaired_updated_at_str = repaired.updated_at.to_rfc3339();
        let write_bytes = bytes_str(repaired_status_str)
            + bytes_opt_i64(repaired.end_seq)
            + bytes_opt_str(repaired_metrics_json.as_deref())
            + bytes_opt_str(repaired_failure_json.as_deref())
            + (I64_BYTES * 5)
            + bytes_str(&repaired_updated_at_str);
        let result = self
            .query(
                r#"UPDATE session_turns
                   SET status = ?,
                       end_seq = ?,
                       metrics_json = ?,
                       failure_json = ?,
                       tool_total = ?,
                       tool_pending = ?,
                       tool_running = ?,
                       tool_completed = ?,
                       tool_failed = ?,
                       updated_at = ?
                   WHERE session_id = ? AND turn_id = ?"#,
            )
            .bind(repaired_status_str)
            .bind(repaired.end_seq)
            .bind(repaired_metrics_json)
            .bind(repaired_failure_json)
            .bind(repaired.tool_counts.total)
            .bind(repaired.tool_counts.pending)
            .bind(repaired.tool_counts.running)
            .bind(repaired.tool_counts.completed)
            .bind(repaired.tool_counts.failed)
            .bind(&repaired_updated_at_str)
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

    pub(super) async fn repair_session_turn_projection_from_events_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<bool> {
        let row = sqlx::query(
            r#"SELECT turn_id, session_id, run_id, user_message_id, status,
                      start_seq, end_seq, started_at, updated_at, assistant_partial, thought_partial,
                      metrics_json, failure_json, tool_total, tool_pending, tool_running, tool_completed, tool_failed
               FROM session_turns
               WHERE session_id = ? AND turn_id = ?"#,
        )
        .bind(session_id.0.to_string())
        .bind(turn_id.0.to_string())
        .fetch_optional(&mut **tx)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let Some(turn) = build_session_turn_from_row(row).ok() else {
            return Ok(false);
        };

        let events = list_session_events_for_turn_tx(tx, session_id, turn_id).await?;
        let Some(terminal) = resolve_turn_terminal_state(&events) else {
            return Ok(false);
        };
        let tools = list_turn_tools_tx(tx, session_id, turn_id).await?;
        let repaired = build_repaired_turn_projection(&turn, &terminal, &tools);
        if !turn_projection_changed(&turn, &repaired) {
            return Ok(false);
        }

        let repaired_metrics_json = repaired
            .metrics_json
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serializing repaired turn metrics")?;
        let repaired_failure_json = repaired
            .failure
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serializing repaired turn failure")?;
        sqlx::query(
            r#"UPDATE session_turns
               SET status = ?,
                   end_seq = ?,
                   metrics_json = ?,
                   failure_json = ?,
                   tool_total = ?,
                   tool_pending = ?,
                   tool_running = ?,
                   tool_completed = ?,
                   tool_failed = ?,
                   updated_at = ?
               WHERE session_id = ? AND turn_id = ?"#,
        )
        .bind(session_turn_status_to_str(&repaired.status))
        .bind(repaired.end_seq)
        .bind(repaired_metrics_json)
        .bind(repaired_failure_json)
        .bind(repaired.tool_counts.total)
        .bind(repaired.tool_counts.pending)
        .bind(repaired.tool_counts.running)
        .bind(repaired.tool_counts.completed)
        .bind(repaired.tool_counts.failed)
        .bind(repaired.updated_at.to_rfc3339())
        .bind(session_id.0.to_string())
        .bind(turn_id.0.to_string())
        .execute(&mut **tx)
        .await?;
        Ok(true)
    }
}

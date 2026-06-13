impl Store {
    pub async fn list_turn_tools(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<SessionTurnTool>> {
        let rows = self
            .query(
                r#"SELECT session_id, tool_call_id, turn_id, tool_kind, provider_tool_name, title, subtitle, status, input_json,
                      output_text, order_seq, first_event_seq, input_truncated, input_original_bytes,
                      output_truncated, output_original_bytes, created_at, updated_at
               FROM session_turn_tools
               WHERE session_id = ? AND turn_id = ?
               ORDER BY created_at ASC"#,
            )
            .bind(session_id.0.to_string())
            .bind(turn_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;
        if !rows.is_empty() {
            let mut out = Vec::with_capacity(rows.len());
            for r in rows {
                if let Ok(tool) = build_session_turn_tool_from_row(r) {
                    out.push(tool);
                }
            }
            out.sort_by(compare_tool_order);
            return Ok(out);
        }

        let events = self
            .list_session_events_for_turn(session_id, turn_id, false)
            .await?;
        let tools = build_turn_tools_from_events(session_id, turn_id, &events);
        for tool in &tools {
            let _ = self.upsert_session_turn_tool(tool.clone()).await;
        }
        Ok(tools)
    }

    pub async fn list_turn_tool_summaries_for_turns(
        &self,
        session_id: SessionId,
        turn_ids: &[TurnId],
    ) -> Result<Vec<SessionTurnToolSummary>> {
        if turn_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = String::from(
            r#"SELECT session_id, tool_call_id, turn_id, tool_kind, provider_tool_name, title, subtitle, status, input_json,
                      output_text, order_seq, first_event_seq, input_truncated, input_original_bytes,
                      output_truncated, output_original_bytes, created_at, updated_at
               FROM session_turn_tools
               WHERE session_id = ? AND turn_id IN ("#,
        );
        for i in 0..turn_ids.len() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
        }
        sql.push_str(") ORDER BY created_at ASC");
        let sql = self.rewrite_sql(&sql);
        let mut query = sqlx::query(sql.as_ref()).bind(session_id.0.to_string());
        for turn_id in turn_ids {
            query = query.bind(turn_id.0.to_string());
        }
        let rows = query.fetch_all(&self.pool).await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            if let Ok(tool) = build_session_turn_tool_summary_from_row(r) {
                out.push(tool);
            }
        }
        out.sort_by(compare_tool_summary_order);
        Ok(out)
    }

    pub async fn list_recent_turn_tool_summaries_for_turns(
        &self,
        session_id: SessionId,
        turn_ids: &[TurnId],
        limit: usize,
    ) -> Result<Vec<SessionTurnToolSummary>> {
        if turn_ids.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut sql = String::from(
            r#"SELECT tools.session_id, tools.tool_call_id, tools.turn_id, tools.tool_kind,
                      tools.provider_tool_name, tools.title, tools.subtitle, tools.status,
                      tools.input_json, tools.output_text, tools.order_seq, tools.first_event_seq,
                      tools.input_truncated, tools.input_original_bytes,
                      tools.output_truncated, tools.output_original_bytes,
                      tools.created_at, tools.updated_at
               FROM session_turn_tools AS tools
               INNER JOIN (
                   SELECT session_id, tool_call_id
                   FROM session_turn_tools
                   WHERE session_id = ? AND turn_id IN ("#,
        );
        for i in 0..turn_ids.len() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
        }
        sql.push_str(
            r#")
                   ORDER BY order_seq DESC, created_at DESC, tool_call_id DESC
                   LIMIT ?
               ) AS recent
                 ON recent.session_id = tools.session_id
                AND recent.tool_call_id = tools.tool_call_id
               ORDER BY tools.order_seq ASC, tools.created_at ASC, tools.tool_call_id ASC"#,
        );
        let sql = self.rewrite_sql(&sql);
        let mut query = sqlx::query(sql.as_ref()).bind(session_id.0.to_string());
        for turn_id in turn_ids {
            query = query.bind(turn_id.0.to_string());
        }
        query = query.bind(limit.saturating_add(1) as i64);
        let rows = query.fetch_all(&self.pool).await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            if let Ok(tool) = build_session_turn_tool_summary_from_row(r) {
                out.push(tool);
            }
        }
        out.sort_by(compare_tool_summary_order);
        Ok(out)
    }

    pub async fn get_session_turn_tool(
        &self,
        session_id: SessionId,
        tool_call_id: &str,
    ) -> Result<Option<SessionTurnTool>> {
        let row = self
            .query(
                r#"SELECT session_id, tool_call_id, turn_id, tool_kind, provider_tool_name, title, subtitle, status, input_json,
                      output_text, order_seq, first_event_seq, input_truncated, input_original_bytes,
                      output_truncated, output_original_bytes, created_at, updated_at
               FROM session_turn_tools
               WHERE session_id = ? AND tool_call_id = ?"#,
            )
            .bind(session_id.0.to_string())
            .bind(tool_call_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| build_session_turn_tool_from_row(r).ok()))
    }

    pub async fn upsert_session_turn_tool(&self, tool: SessionTurnTool) -> Result<SessionTurnTool> {
        if disable_tool_summary_persistence() {
            return Ok(tool);
        }
        let input_json = tool
            .input_json
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serializing tool input")?;
        let input_truncated = tool.input_truncated.map(|value| if value { 1 } else { 0 });
        let output_truncated = tool.output_truncated.map(|value| if value { 1 } else { 0 });
        let session_id = tool.session_id.0.to_string();
        let turn_id = tool.turn_id.0.to_string();
        let created_at = tool.created_at.to_rfc3339();
        let updated_at = tool.updated_at.to_rfc3339();
        let write_bytes = bytes_str(&session_id)
            + bytes_str(&tool.tool_call_id)
            + bytes_str(&turn_id)
            + bytes_opt_str(tool.tool_kind.as_deref())
            + bytes_opt_str(tool.provider_tool_name.as_deref())
            + bytes_opt_str(tool.title.as_deref())
            + bytes_opt_str(tool.subtitle.as_deref())
            + bytes_opt_str(tool.status.as_deref())
            + bytes_opt_str(input_json.as_deref())
            + bytes_opt_str(tool.output_text.as_deref())
            + I64_BYTES
            + bytes_opt_i64(tool.first_event_seq)
            + if input_truncated.is_some() {
                BOOL_BYTES
            } else {
                0
            }
            + bytes_opt_i64(tool.input_original_bytes)
            + if output_truncated.is_some() {
                BOOL_BYTES
            } else {
                0
            }
            + bytes_opt_i64(tool.output_original_bytes)
            + bytes_str(&created_at)
            + bytes_str(&updated_at);
        let result = self.query(
            r#"INSERT INTO session_turn_tools (
                    session_id, tool_call_id, turn_id, tool_kind, provider_tool_name, title, subtitle, status, input_json,
                    output_text, order_seq, first_event_seq, input_truncated, input_original_bytes, output_truncated,
                    output_original_bytes, created_at, updated_at
               ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(session_id, tool_call_id) DO UPDATE SET
                   turn_id = excluded.turn_id,
                   tool_kind = COALESCE(excluded.tool_kind, session_turn_tools.tool_kind),
                   provider_tool_name = COALESCE(excluded.provider_tool_name, session_turn_tools.provider_tool_name),
                   title = COALESCE(excluded.title, session_turn_tools.title),
                   subtitle = COALESCE(excluded.subtitle, session_turn_tools.subtitle),
                   status = CASE WHEN excluded.status IS NULL THEN session_turn_tools.status
                       WHEN session_turn_tools.status IN ('completed', 'failed')
                            AND excluded.status IN ('pending', 'in_progress') THEN session_turn_tools.status
                       ELSE excluded.status END,
                   input_json = COALESCE(excluded.input_json, session_turn_tools.input_json),
                   output_text = COALESCE(excluded.output_text, session_turn_tools.output_text),
                   order_seq = COALESCE(MIN(session_turn_tools.order_seq, excluded.order_seq), excluded.order_seq),
                   first_event_seq = COALESCE(session_turn_tools.first_event_seq, excluded.first_event_seq),
                   input_truncated = COALESCE(excluded.input_truncated, session_turn_tools.input_truncated), input_original_bytes = COALESCE(excluded.input_original_bytes, session_turn_tools.input_original_bytes),
                   output_truncated = COALESCE(excluded.output_truncated, session_turn_tools.output_truncated), output_original_bytes = COALESCE(excluded.output_original_bytes, session_turn_tools.output_original_bytes),
                   updated_at = excluded.updated_at"#,
        )
        .bind(&session_id)
        .bind(&tool.tool_call_id)
        .bind(&turn_id)
        .bind(tool.tool_kind.as_deref())
        .bind(tool.provider_tool_name.as_deref())
        .bind(tool.title.as_deref())
        .bind(tool.subtitle.as_deref())
        .bind(tool.status.as_deref())
        .bind(input_json)
        .bind(tool.output_text.as_deref())
        .bind(tool.order_seq)
        .bind(tool.first_event_seq)
        .bind(input_truncated)
        .bind(tool.input_original_bytes)
        .bind(output_truncated)
        .bind(tool.output_original_bytes)
        .bind(&created_at)
        .bind(&updated_at)
        .execute(&self.pool)
        .await?;
        record_write(
            WriteMetricTable::SessionTurnTools,
            result.rows_affected(),
            write_bytes,
        );
        Ok(tool)
    }
}

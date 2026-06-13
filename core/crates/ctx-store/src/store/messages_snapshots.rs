use super::*;

fn session_snapshot_preview_sql(column: &str) -> String {
    let trimmed_all = format!("trim({column}, char(9) || char(10) || char(13) || ' ')");
    let first_line = format!(
        "CASE WHEN instr({trimmed_all}, char(10)) > 0 THEN substr({trimmed_all}, 1, instr({trimmed_all}, char(10)) - 1) ELSE {trimmed_all} END",
    );
    let trimmed = format!("trim({first_line})");
    format!(
        "CASE \
            WHEN {column} IS NULL THEN NULL \
            WHEN length({trimmed}) = 0 THEN NULL \
            WHEN length({trimmed}) > {limit} THEN substr({trimmed}, 1, {limit}) || '...' \
            ELSE {trimmed} \
         END",
        column = column,
        trimmed = trimmed,
        limit = MESSAGE_PREVIEW_MAX_CHARS,
    )
}

impl Store {
    pub(super) async fn build_workspace_task_summaries(
        &self,
        rows: Vec<(Task, DateTime<Utc>)>,
    ) -> Result<Vec<WorkspaceTaskSummary>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        const SESSION_LIMIT: i64 = 4;
        let mut summaries = Vec::with_capacity(rows.len());
        let mut index_by_task = HashMap::new();
        for (idx, (task, sort_at)) in rows.into_iter().enumerate() {
            index_by_task.insert(task.id, idx);
            summaries.push(WorkspaceTaskSummary {
                task,
                provider_ids: Vec::new(),
                sessions: Vec::new(),
                sort_at,
            });
        }

        let task_ids: Vec<TaskId> = summaries.iter().map(|s| s.task.id).collect();

        if !task_ids.is_empty() {
            let mut session_sql = String::from(
                "
                SELECT id, task_id, workspace_id, parent_session_id, relationship,
                       execution_environment, provider_id, model_id, reasoning_effort, title, status, created_at, updated_at
                FROM (
                    SELECT
                        s.*,
                        ROW_NUMBER() OVER (
                            PARTITION BY s.task_id
                            ORDER BY
                                CASE
                                    WHEN s.relationship = 'sub_agent' THEN 1
                                    ELSE 0
                                END,
                                CASE s.status
                                    WHEN 'active' THEN 0
                                    ELSE 1
                                END,
                                s.updated_at DESC
                        ) AS rn
                    FROM sessions s
                    WHERE s.task_id IN (",
            );
            for i in 0..task_ids.len() {
                if i > 0 {
                    session_sql.push_str(", ");
                }
                session_sql.push('?');
            }
            session_sql.push_str(
                ")
                      AND (s.relationship != 'sub_agent' OR s.relationship IS NULL OR s.archived_at IS NULL)
                ) WHERE rn <= ? ORDER BY task_id, rn",
            );

            let session_sql = self.rewrite_sql(&session_sql);
            let mut session_query = sqlx::query(session_sql.as_ref());
            for task_id in &task_ids {
                session_query = session_query.bind(task_id.0.to_string());
            }
            session_query = session_query.bind(SESSION_LIMIT);
            let session_rows = session_query.fetch_all(&self.pool).await?;

            for r in session_rows {
                let id: String = r.try_get("id")?;
                let task_id: String = r.try_get("task_id")?;
                let ws_id: String = r.try_get("workspace_id")?;
                let created_at: String = r.try_get("created_at")?;
                let updated_at: String = r.try_get("updated_at")?;
                let summary = SessionSummary {
                    id: SessionId(uuid::Uuid::parse_str(&id)?),
                    task_id: TaskId(uuid::Uuid::parse_str(&task_id)?),
                    workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id)?),
                    execution_environment: parse_execution_environment(
                        r.try_get::<String, _>("execution_environment")?.as_str(),
                    )?,
                    parent_session_id: parse_optional_session_id(r.try_get("parent_session_id")?),
                    relationship: r.try_get("relationship")?,
                    provider_id: r.try_get("provider_id")?,
                    model_id: r.try_get("model_id")?,
                    reasoning_effort: r.try_get("reasoning_effort")?,
                    title: r.try_get("title")?,
                    status: parse_session_status(r.try_get::<String, _>("status")?.as_str()),
                    created_at: parse_dt(&created_at)?,
                    updated_at: parse_dt(&updated_at)?,
                };
                if let Some(task_idx) = index_by_task.get(&summary.task_id) {
                    summaries[*task_idx].sessions.push(summary.clone());
                    let summary_task = &mut summaries[*task_idx];
                    let pid = summary.provider_id.trim().to_string();
                    if !pid.is_empty() && !summary_task.provider_ids.contains(&pid) {
                        summary_task.provider_ids.push(pid);
                        summary_task.provider_ids.sort();
                        if summary_task.provider_ids.len() > 3 {
                            summary_task.provider_ids.truncate(3);
                        }
                    }
                }
            }
        }

        Ok(summaries)
    }

    pub(super) async fn list_session_snapshot_rows(
        &self,
        task_ids: &[TaskId],
    ) -> Result<Vec<SessionSnapshotSummary>> {
        if task_ids.is_empty() {
            return Ok(Vec::new());
        }

        let preview_expr = session_snapshot_preview_sql("ss.last_message_preview");
        let mut session_sql = format!(
            r#"
            SELECT
                s.id,
                s.task_id,
                s.workspace_id,
                s.worktree_id,
                s.execution_environment,
                s.parent_session_id,
                s.relationship,
                s.provider_id,
                s.model_id,
                s.reasoning_effort,
                s.title,
                s.agent_role,
                s.status,
                s.provider_session_ref,
                s.parent_session_id,
                s.relationship,
                s.created_at,
                s.updated_at,
                {preview_expr} AS last_message_preview,
                ss.last_message_at AS last_message_at,
                ss.last_event_seq AS last_event_seq,
                COALESCE(ss.projection_rev, COALESCE(ss.last_event_seq, 0)) AS projection_rev,
                ss.last_turn_status AS last_turn_status,
                COALESCE(ss.running_turn_count, 0) AS running_turn_count
            FROM sessions s
            LEFT JOIN session_snapshot_summaries ss
              ON ss.session_id = s.id
            WHERE s.task_id IN ("#,
        );
        for i in 0..task_ids.len() {
            if i > 0 {
                session_sql.push_str(", ");
            }
            session_sql.push('?');
        }
        session_sql.push_str(
            ")
              AND (s.relationship != 'sub_agent' OR s.relationship IS NULL OR s.archived_at IS NULL)
            ORDER BY s.task_id ASC, s.created_at ASC, s.id ASC",
        );

        let session_sql = self.rewrite_sql(&session_sql);
        let mut session_query = sqlx::query(session_sql.as_ref());
        for task_id in task_ids {
            session_query = session_query.bind(task_id.0.to_string());
        }
        let session_rows = session_query.fetch_all(&self.pool).await?;
        let mut out = Vec::with_capacity(session_rows.len());

        for r in session_rows {
            out.push(decode_session_snapshot_summary_row(&r)?);
        }

        Ok(out)
    }

    pub(super) async fn list_session_snapshot_rows_base(
        &self,
        task_ids: &[TaskId],
    ) -> Result<Vec<SessionSnapshotSummary>> {
        if task_ids.is_empty() {
            return Ok(Vec::new());
        }

        let preview_expr = session_snapshot_preview_sql("lm.content");
        let mut session_sql = String::from(
            r#"
            WITH session_scope AS (
                SELECT id
                FROM sessions
                WHERE task_id IN ("#,
        );
        for i in 0..task_ids.len() {
            if i > 0 {
                session_sql.push_str(", ");
            }
            session_sql.push('?');
        }
        session_sql.push_str(
            r#")
                  AND (relationship != 'sub_agent' OR relationship IS NULL OR archived_at IS NULL)
            ),
            last_messages AS (
                SELECT
                    m.session_id,
                    m.content,
                    m.created_at,
                    ROW_NUMBER() OVER (
                        PARTITION BY m.session_id
                        ORDER BY m.created_at DESC,
                                 COALESCE(m.turn_sequence, -1) DESC,
                                 m.id DESC
                    ) AS rn
                FROM messages m
                JOIN session_scope ss ON ss.id = m.session_id
                WHERE m.role IN ('assistant', 'user')
            ),
            last_events AS (
                SELECT e.session_id, MAX(e.seq) AS last_event_seq
                FROM session_events e
                JOIN session_scope ss ON ss.id = e.session_id
                GROUP BY e.session_id
            ),
            last_turns AS (
                SELECT
                    t.session_id,
                    t.status,
                    t.start_seq,
                    ROW_NUMBER() OVER (
                        PARTITION BY t.session_id
                        ORDER BY COALESCE(t.start_seq, -1) DESC,
                                 t.started_at DESC,
                                 t.turn_id DESC
                    ) AS rn
                FROM session_turns t
                JOIN session_scope ss ON ss.id = t.session_id
            ),
            running_turns AS (
                SELECT t.session_id, COUNT(*) AS running_count
                FROM session_turns t
                JOIN session_scope ss ON ss.id = t.session_id
                WHERE t.status = 'running'
                GROUP BY t.session_id
            )
            SELECT
                s.id,
                s.task_id,
                s.workspace_id,
                s.worktree_id,
                s.execution_environment,
                s.parent_session_id,
                s.relationship,
                s.provider_id,
                s.model_id,
                s.reasoning_effort,
                s.title,
                s.agent_role,
                s.status,
                s.provider_session_ref,
                s.created_at,
                s.updated_at,
                {preview_expr} AS last_message_preview,
                lm.content AS last_message_content,
                lm.created_at AS last_message_at,
                le.last_event_seq AS last_event_seq,
                COALESCE(sss.projection_rev, COALESCE(le.last_event_seq, 0)) AS projection_rev,
                lt.status AS last_turn_status,
                COALESCE(rt.running_count, 0) AS running_turn_count
            FROM sessions s
            JOIN session_scope ss ON ss.id = s.id
            LEFT JOIN session_snapshot_summaries sss ON sss.session_id = s.id
            LEFT JOIN last_messages lm ON lm.session_id = s.id AND lm.rn = 1
            LEFT JOIN last_events le ON le.session_id = s.id
            LEFT JOIN last_turns lt ON lt.session_id = s.id AND lt.rn = 1
            LEFT JOIN running_turns rt ON rt.session_id = s.id
            ORDER BY s.task_id ASC, s.created_at ASC, s.id ASC"#,
        );
        let session_sql = session_sql.replace("{preview_expr}", preview_expr.as_str());

        let session_sql = self.rewrite_sql(&session_sql);
        let mut session_query = sqlx::query(session_sql.as_ref());
        for task_id in task_ids {
            session_query = session_query.bind(task_id.0.to_string());
        }
        let session_rows = session_query.fetch_all(&self.pool).await?;
        let mut out = Vec::with_capacity(session_rows.len());

        for r in session_rows {
            out.push(decode_session_snapshot_summary_row(&r)?);
        }

        Ok(out)
    }

    pub(crate) async fn get_session_snapshot_summary(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionSnapshotSummary>> {
        let preview_expr = session_snapshot_preview_sql("lm.content");
        let sql = format!(
            r#"
            WITH last_messages AS (
                SELECT
                    m.session_id,
                    m.content,
                    m.created_at,
                    ROW_NUMBER() OVER (
                        PARTITION BY m.session_id
                        ORDER BY m.created_at DESC,
                                 COALESCE(m.turn_sequence, -1) DESC,
                                 m.id DESC
                    ) AS rn
                FROM messages m
                WHERE m.session_id = ?
                  AND m.role IN ('assistant', 'user')
            ),
            last_events AS (
                SELECT e.session_id, MAX(e.seq) AS last_event_seq
                FROM session_events e
                WHERE e.session_id = ?
                GROUP BY e.session_id
            ),
            last_turns AS (
                SELECT
                    t.session_id,
                    t.status,
                    t.start_seq,
                    ROW_NUMBER() OVER (
                        PARTITION BY t.session_id
                        ORDER BY COALESCE(t.start_seq, -1) DESC,
                                 t.started_at DESC,
                                 t.turn_id DESC
                    ) AS rn
                FROM session_turns t
                WHERE t.session_id = ?
            ),
            running_turns AS (
                SELECT t.session_id, COUNT(*) AS running_count
                FROM session_turns t
                WHERE t.session_id = ?
                  AND t.status = 'running'
                GROUP BY t.session_id
            )
            SELECT
                s.id,
                s.task_id,
                s.workspace_id,
                s.worktree_id,
                s.execution_environment,
                s.parent_session_id,
                s.relationship,
                s.provider_id,
                s.model_id,
                s.reasoning_effort,
                s.title,
                s.agent_role,
                s.status,
                s.provider_session_ref,
                s.parent_session_id,
                s.relationship,
                s.created_at,
                s.updated_at,
                {preview_expr} AS last_message_preview,
                lm.content AS last_message_content,
                lm.created_at AS last_message_at,
                le.last_event_seq AS last_event_seq,
                COALESCE(ss.projection_rev, COALESCE(le.last_event_seq, 0)) AS projection_rev,
                lt.status AS last_turn_status,
                COALESCE(rt.running_count, 0) AS running_turn_count
            FROM sessions s
            LEFT JOIN session_snapshot_summaries ss
              ON ss.session_id = s.id
            LEFT JOIN last_messages lm
              ON lm.session_id = s.id AND lm.rn = 1
            LEFT JOIN last_events le
              ON le.session_id = s.id
            LEFT JOIN last_turns lt
              ON lt.session_id = s.id AND lt.rn = 1
            LEFT JOIN running_turns rt
              ON rt.session_id = s.id
            WHERE s.id = ?"#,
        );
        let sql = self.rewrite_sql(&sql);
        let row = sqlx::query(sql.as_ref())
            .bind(session_id.0.to_string())
            .bind(session_id.0.to_string())
            .bind(session_id.0.to_string())
            .bind(session_id.0.to_string())
            .bind(session_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        let Some(r) = row else {
            return Ok(None);
        };

        Ok(Some(decode_session_snapshot_summary_row(&r)?))
    }
}

impl Store {
    pub async fn list_workspace_active_head_snapshots(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<SessionHeadSnapshot>> {
        crate::fault_injection::maybe_fail("ctx_store.list_workspace_active_head_snapshots")?;
        let stale_rows = self
            .query(
                r#"SELECT s.id
                   FROM sessions s
                   JOIN tasks t ON t.id = s.task_id AND t.primary_session_id = s.id
                   LEFT JOIN session_snapshot_summaries ss ON ss.session_id = s.id
                   LEFT JOIN session_active_snapshot_heads h ON h.session_id = s.id
                   WHERE s.workspace_id = ?
                     AND t.archived_at IS NULL
                     AND (
                         h.session_id IS NULL
                         OR h.last_event_seq != COALESCE(ss.last_event_seq, 0)
                         OR h.head_rev != COALESCE(ss.projection_rev, COALESCE(ss.last_event_seq, 0))
                         OR CASE
                                WHEN h.tool_summaries_json IS NULL THEN 0
                                WHEN json_valid(h.tool_summaries_json)
                                    THEN COALESCE(json_array_length(h.tool_summaries_json), 0)
                                ELSE ?
                            END > ?
                     )
                   ORDER BY s.created_at ASC, s.id ASC"#,
            )
            .bind(workspace_id.0.to_string())
            .bind((ACTIVE_SNAPSHOT_TOOL_SUMMARY_LIMIT as i64) + 1)
            .bind(ACTIVE_SNAPSHOT_TOOL_SUMMARY_LIMIT as i64)
            .fetch_all(&self.pool)
            .await?;
        for row in stale_rows {
            let session_id: String = row.try_get("id")?;
            self.refresh_active_snapshot_head(SessionId(uuid::Uuid::parse_str(&session_id)?), None)
                .await?;
        }

        let rows = self
            .query(
                r#"SELECT s.id AS session_id,
                          s.task_id,
                          s.workspace_id,
                          s.worktree_id,
                          s.execution_environment,
                          s.parent_session_id,
                          s.relationship,
                          s.provider_id,
                          s.model_id,
                          s.reasoning_effort,
                          s.agent_role,
                          s.title,
                          s.status,
                          s.provider_session_ref,
                          s.created_at,
                          s.updated_at,
                          h.last_event_seq,
                          h.turns_json,
                          h.tool_summaries_json,
                          h.messages_json,
                          h.has_more_turns,
                          h.head_window_json,
                          h.summary_checkpoint_json,
                          h.head_rev AS projection_rev
                   FROM session_active_snapshot_heads h
                   JOIN sessions s ON s.id = h.session_id
                   JOIN tasks t ON t.id = s.task_id AND t.primary_session_id = s.id
                   LEFT JOIN session_snapshot_summaries ss ON ss.session_id = s.id
                   WHERE s.workspace_id = ?
                     AND t.archived_at IS NULL
                   ORDER BY s.created_at ASC, s.id ASC"#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let session_id = SessionId(uuid::Uuid::parse_str(
                row.try_get::<String, _>("session_id")?.as_str(),
            )?);
            match decode_active_head_snapshot_row(&row) {
                Ok(snapshot) => out.push(snapshot),
                Err(err) => {
                    tracing::warn!(
                        session_id = %session_id.0,
                        "rebuilding malformed workspace active head snapshot row: {err:#}"
                    );
                    self.refresh_active_snapshot_head(session_id, None).await?;
                    if let Some(snapshot) = self.get_active_snapshot_head(session_id).await? {
                        out.push(snapshot);
                    }
                }
            }
        }
        Ok(out)
    }

    pub async fn get_workspace_active_task_summary(
        &self,
        task_id: TaskId,
    ) -> Result<Option<WorkspaceActiveTaskSummary>> {
        let row = self
            .query(
                r#"SELECT id, workspace_id, title, description, status, exec_plan_id,
                      primary_session_id, primary_worktree_id,
                      created_at, updated_at, archived_at, assistant_seen_at,
                      t.last_assistant_message_at AS last_assistant_message_at,
                      EXISTS(
                        SELECT 1
                        FROM sessions s
                        WHERE s.task_id = t.id AND s.status = 'active'
                      ) AS has_active_session,
                      COALESCE(t.last_activity_at, t.updated_at, t.created_at) AS activity_at
               FROM tasks t
               WHERE id = ?
                 AND archived_at IS NULL
                 AND EXISTS (SELECT 1 FROM sessions s WHERE s.task_id = t.id)"#,
            )
            .bind(task_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        if let Some(r) = row {
            let id: String = r.try_get("id")?;
            let ws_id: String = r.try_get("workspace_id")?;
            let created_at: String = r.try_get("created_at")?;
            let updated_at: String = r.try_get("updated_at")?;
            let archived_at: Option<String> = r.try_get("archived_at")?;
            let assistant_seen_at: Option<String> = r.try_get("assistant_seen_at")?;
            let primary_session_id: Option<String> = r.try_get("primary_session_id")?;
            let primary_worktree_id: Option<String> = r.try_get("primary_worktree_id")?;
            let last_assistant_message_at: Option<String> =
                r.try_get("last_assistant_message_at")?;
            let has_active_session: i64 = r.try_get("has_active_session")?;
            let activity_at: String = r.try_get("activity_at")?;
            let activity_at_dt = parse_dt(&activity_at)?;

            let task = Task {
                id: TaskId(uuid::Uuid::parse_str(&id)?),
                workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id)?),
                title: r.try_get("title")?,
                description: r.try_get("description")?,
                status: parse_task_status(r.try_get::<String, _>("status")?.as_str()),
                created_at: parse_dt(&created_at)?,
                updated_at: parse_dt(&updated_at)?,
                exec_plan_id: r.try_get("exec_plan_id")?,
                primary_session_id: primary_session_id
                    .as_deref()
                    .and_then(|value| uuid::Uuid::parse_str(value).ok())
                    .map(SessionId),
                primary_worktree_id: primary_worktree_id
                    .as_deref()
                    .and_then(|value| uuid::Uuid::parse_str(value).ok())
                    .map(WorktreeId),
                archived_at: archived_at.as_deref().map(parse_dt).transpose()?,
                assistant_seen_at: assistant_seen_at.as_deref().map(parse_dt).transpose()?,
                last_activity_at: Some(activity_at_dt),
                last_assistant_message_at: last_assistant_message_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()?,
                has_active_session: has_active_session != 0,
            };

            let sort_at = task.created_at;
            let summaries = self
                .build_workspace_active_task_summaries(vec![(task, sort_at)])
                .await?;
            return Ok(summaries.into_iter().next());
        }
        Ok(None)
    }

    pub(super) async fn build_workspace_active_task_summaries(
        &self,
        rows: Vec<(Task, DateTime<Utc>)>,
    ) -> Result<Vec<WorkspaceActiveTaskSummary>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let task_ids: Vec<TaskId> = rows.iter().map(|(task, _)| task.id).collect();
        let session_rows = self.list_session_snapshot_rows(&task_ids).await?;
        Ok(Self::build_workspace_active_task_summaries_from_rows(
            rows,
            session_rows,
        ))
    }

    fn build_workspace_active_task_summaries_from_rows(
        seeds: Vec<(Task, DateTime<Utc>)>,
        session_rows: Vec<SessionSnapshotSummary>,
    ) -> Vec<WorkspaceActiveTaskSummary> {
        let mut sessions_by_task: HashMap<TaskId, Vec<SessionSnapshotSummary>> = HashMap::new();
        for summary in session_rows {
            sessions_by_task
                .entry(summary.session.task_id)
                .or_default()
                .push(summary);
        }

        let mut summaries = Vec::with_capacity(seeds.len());
        for (task, sort_at) in seeds {
            let mut task_sessions = sessions_by_task.remove(&task.id).unwrap_or_default();
            if task_sessions.is_empty() {
                continue;
            }

            let mut primary_idx = None;
            let mut include_children = false;
            if let Some(primary_id) = task.primary_session_id {
                if let Some(idx) = task_sessions
                    .iter()
                    .position(|summary| summary.session.id == primary_id)
                {
                    primary_idx = Some(idx);
                    include_children = true;
                }
            }
            if primary_idx.is_none() {
                primary_idx = task_sessions
                    .iter()
                    .position(|summary| summary.session.parent_session_id.is_none());
            }
            if primary_idx.is_none() {
                primary_idx = Some(0);
            }

            let Some(primary_idx) = primary_idx else {
                continue;
            };
            let primary_summary = task_sessions.remove(primary_idx);
            let primary_id = primary_summary.session.id;
            let mut sessions = Vec::new();
            for summary in task_sessions {
                let include = if include_children {
                    summary.session.parent_session_id == Some(primary_id)
                } else {
                    summary.session.parent_session_id.is_none()
                };
                if include {
                    sessions.push(summary);
                }
            }

            summaries.push(WorkspaceActiveTaskSummary {
                task,
                primary_session: primary_summary,
                primary_session_head: None,
                sessions,
                sort_at,
            });
        }

        summaries
    }
}

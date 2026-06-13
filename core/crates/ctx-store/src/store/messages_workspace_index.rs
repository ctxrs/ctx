use super::*;

impl Store {
    pub async fn workspace_task_counts(&self, workspace_id: WorkspaceId) -> Result<(i64, i64)> {
        let active: i64 = self
            .query_scalar(
                r#"SELECT COUNT(*) FROM tasks WHERE workspace_id = ? AND archived_at IS NULL"#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;

        let archived: i64 = self
            .query_scalar(
                r#"SELECT COUNT(*) FROM tasks WHERE workspace_id = ? AND archived_at IS NOT NULL"#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;

        Ok((active, archived))
    }

    pub async fn count_active_tasks_for_worktree(
        &self,
        worktree_id: WorktreeId,
        exclude_task_id: Option<TaskId>,
    ) -> Result<i64> {
        let count: i64 = if let Some(task_id) = exclude_task_id {
            self.query_scalar(
                r#"SELECT COUNT(*) FROM tasks t
                   WHERE t.archived_at IS NULL
                     AND t.id != ?
                     AND (
                       t.primary_worktree_id = ?
                       OR EXISTS(
                         SELECT 1 FROM sessions s
                         WHERE s.task_id = t.id AND s.worktree_id = ?
                       )
                     )"#,
            )
            .bind(task_id.0.to_string())
            .bind(worktree_id.0.to_string())
            .bind(worktree_id.0.to_string())
            .fetch_one(&self.pool)
            .await?
        } else {
            self.query_scalar(
                r#"SELECT COUNT(*) FROM tasks t
                   WHERE t.archived_at IS NULL
                     AND (
                       t.primary_worktree_id = ?
                       OR EXISTS(
                         SELECT 1 FROM sessions s
                         WHERE s.task_id = t.id AND s.worktree_id = ?
                       )
                     )"#,
            )
            .bind(worktree_id.0.to_string())
            .bind(worktree_id.0.to_string())
            .fetch_one(&self.pool)
            .await?
        };
        Ok(count)
    }

    pub async fn count_tasks_for_worktree(
        &self,
        worktree_id: WorktreeId,
        exclude_task_id: Option<TaskId>,
    ) -> Result<i64> {
        let count: i64 = if let Some(task_id) = exclude_task_id {
            self.query_scalar(
                r#"SELECT COUNT(*) FROM tasks t
                   WHERE t.id != ?
                     AND (
                       t.primary_worktree_id = ?
                       OR EXISTS(
                         SELECT 1 FROM sessions s
                         WHERE s.task_id = t.id AND s.worktree_id = ?
                       )
                     )"#,
            )
            .bind(task_id.0.to_string())
            .bind(worktree_id.0.to_string())
            .bind(worktree_id.0.to_string())
            .fetch_one(&self.pool)
            .await?
        } else {
            self.query_scalar(
                r#"SELECT COUNT(*) FROM tasks t
                   WHERE t.primary_worktree_id = ?
                      OR EXISTS(
                        SELECT 1 FROM sessions s
                        WHERE s.task_id = t.id AND s.worktree_id = ?
                      )"#,
            )
            .bind(worktree_id.0.to_string())
            .bind(worktree_id.0.to_string())
            .fetch_one(&self.pool)
            .await?
        };
        Ok(count)
    }

    pub async fn list_workspace_index_page(
        &self,
        workspace_id: WorkspaceId,
        cursor: Option<WorkspaceIndexCursor>,
        limit: i64,
        include_archived: bool,
    ) -> Result<(Vec<WorkspaceTaskSummary>, Option<WorkspaceIndexCursor>)> {
        let filter = if include_archived { None } else { Some(false) };
        self.list_workspace_index_page_filtered(workspace_id, cursor, limit, filter)
            .await
    }

    pub async fn list_workspace_archived_page(
        &self,
        workspace_id: WorkspaceId,
        cursor: Option<WorkspaceIndexCursor>,
        limit: i64,
    ) -> Result<(Vec<WorkspaceTaskSummary>, Option<WorkspaceIndexCursor>)> {
        self.list_workspace_index_page_filtered(workspace_id, cursor, limit, Some(true))
            .await
    }

    pub(super) async fn list_workspace_index_page_filtered(
        &self,
        workspace_id: WorkspaceId,
        cursor: Option<WorkspaceIndexCursor>,
        limit: i64,
        archived_only: Option<bool>,
    ) -> Result<(Vec<WorkspaceTaskSummary>, Option<WorkspaceIndexCursor>)> {
        const MAX_LIMIT: i64 = 200;
        let limit = limit.clamp(1, MAX_LIMIT);

        const ACTIVITY_EXPR: &str = "COALESCE(t.last_activity_at, t.updated_at, t.created_at)";
        const SORT_EXPR: &str = "COALESCE(t.archived_at, t.created_at)";

        let mut sql = format!(
            r#"
            SELECT
              t.id,
              t.workspace_id,
              t.title,
              t.description,
              t.status,
              t.exec_plan_id,
              t.primary_session_id,
              t.primary_worktree_id,
              t.created_at,
              t.updated_at,
              t.archived_at,
              t.assistant_seen_at,
              t.last_assistant_message_at AS last_assistant_message_at,
              EXISTS(
                SELECT 1
                FROM sessions s
                WHERE s.task_id = t.id AND s.status = 'active'
              ) AS has_active_session,
              ({ACTIVITY_EXPR}) AS activity_at,
              ({SORT_EXPR}) AS sort_at
            FROM tasks t
            WHERE t.workspace_id = ?
            "#
        );

        if let Some(archived_only) = archived_only {
            if archived_only {
                sql.push_str(" AND t.archived_at IS NOT NULL");
            } else {
                sql.push_str(" AND t.archived_at IS NULL");
            }
        }

        if cursor.is_some() {
            sql.push_str(&format!(
                " AND (({SORT_EXPR}) < ? OR (({SORT_EXPR}) = ? AND t.id < ?))"
            ));
        }

        sql.push_str(" ORDER BY sort_at DESC, t.id DESC LIMIT ?");

        let sql = self.rewrite_sql(&sql);
        let mut query = sqlx::query(sql.as_ref()).bind(workspace_id.0.to_string());

        if let Some(cursor) = &cursor {
            let cursor_ts = cursor.sort_at.to_rfc3339();
            query = query
                .bind(cursor_ts.clone())
                .bind(cursor_ts)
                .bind(cursor.task_id.0.to_string());
        }

        query = query.bind(limit + 1);

        let rows = query.fetch_all(&self.pool).await?;

        let mut task_rows = Vec::with_capacity(rows.len());
        for r in rows {
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
            let sort_at: String = r.try_get("sort_at")?;
            let sort_at_dt = parse_dt(&sort_at)?;

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
            task_rows.push((task, sort_at_dt));
        }

        let mut next_cursor: Option<WorkspaceIndexCursor> = None;
        if task_rows.len() as i64 > limit {
            if let Some((task, sort_at)) = task_rows.pop() {
                next_cursor = Some(WorkspaceIndexCursor {
                    sort_at,
                    task_id: task.id,
                });
            }
        }

        if task_rows.is_empty() {
            return Ok((Vec::new(), next_cursor));
        }

        let summaries = self.build_workspace_task_summaries(task_rows).await?;

        Ok((summaries, next_cursor))
    }

    pub async fn get_workspace_task_summary(
        &self,
        task_id: TaskId,
    ) -> Result<Option<WorkspaceTaskSummary>> {
        const ACTIVITY_EXPR: &str = "COALESCE(t.last_activity_at, t.updated_at, t.created_at)";
        const SORT_EXPR: &str = "COALESCE(t.archived_at, t.created_at)";
        let sql = format!(
            r#"
            SELECT
              t.id,
              t.workspace_id,
              t.title,
              t.description,
              t.status,
              t.exec_plan_id,
              t.primary_session_id,
              t.primary_worktree_id,
              t.created_at,
              t.updated_at,
              t.archived_at,
              t.assistant_seen_at,
              t.last_assistant_message_at AS last_assistant_message_at,
              EXISTS(
                SELECT 1
                FROM sessions s
                WHERE s.task_id = t.id AND s.status = 'active'
              ) AS has_active_session,
              ({ACTIVITY_EXPR}) AS activity_at,
              ({SORT_EXPR}) AS sort_at
            FROM tasks t
            WHERE t.id = ?
            LIMIT 1
            "#
        );

        let sql = self.rewrite_sql(&sql);
        if let Some(r) = sqlx::query(sql.as_ref())
            .bind(task_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?
        {
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
            let sort_at: String = r.try_get("sort_at")?;
            let sort_at_dt = parse_dt(&sort_at)?;

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

            let summaries = self
                .build_workspace_task_summaries(vec![(task, sort_at_dt)])
                .await?;
            Ok(summaries.into_iter().next())
        } else {
            Ok(None)
        }
    }
}

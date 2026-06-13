impl Store {
    pub async fn list_workspace_active_page(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<(Vec<WorkspaceActiveTaskSummary>, i64)> {
        let limit = limit.clamp(1, WORKSPACE_ACTIVE_PAGE_MAX_LIMIT);
        let total_count = self.count_workspace_active_tasks(workspace_id).await?;
        let task_rows = self
            .list_workspace_active_task_rows(workspace_id, limit)
            .await?;

        if task_rows.is_empty() {
            return Ok((Vec::new(), total_count));
        }

        let summaries = self
            .build_workspace_active_task_summaries(task_rows)
            .await?;
        Ok((summaries, total_count))
    }

    pub async fn list_workspace_active_page_without_total(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<Vec<WorkspaceActiveTaskSummary>> {
        let limit = limit.clamp(1, WORKSPACE_ACTIVE_PAGE_MAX_LIMIT);
        let task_rows = self
            .list_workspace_active_task_rows(workspace_id, limit)
            .await?;
        if task_rows.is_empty() {
            return Ok(Vec::new());
        }
        self.build_workspace_active_task_summaries(task_rows).await
    }

    pub async fn list_workspace_active_page_base(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<(Vec<WorkspaceActiveTaskSummary>, i64)> {
        let limit = limit.clamp(1, WORKSPACE_ACTIVE_PAGE_MAX_LIMIT);
        let total_count = self.count_workspace_active_tasks(workspace_id).await?;
        let task_rows = self
            .list_workspace_active_task_rows(workspace_id, limit)
            .await?;

        if task_rows.is_empty() {
            return Ok((Vec::new(), total_count));
        }

        let task_ids: Vec<TaskId> = task_rows.iter().map(|(task, _)| task.id).collect();
        let session_rows = self.list_session_snapshot_rows_base(&task_ids).await?;
        let summaries =
            Self::build_workspace_active_task_summaries_from_rows(task_rows, session_rows);
        Ok((summaries, total_count))
    }

    async fn count_workspace_active_tasks(&self, workspace_id: WorkspaceId) -> Result<i64> {
        let total_count = self
            .query_scalar(
                r#"SELECT COUNT(*)
               FROM tasks t
               WHERE t.workspace_id = ?
                 AND t.archived_at IS NULL
                 AND EXISTS (SELECT 1 FROM sessions s WHERE s.task_id = t.id)"#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(total_count)
    }

    async fn list_workspace_active_task_rows(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<Vec<(Task, DateTime<Utc>)>> {
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
              ({WORKSPACE_ACTIVE_ACTIVITY_EXPR}) AS activity_at
            FROM tasks t
            WHERE t.workspace_id = ?
              AND t.archived_at IS NULL
              AND EXISTS (SELECT 1 FROM sessions s WHERE s.task_id = t.id)
            ORDER BY t.created_at DESC, t.id DESC
            LIMIT ?
            "#
        );

        let sql = self.rewrite_sql(&sql);
        let rows = sqlx::query(sql.as_ref())
            .bind(workspace_id.0.to_string())
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

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
            task_rows.push((task, sort_at));
        }

        Ok(task_rows)
    }

    pub async fn list_workspace_active_session_ids(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<SessionId>> {
        let rows = self
            .query(
                r#"SELECT s.id
               FROM tasks t
               JOIN sessions s ON s.id = t.primary_session_id
               WHERE t.workspace_id = ?
                 AND t.archived_at IS NULL
               ORDER BY t.created_at ASC, t.id ASC"#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id")?;
            out.push(SessionId(uuid::Uuid::parse_str(&id)?));
        }
        Ok(out)
    }

    pub async fn get_workspace_active_snapshot_state(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(i64, i64)> {
        crate::fault_injection::maybe_fail("ctx_store.get_workspace_active_snapshot_state")?;
        let row = self
            .query(
                r#"SELECT snapshot_rev, archived_rev
               FROM workspace_active_snapshot_state
               WHERE workspace_id = ?"#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|r| {
                (
                    r.try_get("snapshot_rev").unwrap_or(0),
                    r.try_get("archived_rev").unwrap_or(0),
                )
            })
            .unwrap_or((0, 0)))
    }

    pub async fn bump_workspace_active_snapshot_rev(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<i64> {
        let (snapshot_rev, _) = self
            .get_workspace_active_snapshot_state(workspace_id)
            .await?;
        Ok(snapshot_rev)
    }

    pub async fn bump_workspace_archived_snapshot_rev(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        let archived_rev: i64 = self
            .query_scalar(
                r#"INSERT INTO workspace_active_snapshot_state (
                    workspace_id, snapshot_rev, archived_rev, updated_at
               )
               VALUES (?, 0, 1, ?)
               ON CONFLICT(workspace_id) DO UPDATE SET
                   archived_rev = workspace_active_snapshot_state.archived_rev + 1,
                   updated_at = excluded.updated_at
               RETURNING archived_rev"#,
            )
            .bind(workspace_id.0.to_string())
            .bind(&now)
            .fetch_one(&self.pool)
            .await?;
        Ok(archived_rev)
    }
}

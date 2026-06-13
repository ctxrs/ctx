use super::*;

pub struct CreateTaskInsertResult {
    pub task: Task,
    pub created: bool,
}

impl Store {
    // Task APIs
    pub async fn list_tasks(&self, workspace_id: WorkspaceId) -> Result<Vec<Task>> {
        let rows = self
            .query(
                r#"
            SELECT
              t.id, t.workspace_id, t.title, t.description, t.status, t.exec_plan_id,
              t.primary_session_id, t.primary_worktree_id,
              t.created_at, t.updated_at, t.archived_at, t.assistant_seen_at,
              t.last_activity_at,
              t.last_assistant_message_at,
              EXISTS(
                SELECT 1
                FROM sessions s
                WHERE s.task_id = t.id AND s.status = 'active'
              ) AS has_active_session
            FROM tasks t
            WHERE t.workspace_id = ?
            ORDER BY COALESCE(last_activity_at, t.updated_at, t.created_at) DESC
            "#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.try_get("id")?;
            let ws_id: String = r.try_get("workspace_id")?;
            let created_at: String = r.try_get("created_at")?;
            let updated_at: String = r.try_get("updated_at")?;
            let archived_at: Option<String> = r.try_get("archived_at")?;
            let assistant_seen_at: Option<String> = r.try_get("assistant_seen_at")?;
            let primary_session_id: Option<String> = r.try_get("primary_session_id")?;
            let primary_worktree_id: Option<String> = r.try_get("primary_worktree_id")?;
            let last_activity_at: Option<String> = r.try_get("last_activity_at")?;
            let last_assistant_message_at: Option<String> =
                r.try_get("last_assistant_message_at")?;
            let has_active_session: i64 = r.try_get("has_active_session")?;
            out.push(Task {
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
                last_activity_at: last_activity_at.as_deref().map(parse_dt).transpose()?,
                last_assistant_message_at: last_assistant_message_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()?,
                has_active_session: has_active_session != 0,
            });
        }
        Ok(out)
    }

    pub async fn create_task(
        &self,
        workspace_id: WorkspaceId,
        title: String,
        description: Option<String>,
    ) -> Result<Task> {
        Ok(self
            .create_task_with_id_result(workspace_id, TaskId::new(), title, description)
            .await?
            .task)
    }

    pub async fn create_task_with_id(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
        title: String,
        description: Option<String>,
    ) -> Result<Task> {
        Ok(self
            .create_task_with_id_result(workspace_id, task_id, title, description)
            .await?
            .task)
    }

    pub async fn create_task_with_id_result(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
        title: String,
        description: Option<String>,
    ) -> Result<CreateTaskInsertResult> {
        let now = Utc::now();
        let task = Task {
            id: task_id,
            workspace_id,
            title,
            description,
            status: TaskStatus::Pending,
            created_at: now,
            updated_at: now,
            exec_plan_id: None,
            primary_session_id: None,
            primary_worktree_id: None,
            archived_at: None,
            assistant_seen_at: None,
            last_activity_at: None,
            last_assistant_message_at: None,
            has_active_session: false,
        };
        let result = self.query(
            r#"INSERT INTO tasks (id, workspace_id, title, description, status, exec_plan_id, primary_session_id, primary_worktree_id, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(id) DO NOTHING"#,
        )
        .bind(task.id.0.to_string())
        .bind(task.workspace_id.0.to_string())
        .bind(&task.title)
        .bind(&task.description)
        .bind(task_status_to_str(&task.status))
        .bind(&task.exec_plan_id)
        .bind(task.primary_session_id.map(|id| id.0.to_string()))
        .bind(task.primary_worktree_id.map(|id| id.0.to_string()))
        .bind(task.created_at.to_rfc3339())
        .bind(task.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return self
                .get_task(task_id)
                .await?
                .map(|task| CreateTaskInsertResult {
                    task,
                    created: false,
                })
                .ok_or_else(|| anyhow::anyhow!("task exists but could not be loaded"));
        }

        Ok(CreateTaskInsertResult {
            task,
            created: true,
        })
    }

    pub async fn get_task(&self, id: TaskId) -> Result<Option<Task>> {
        let row = self
            .query(
                r#"SELECT id, workspace_id, title, description, status, exec_plan_id,
                      primary_session_id, primary_worktree_id,
                      created_at, updated_at, archived_at, assistant_seen_at,
                      last_activity_at, last_assistant_message_at
               FROM tasks WHERE id = ?"#,
            )
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.and_then(|r| {
            let id: String = r.try_get("id").ok()?;
            let ws_id: String = r.try_get("workspace_id").ok()?;
            let created_at: String = r.try_get("created_at").ok()?;
            let updated_at: String = r.try_get("updated_at").ok()?;
            let archived_at: Option<String> = r.try_get("archived_at").ok()?;
            let assistant_seen_at: Option<String> = r.try_get("assistant_seen_at").ok()?;
            let last_activity_at: Option<String> = r.try_get("last_activity_at").ok()?;
            let last_assistant_message_at: Option<String> =
                r.try_get("last_assistant_message_at").ok()?;
            let primary_session_id: Option<String> = r.try_get("primary_session_id").ok()?;
            let primary_worktree_id: Option<String> = r.try_get("primary_worktree_id").ok()?;
            Some(Task {
                id: TaskId(uuid::Uuid::parse_str(&id).ok()?),
                workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id).ok()?),
                title: r.try_get("title").ok()?,
                description: r.try_get("description").ok()?,
                status: parse_task_status(r.try_get::<String, _>("status").ok()?.as_str()),
                created_at: parse_dt(&created_at).ok()?,
                updated_at: parse_dt(&updated_at).ok()?,
                exec_plan_id: r.try_get("exec_plan_id").ok()?,
                primary_session_id: primary_session_id
                    .as_deref()
                    .and_then(|value| uuid::Uuid::parse_str(value).ok())
                    .map(SessionId),
                primary_worktree_id: primary_worktree_id
                    .as_deref()
                    .and_then(|value| uuid::Uuid::parse_str(value).ok())
                    .map(WorktreeId),
                archived_at: archived_at.as_deref().map(parse_dt).transpose().ok()?,
                assistant_seen_at: assistant_seen_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()
                    .ok()?,
                last_activity_at: last_activity_at.as_deref().map(parse_dt).transpose().ok()?,
                last_assistant_message_at: last_assistant_message_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()
                    .ok()?,
                has_active_session: false,
            })
        }))
    }

    pub async fn archive_task(&self, id: TaskId) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let res = self
            .query(
                r#"UPDATE tasks
               SET archived_at = ?, updated_at = ?
               WHERE id = ?"#,
            )
            .bind(&now)
            .bind(&now)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        if res.rows_affected() > 0 {
            self.materialize_archived_heads_for_task(id).await?;
            self.delete_session_head_materializations_for_task(id, SessionHeadKind::Active)
                .await?;
            self.delete_active_snapshot_heads_for_task(id).await?;
        }
        Ok(res.rows_affected() > 0)
    }

    pub async fn unarchive_task(&self, id: TaskId) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let res = self
            .query(
                r#"UPDATE tasks
               SET archived_at = NULL, updated_at = ?
               WHERE id = ?"#,
            )
            .bind(&now)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        if res.rows_affected() > 0 {
            self.delete_session_head_materializations_for_task(id, SessionHeadKind::Archived)
                .await?;
            self.refresh_active_snapshot_heads_for_task(id).await?;
        }
        Ok(res.rows_affected() > 0)
    }

    pub async fn update_task_title(&self, id: TaskId, title: String) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let res = self
            .query(
                r#"UPDATE tasks
               SET title = ?, updated_at = ?
               WHERE id = ?"#,
            )
            .bind(title)
            .bind(&now)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn set_task_primary_session(
        &self,
        id: TaskId,
        session_id: SessionId,
        worktree_id: WorktreeId,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let res = self
            .query(
                r#"UPDATE tasks
               SET primary_session_id = ?, primary_worktree_id = ?, updated_at = ?
               WHERE id = ?"#,
            )
            .bind(session_id.0.to_string())
            .bind(worktree_id.0.to_string())
            .bind(&now)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn set_task_primary_worktree(
        &self,
        id: TaskId,
        worktree_id: WorktreeId,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let res = self
            .query(
                r#"UPDATE tasks
               SET primary_worktree_id = ?, updated_at = ?
               WHERE id = ?"#,
            )
            .bind(worktree_id.0.to_string())
            .bind(&now)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn mark_task_read(&self, id: TaskId) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let res = self
            .query(
                r#"UPDATE tasks
               SET assistant_seen_at = ?
               WHERE id = ?"#,
            )
            .bind(&now)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn mark_task_unread(&self, id: TaskId) -> Result<bool> {
        let res = self
            .query(
                r#"UPDATE tasks
               SET assistant_seen_at = NULL
               WHERE id = ?"#,
            )
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn delete_task(&self, id: TaskId) -> Result<bool> {
        let res = self
            .query(r#"DELETE FROM tasks WHERE id = ?"#)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn get_task_with_activity(&self, id: TaskId) -> Result<Option<Task>> {
        let row = self
            .query(
                r#"
            SELECT
              t.id, t.workspace_id, t.title, t.description, t.status, t.exec_plan_id,
              t.primary_session_id, t.primary_worktree_id,
              t.created_at, t.updated_at, t.archived_at, t.assistant_seen_at,
              t.last_activity_at,
              t.last_assistant_message_at,
              EXISTS(
                SELECT 1
                FROM sessions s
                WHERE s.task_id = t.id AND s.status = 'active'
              ) AS has_active_session
            FROM tasks t
            WHERE t.id = ?
            "#,
            )
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.and_then(|r| {
            let id: String = r.try_get("id").ok()?;
            let ws_id: String = r.try_get("workspace_id").ok()?;
            let created_at: String = r.try_get("created_at").ok()?;
            let updated_at: String = r.try_get("updated_at").ok()?;
            let archived_at: Option<String> = r.try_get("archived_at").ok()?;
            let assistant_seen_at: Option<String> = r.try_get("assistant_seen_at").ok()?;
            let primary_session_id: Option<String> = r.try_get("primary_session_id").ok()?;
            let primary_worktree_id: Option<String> = r.try_get("primary_worktree_id").ok()?;
            let last_activity_at: Option<String> = r.try_get("last_activity_at").ok()?;
            let last_assistant_message_at: Option<String> =
                r.try_get("last_assistant_message_at").ok()?;
            let has_active_session: i64 = r.try_get("has_active_session").ok()?;
            Some(Task {
                id: TaskId(uuid::Uuid::parse_str(&id).ok()?),
                workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id).ok()?),
                title: r.try_get("title").ok()?,
                description: r.try_get("description").ok()?,
                status: parse_task_status(r.try_get::<String, _>("status").ok()?.as_str()),
                created_at: parse_dt(&created_at).ok()?,
                updated_at: parse_dt(&updated_at).ok()?,
                exec_plan_id: r.try_get("exec_plan_id").ok()?,
                primary_session_id: primary_session_id
                    .as_deref()
                    .and_then(|value| uuid::Uuid::parse_str(value).ok())
                    .map(SessionId),
                primary_worktree_id: primary_worktree_id
                    .as_deref()
                    .and_then(|value| uuid::Uuid::parse_str(value).ok())
                    .map(WorktreeId),
                archived_at: archived_at.as_deref().map(parse_dt).transpose().ok()?,
                assistant_seen_at: assistant_seen_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()
                    .ok()?,
                last_activity_at: last_activity_at.as_deref().map(parse_dt).transpose().ok()?,
                last_assistant_message_at: last_assistant_message_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()
                    .ok()?,
                has_active_session: has_active_session != 0,
            })
        }))
    }
}

use super::*;

impl Store {
    // Workspace APIs
    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let rows = self
            .query(
                r#"SELECT id, name, root_path, created_at, vcs_kind FROM workspaces ORDER BY created_at ASC"#,
            )
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.try_get("id")?;
            let name: String = r.try_get("name")?;
            let root_path: String = r.try_get("root_path")?;
            let created_at: String = r.try_get("created_at")?;
            let vcs_kind: Option<String> = r.try_get("vcs_kind").ok();
            out.push(Workspace {
                id: WorkspaceId(uuid::Uuid::parse_str(&id)?),
                name,
                root_path,
                created_at: parse_dt(&created_at)?,
                vcs_kind: parse_vcs_kind(vcs_kind),
            });
        }
        Ok(out)
    }

    pub async fn get_workspace(&self, id: WorkspaceId) -> Result<Option<Workspace>> {
        let row = self
            .query(
                r#"SELECT id, name, root_path, created_at, vcs_kind FROM workspaces WHERE id = ?"#,
            )
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.and_then(|r| {
            let id: String = r.try_get("id").ok()?;
            let vcs_kind: Option<String> = r.try_get("vcs_kind").ok()?;
            Some(Workspace {
                id: WorkspaceId(uuid::Uuid::parse_str(&id).ok()?),
                name: r.try_get("name").ok()?,
                root_path: r.try_get("root_path").ok()?,
                created_at: parse_dt(r.try_get::<String, _>("created_at").ok()?.as_str()).ok()?,
                vcs_kind: parse_vcs_kind(vcs_kind),
            })
        }))
    }

    pub async fn create_workspace(
        &self,
        name: String,
        root_path: String,
        vcs_kind: VcsKind,
    ) -> Result<Workspace> {
        let workspace = Workspace {
            id: WorkspaceId::new(),
            name,
            root_path,
            created_at: Utc::now(),
            vcs_kind: Some(vcs_kind),
        };
        self.query(
            r#"INSERT INTO workspaces (id, name, root_path, created_at, vcs_kind) VALUES (?, ?, ?, ?, ?)"#,
        )
        .bind(workspace.id.0.to_string())
        .bind(&workspace.name)
        .bind(&workspace.root_path)
        .bind(workspace.created_at.to_rfc3339())
        .bind(workspace.vcs_kind.as_ref().map(vcs_kind_to_str))
        .execute(&self.pool)
        .await?;
        Ok(workspace)
    }

    pub async fn delete_workspace(&self, id: WorkspaceId) -> Result<()> {
        self.query(r#"DELETE FROM workspaces WHERE id = ?"#)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upsert_workspace(&self, workspace: &Workspace) -> Result<()> {
        self.query(
            r#"INSERT INTO workspaces (id, name, root_path, created_at, vcs_kind)
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 root_path = excluded.root_path,
                 vcs_kind = excluded.vcs_kind"#,
        )
        .bind(workspace.id.0.to_string())
        .bind(&workspace.name)
        .bind(&workspace.root_path)
        .bind(workspace.created_at.to_rfc3339())
        .bind(workspace.vcs_kind.as_ref().map(vcs_kind_to_str))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_workspace_task_index(
        &self,
        task_id: TaskId,
        workspace_id: WorkspaceId,
    ) -> Result<()> {
        self.query(
            r#"INSERT INTO workspace_task_index (task_id, workspace_id)
               VALUES (?, ?)
               ON CONFLICT(task_id) DO UPDATE SET workspace_id = excluded.workspace_id"#,
        )
        .bind(task_id.0.to_string())
        .bind(workspace_id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_workspace_session_index(
        &self,
        session_id: SessionId,
        workspace_id: WorkspaceId,
    ) -> Result<()> {
        self.query(
            r#"INSERT INTO workspace_session_index (session_id, workspace_id)
               VALUES (?, ?)
               ON CONFLICT(session_id) DO UPDATE SET workspace_id = excluded.workspace_id"#,
        )
        .bind(session_id.0.to_string())
        .bind(workspace_id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_workspace_worktree_index(
        &self,
        worktree_id: WorktreeId,
        workspace_id: WorkspaceId,
    ) -> Result<()> {
        self.query(
            r#"INSERT INTO workspace_worktree_index (worktree_id, workspace_id)
               VALUES (?, ?)
               ON CONFLICT(worktree_id) DO UPDATE SET workspace_id = excluded.workspace_id"#,
        )
        .bind(worktree_id.0.to_string())
        .bind(workspace_id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_workspace_task_index(&self, task_id: TaskId) -> Result<()> {
        self.query(r#"DELETE FROM workspace_task_index WHERE task_id = ?"#)
            .bind(task_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_workspace_session_index(&self, session_id: SessionId) -> Result<()> {
        self.query(r#"DELETE FROM workspace_session_index WHERE session_id = ?"#)
            .bind(session_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_workspace_worktree_index(&self, worktree_id: WorktreeId) -> Result<()> {
        self.query(r#"DELETE FROM workspace_worktree_index WHERE worktree_id = ?"#)
            .bind(worktree_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_workspace_id_for_task(&self, task_id: TaskId) -> Result<Option<WorkspaceId>> {
        let row = self
            .query(r#"SELECT workspace_id FROM workspace_task_index WHERE task_id = ?"#)
            .bind(task_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| {
            let value: String = r.try_get("workspace_id").ok()?;
            uuid::Uuid::parse_str(&value).ok().map(WorkspaceId)
        }))
    }

    pub async fn get_workspace_id_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<WorkspaceId>> {
        let row = self
            .query(r#"SELECT workspace_id FROM workspace_session_index WHERE session_id = ?"#)
            .bind(session_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| {
            let value: String = r.try_get("workspace_id").ok()?;
            uuid::Uuid::parse_str(&value).ok().map(WorkspaceId)
        }))
    }

    pub async fn get_workspace_id_for_worktree(
        &self,
        worktree_id: WorktreeId,
    ) -> Result<Option<WorkspaceId>> {
        let row = self
            .query(r#"SELECT workspace_id FROM workspace_worktree_index WHERE worktree_id = ?"#)
            .bind(worktree_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| {
            let value: String = r.try_get("workspace_id").ok()?;
            uuid::Uuid::parse_str(&value).ok().map(WorkspaceId)
        }))
    }

    pub async fn refresh_workspace_indexes(&self, workspace_id: WorkspaceId) -> Result<()> {
        let workspace_id = workspace_id.0.to_string();
        self.query(
            r#"INSERT INTO workspace_task_index (task_id, workspace_id)
               SELECT id, workspace_id FROM tasks WHERE workspace_id = ?
               ON CONFLICT(task_id) DO UPDATE SET
                   workspace_id = excluded.workspace_id"#,
        )
        .bind(&workspace_id)
        .execute(&self.pool)
        .await?;

        self.query(
            r#"INSERT INTO workspace_session_index (session_id, workspace_id)
               SELECT id, workspace_id FROM sessions WHERE workspace_id = ?
               ON CONFLICT(session_id) DO UPDATE SET
                   workspace_id = excluded.workspace_id"#,
        )
        .bind(&workspace_id)
        .execute(&self.pool)
        .await?;

        self.query(
            r#"INSERT INTO workspace_worktree_index (worktree_id, workspace_id)
               SELECT id, workspace_id FROM worktrees WHERE workspace_id = ?
               ON CONFLICT(worktree_id) DO UPDATE SET
                   workspace_id = excluded.workspace_id"#,
        )
        .bind(&workspace_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_workspace_indexes(&self, workspace_id: WorkspaceId) -> Result<()> {
        let workspace_id = workspace_id.0.to_string();
        self.query(r#"DELETE FROM workspace_task_index WHERE workspace_id = ?"#)
            .bind(&workspace_id)
            .execute(&self.pool)
            .await?;
        self.query(r#"DELETE FROM workspace_session_index WHERE workspace_id = ?"#)
            .bind(&workspace_id)
            .execute(&self.pool)
            .await?;
        self.query(r#"DELETE FROM workspace_worktree_index WHERE workspace_id = ?"#)
            .bind(&workspace_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

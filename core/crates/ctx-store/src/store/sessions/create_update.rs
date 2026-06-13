impl Store {
    #[allow(clippy::too_many_arguments)]
    pub async fn create_session(
        &self,
        task_id: TaskId,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        execution_environment: ExecutionEnvironment,
        provider_id: String,
        model_id: String,
        agent_role: String,
        parent_session_id: Option<SessionId>,
        relationship: Option<String>,
        provider_session_ref: Option<String>,
    ) -> Result<Session> {
        self.create_session_with_id_inner(
            SessionId::new(),
            task_id,
            workspace_id,
            worktree_id,
            execution_environment,
            provider_id,
            model_id,
            None,
            agent_role,
            parent_session_id,
            relationship,
            provider_session_ref,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_session_with_reasoning_effort(
        &self,
        task_id: TaskId,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        execution_environment: ExecutionEnvironment,
        provider_id: String,
        model_id: String,
        reasoning_effort: Option<String>,
        agent_role: String,
        parent_session_id: Option<SessionId>,
        relationship: Option<String>,
        provider_session_ref: Option<String>,
    ) -> Result<Session> {
        self.create_session_with_id_inner(
            SessionId::new(),
            task_id,
            workspace_id,
            worktree_id,
            execution_environment,
            provider_id,
            model_id,
            reasoning_effort,
            agent_role,
            parent_session_id,
            relationship,
            provider_session_ref,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_session_with_id(
        &self,
        session_id: SessionId,
        task_id: TaskId,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        execution_environment: ExecutionEnvironment,
        provider_id: String,
        model_id: String,
        agent_role: String,
        parent_session_id: Option<SessionId>,
        relationship: Option<String>,
        provider_session_ref: Option<String>,
    ) -> Result<Session> {
        self.create_session_with_id_inner(
            session_id,
            task_id,
            workspace_id,
            worktree_id,
            execution_environment,
            provider_id,
            model_id,
            None,
            agent_role,
            parent_session_id,
            relationship,
            provider_session_ref,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_session_with_id_and_reasoning_effort(
        &self,
        session_id: SessionId,
        task_id: TaskId,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        execution_environment: ExecutionEnvironment,
        provider_id: String,
        model_id: String,
        reasoning_effort: Option<String>,
        agent_role: String,
        parent_session_id: Option<SessionId>,
        relationship: Option<String>,
        provider_session_ref: Option<String>,
    ) -> Result<Session> {
        self.create_session_with_id_inner(
            session_id,
            task_id,
            workspace_id,
            worktree_id,
            execution_environment,
            provider_id,
            model_id,
            reasoning_effort,
            agent_role,
            parent_session_id,
            relationship,
            provider_session_ref,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_session_with_id_inner(
        &self,
        session_id: SessionId,
        task_id: TaskId,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        execution_environment: ExecutionEnvironment,
        provider_id: String,
        model_id: String,
        reasoning_effort: Option<String>,
        agent_role: String,
        parent_session_id: Option<SessionId>,
        relationship: Option<String>,
        provider_session_ref: Option<String>,
    ) -> Result<Session> {
        let relationship = relationship.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        if parent_session_id.is_some() && relationship.is_none() {
            anyhow::bail!("parent_session_id requires relationship");
        }
        if parent_session_id.is_none() && relationship.is_some() {
            anyhow::bail!("relationship requires parent_session_id");
        }
        let now = Utc::now();
        let title = if relationship.as_deref() == Some("sub_agent") {
            format!("subagent-{}", session_id.0)
        } else {
            "New Task".to_string()
        };
        let session = Session {
            id: session_id,
            task_id,
            workspace_id,
            worktree_id,
            execution_environment,
            parent_session_id,
            relationship,
            provider_id,
            model_id,
            reasoning_effort,
            title,
            agent_role,
            status: SessionStatus::Active,
            provider_session_ref,
            created_at: now,
            updated_at: now,
        };
        let result = self.query(
            r#"INSERT INTO sessions (id, task_id, workspace_id, worktree_id, parent_session_id, relationship,
               execution_environment, provider_id, model_id, reasoning_effort, title, agent_role, status, provider_session_ref, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(id) DO NOTHING"#,
        )
        .bind(session.id.0.to_string())
        .bind(session.task_id.0.to_string())
        .bind(session.workspace_id.0.to_string())
        .bind(session.worktree_id.0.to_string())
        .bind(session.parent_session_id.map(|id| id.0.to_string()))
        .bind(&session.relationship)
        .bind(execution_environment_to_str(session.execution_environment))
        .bind(&session.provider_id)
        .bind(&session.model_id)
        .bind(&session.reasoning_effort)
        .bind(&session.title)
        .bind(&session.agent_role)
        .bind(session_status_to_str(&session.status))
        .bind(&session.provider_session_ref)
        .bind(session.created_at.to_rfc3339())
        .bind(session.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return self
                .get_session(session_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("session exists but could not be loaded"));
        }

        self.ensure_session_snapshot_summary(session.id).await?;
        self.schedule_active_snapshot_head_refresh(session.id, None)
            .await?;
        Ok(session)
    }

    pub async fn get_session(&self, id: SessionId) -> Result<Option<Session>> {
        let row = self.query(
            r#"SELECT id, task_id, workspace_id, worktree_id, parent_session_id, relationship,
               execution_environment, provider_id, model_id, reasoning_effort, agent_role, title, status, provider_session_ref, created_at, updated_at
               FROM sessions WHERE id = ?"#,
        )
        .bind(id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;

        row.map(|r| decode_session_row(&r)).transpose()
    }

    pub async fn update_session_model(&self, id: SessionId, model_id: String) -> Result<()> {
        self.update_session_model_config(id, model_id, None).await
    }

    pub async fn update_session_model_config(
        &self,
        id: SessionId,
        model_id: String,
        reasoning_effort: Option<String>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.query(
            r#"UPDATE sessions
               SET model_id = ?, reasoning_effort = ?, updated_at = ?
               WHERE id = ?"#,
        )
        .bind(model_id)
        .bind(reasoning_effort)
        .bind(now)
        .bind(id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_session_execution_environment(
        &self,
        id: SessionId,
        execution_environment: ExecutionEnvironment,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.query(
            r#"UPDATE sessions
               SET execution_environment = ?, updated_at = ?
               WHERE id = ?"#,
        )
        .bind(execution_environment_to_str(execution_environment))
        .bind(now)
        .bind(id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_session_title(&self, id: SessionId, title: String) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let res = self
            .query(
                r#"UPDATE sessions
               SET title = ?, updated_at = ?
               WHERE id = ?"#,
            )
            .bind(title)
            .bind(now)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn archive_subagent_session(
        &self,
        parent_session_id: SessionId,
        session_id: SessionId,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let res = self
            .query(
                r#"UPDATE sessions
                   SET archived_at = ?, updated_at = ?
                   WHERE id = ?
                     AND parent_session_id = ?
                     AND relationship = 'sub_agent'
                     AND archived_at IS NULL"#,
            )
            .bind(&now)
            .bind(&now)
            .bind(session_id.0.to_string())
            .bind(parent_session_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn claim_session_provider_session_ref(
        &self,
        id: SessionId,
        provider_session_ref: String,
        source: &str,
    ) -> Result<()> {
        let provider_session_ref = provider_session_ref.trim().to_string();
        if provider_session_ref.is_empty() {
            anyhow::bail!("provider session ref claim requires a non-empty ref");
        }
        let source = source.trim();
        if source.is_empty() {
            anyhow::bail!("provider session ref claim requires a non-empty source");
        }
        let now = Utc::now().to_rfc3339();
        let session_id = id.0.to_string();

        let session = sqlx::query(
            r#"SELECT id, provider_id, provider_session_ref, workspace_id, task_id, worktree_id
               FROM sessions
               WHERE id = ?"#,
        )
        .bind(&session_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("session not found for provider ref claim: {}", id.0))?;

        let provider_id: String = session.try_get("provider_id")?;
        let current_ref: Option<String> = session.try_get("provider_session_ref")?;
        if let Some(current_ref) = current_ref.as_deref().map(str::trim) {
            if !current_ref.is_empty() && current_ref != provider_session_ref {
                anyhow::bail!(
                    "provider session ref substitution rejected for session {}: existing ref `{}` differs from returned ref `{}`",
                    id.0,
                    current_ref,
                    provider_session_ref
                );
            }
        }

        let local_duplicate: Option<String> = sqlx::query_scalar(
            r#"SELECT id
               FROM sessions
               WHERE provider_id = ?
                 AND provider_session_ref = ?
                 AND id <> ?
               LIMIT 1"#,
        )
        .bind(&provider_id)
        .bind(&provider_session_ref)
        .bind(&session_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(owner) = local_duplicate {
            anyhow::bail!(
                "provider session ref `{}` for provider `{}` is already attached to session {}; refusing to attach it to session {}",
                provider_session_ref,
                provider_id,
                owner,
                id.0
            );
        }

        let workspace_id: String = session.try_get("workspace_id")?;
        let task_id: String = session.try_get("task_id")?;
        let worktree_id: String = session.try_get("worktree_id")?;
        sqlx::query(
            r#"INSERT OR IGNORE INTO provider_session_bindings (
                provider_id,
                provider_account_scope,
                provider_session_ref,
                session_id,
                workspace_id,
                task_id,
                worktree_id,
                source,
                created_at,
                updated_at
               )
               VALUES (?, 'default', ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&provider_id)
        .bind(&provider_session_ref)
        .bind(&session_id)
        .bind(workspace_id)
        .bind(task_id)
        .bind(worktree_id)
        .bind(source)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        let binding_owner: Option<String> = sqlx::query_scalar(
            r#"SELECT session_id
               FROM provider_session_bindings
               WHERE provider_id = ?
                 AND provider_account_scope = 'default'
                 AND provider_session_ref = ?
               LIMIT 1"#,
        )
        .bind(&provider_id)
        .bind(&provider_session_ref)
        .fetch_optional(&self.pool)
        .await?;
        if binding_owner.as_deref() != Some(session_id.as_str()) {
            let owner = binding_owner.unwrap_or_else(|| "<missing>".to_string());
            anyhow::bail!(
                "provider session ref `{}` for provider `{}` is owned by session {}; refusing to attach it to session {}",
                provider_session_ref,
                provider_id,
                owner,
                id.0
            );
        }

        let updated = sqlx::query(
            r#"UPDATE sessions
               SET provider_session_ref = ?, updated_at = ?
               WHERE id = ?
                 AND (
                   provider_session_ref IS NULL
                   OR trim(provider_session_ref) = ''
                   OR provider_session_ref = ?
                 )"#,
        )
        .bind(&provider_session_ref)
        .bind(&now)
        .bind(&session_id)
        .bind(&provider_session_ref)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() == 0 {
            if let Err(cleanup_err) = sqlx::query(
                r#"DELETE FROM provider_session_bindings
                   WHERE provider_id = ?
                     AND provider_account_scope = 'default'
                     AND provider_session_ref = ?
                     AND session_id = ?"#,
            )
            .bind(&provider_id)
            .bind(&provider_session_ref)
            .bind(&session_id)
            .execute(&self.pool)
            .await
            {
                anyhow::bail!(
                    "provider session ref substitution rejected for session {} and binding cleanup failed: {cleanup_err:#}",
                    id.0
                );
            }
            anyhow::bail!(
                "provider session ref substitution rejected for session {}: existing ref changed before returned ref `{}` could be attached",
                id.0,
                provider_session_ref
            );
        }
        Ok(())
    }
}

impl Store {
    pub async fn list_all_sessions_for_task(&self, task_id: TaskId) -> Result<Vec<Session>> {
        let rows = self.query(
            r#"SELECT id, task_id, workspace_id, worktree_id, parent_session_id, relationship,
               execution_environment, provider_id, model_id, reasoning_effort, agent_role, title, status, provider_session_ref, created_at, updated_at
               FROM sessions
               WHERE task_id = ?
               ORDER BY created_at ASC"#,
        )
        .bind(task_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(decode_session_row(&r)?);
        }
        Ok(out)
    }

    pub async fn list_sessions_for_task(&self, task_id: TaskId) -> Result<Vec<Session>> {
        let rows = self.query(
            r#"SELECT id, task_id, workspace_id, worktree_id, parent_session_id, relationship,
               execution_environment, provider_id, model_id, reasoning_effort, agent_role, title, status, provider_session_ref, created_at, updated_at
               FROM sessions
               WHERE task_id = ?
                 AND (relationship != 'sub_agent' OR relationship IS NULL OR archived_at IS NULL)
               ORDER BY created_at ASC"#,
        )
        .bind(task_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(decode_session_row(&r)?);
        }
        Ok(out)
    }

    pub async fn list_active_sessions_for_task(&self, task_id: TaskId) -> Result<Vec<Session>> {
        self.list_sessions_for_task(task_id).await
    }

    pub async fn list_all_sessions_for_worktree(
        &self,
        worktree_id: WorktreeId,
    ) -> Result<Vec<Session>> {
        let rows = self.query(
            r#"SELECT id, task_id, workspace_id, worktree_id, parent_session_id, relationship,
               execution_environment, provider_id, model_id, reasoning_effort, agent_role, title, status, provider_session_ref, created_at, updated_at
               FROM sessions
               WHERE worktree_id = ?
               ORDER BY created_at ASC"#,
        )
        .bind(worktree_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(decode_session_row(&r)?);
        }
        Ok(out)
    }

    pub async fn list_sessions_for_worktree(
        &self,
        worktree_id: WorktreeId,
    ) -> Result<Vec<Session>> {
        let rows = self.query(
            r#"SELECT id, task_id, workspace_id, worktree_id, parent_session_id, relationship,
               execution_environment, provider_id, model_id, reasoning_effort, agent_role, title, status, provider_session_ref, created_at, updated_at
               FROM sessions
               WHERE worktree_id = ?
                 AND (relationship != 'sub_agent' OR relationship IS NULL OR archived_at IS NULL)
               ORDER BY created_at ASC"#,
        )
        .bind(worktree_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(decode_session_row(&r)?);
        }
        Ok(out)
    }

    pub async fn is_archived_subagent_session(&self, session_id: SessionId) -> Result<bool> {
        let archived = self
            .query_scalar::<i64>(
                r#"SELECT 1
                   FROM sessions
                   WHERE id = ? AND relationship = 'sub_agent' AND archived_at IS NOT NULL"#,
            )
            .bind(session_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        Ok(archived.is_some())
    }

    pub async fn list_subagent_sessions(
        &self,
        parent_session_id: SessionId,
    ) -> Result<Vec<SessionSummary>> {
        let rows = self
            .query(
                r#"SELECT id, task_id, workspace_id, parent_session_id, relationship,
               execution_environment, provider_id, model_id, reasoning_effort, title, status, created_at, updated_at
               FROM sessions
               WHERE parent_session_id = ? AND relationship = 'sub_agent' AND archived_at IS NULL
               ORDER BY created_at ASC"#,
            )
            .bind(parent_session_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.try_get("id")?;
            let task_id: String = r.try_get("task_id")?;
            let ws_id: String = r.try_get("workspace_id")?;
            let created_at: String = r.try_get("created_at")?;
            let updated_at: String = r.try_get("updated_at")?;
            out.push(SessionSummary {
                id: SessionId(uuid::Uuid::parse_str(&id)?),
                task_id: TaskId(uuid::Uuid::parse_str(&task_id)?),
                workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id)?),
                execution_environment: parse_execution_environment(
                    r.try_get::<String, _>("execution_environment")?.as_str(),
                )?,
                parent_session_id: r
                    .try_get::<Option<String>, _>("parent_session_id")?
                    .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                    .map(SessionId),
                relationship: r.try_get("relationship")?,
                provider_id: r.try_get("provider_id")?,
                model_id: r.try_get("model_id")?,
                reasoning_effort: r.try_get("reasoning_effort")?,
                title: r.try_get("title")?,
                status: parse_session_status(r.try_get::<String, _>("status")?.as_str()),
                created_at: parse_dt(&created_at)?,
                updated_at: parse_dt(&updated_at)?,
            });
        }
        Ok(out)
    }

    pub async fn get_subagent_session_by_label(
        &self,
        parent_session_id: SessionId,
        label: &str,
    ) -> Result<Option<Session>> {
        let row = self
            .query(
                r#"SELECT id, task_id, workspace_id, worktree_id, parent_session_id, relationship,
               execution_environment, provider_id, model_id, reasoning_effort, agent_role, title, status, provider_session_ref, created_at, updated_at
               FROM sessions
               WHERE parent_session_id = ? AND relationship = 'sub_agent' AND archived_at IS NULL AND title = ?
               LIMIT 1"#,
            )
            .bind(parent_session_id.0.to_string())
            .bind(label)
            .fetch_optional(&self.pool)
            .await?;

        row.map(|r| decode_session_row(&r)).transpose()
    }

    pub async fn get_active_subagent_session(
        &self,
        parent_session_id: SessionId,
        session_id: SessionId,
    ) -> Result<Option<Session>> {
        let row = self
            .query(
                r#"SELECT id, task_id, workspace_id, worktree_id, parent_session_id, relationship,
               execution_environment, provider_id, model_id, reasoning_effort, agent_role, title, status, provider_session_ref, created_at, updated_at
               FROM sessions
               WHERE id = ? AND parent_session_id = ? AND relationship = 'sub_agent' AND archived_at IS NULL
               LIMIT 1"#,
            )
            .bind(session_id.0.to_string())
            .bind(parent_session_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(|r| decode_session_row(&r)).transpose()
    }

    pub async fn count_active_subagent_sessions(&self, parent_session_id: SessionId) -> Result<usize> {
        let count: i64 = self
            .query_scalar(
                r#"SELECT COUNT(*)
                   FROM sessions
                   WHERE parent_session_id = ? AND relationship = 'sub_agent' AND archived_at IS NULL"#,
            )
            .bind(parent_session_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(count as usize)
    }

    pub async fn subagent_label_exists(&self, task_id: TaskId, label: &str) -> Result<bool> {
        let row = self
            .query(
                r#"SELECT 1
               FROM sessions
               WHERE task_id = ? AND relationship = 'sub_agent' AND archived_at IS NULL AND title = ?
               LIMIT 1"#,
            )
            .bind(task_id.0.to_string())
            .bind(label)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn get_session_summary_checkpoint(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionSummaryCheckpoint>> {
        let row = self.query(
            r#"SELECT session_id, checkpoint_id, summary, last_turn_id, last_event_seq, created_at, updated_at
               FROM session_summary_checkpoints
               WHERE session_id = ?"#,
        )
        .bind(session_id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;

        let row = match row {
            Some(row) => row,
            None => return Ok(None),
        };
        let session_id: String = row.try_get("session_id")?;
        let last_turn_id: Option<String> = row.try_get("last_turn_id")?;
        let created_at: String = row.try_get("created_at")?;
        let updated_at: String = row.try_get("updated_at")?;

        Ok(Some(SessionSummaryCheckpoint {
            session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
            checkpoint_id: row.try_get("checkpoint_id")?,
            summary: row.try_get("summary")?,
            last_turn_id: last_turn_id
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(TurnId),
            last_event_seq: row.try_get("last_event_seq")?,
            created_at: parse_dt(&created_at)?,
            updated_at: parse_dt(&updated_at)?,
        }))
    }

    pub async fn upsert_session_summary_checkpoint(
        &self,
        checkpoint: SessionSummaryCheckpoint,
    ) -> Result<SessionSummaryCheckpoint> {
        {
            let _write_guard = self.write_gate.lock().await;
            let mut tx = self.pool.begin().await?;
            let session_id = checkpoint.session_id.0.to_string();
            let checkpoint_updated_at = checkpoint.updated_at.to_rfc3339();
            let projection_updated_at = Utc::now().to_rfc3339();

            sqlx::query(
                r#"INSERT INTO session_summary_checkpoints (
                       session_id, checkpoint_id, summary, last_turn_id, last_event_seq, created_at, updated_at
                   )
                   VALUES (?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(session_id) DO UPDATE SET
                       checkpoint_id = excluded.checkpoint_id,
                       summary = excluded.summary,
                       last_turn_id = excluded.last_turn_id,
                       last_event_seq = excluded.last_event_seq,
                       updated_at = excluded.updated_at"#,
            )
            .bind(&session_id)
            .bind(&checkpoint.checkpoint_id)
            .bind(&checkpoint.summary)
            .bind(checkpoint.last_turn_id.map(|id| id.0.to_string()))
            .bind(checkpoint.last_event_seq)
            .bind(checkpoint.created_at.to_rfc3339())
            .bind(&checkpoint_updated_at)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                r#"INSERT INTO session_snapshot_summaries (
                       session_id, running_turn_count, created_at, updated_at
                   )
                   VALUES (?, 0, ?, ?)
                   ON CONFLICT(session_id) DO NOTHING"#,
            )
            .bind(&session_id)
            .bind(&projection_updated_at)
            .bind(&projection_updated_at)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                r#"UPDATE session_snapshot_summaries
                   SET projection_rev = projection_rev + 1,
                       updated_at = ?
                   WHERE session_id = ?"#,
            )
            .bind(&projection_updated_at)
            .bind(&session_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }

        self.schedule_active_snapshot_head_refresh(checkpoint.session_id, None)
            .await?;
        Ok(checkpoint)
    }
}

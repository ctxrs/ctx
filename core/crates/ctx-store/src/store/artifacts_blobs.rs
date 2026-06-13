use super::*;

impl Store {
    // Blob APIs
    pub async fn insert_blob(
        &self,
        id: &str,
        sha256: &str,
        bytes: i64,
        mime_type: &str,
        name: Option<&str>,
        created_at: DateTime<Utc>,
    ) -> Result<()> {
        self.query(
            r#"INSERT INTO blobs (id, sha256, bytes, mime_type, name, created_at)
               VALUES (?, ?, ?, ?, ?, ?)"#,
        )
        .bind(id)
        .bind(sha256)
        .bind(bytes)
        .bind(mime_type)
        .bind(name)
        .bind(created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_blob(
        &self,
        id: &str,
    ) -> Result<Option<(String, String, i64, Option<String>, DateTime<Utc>)>> {
        let row = self
            .query(
                r#"SELECT sha256, mime_type, bytes, name, created_at
               FROM blobs WHERE id = ?"#,
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|r| {
            let sha256: String = r.try_get("sha256").unwrap_or_default();
            let mime_type: String = r.try_get("mime_type").unwrap_or_default();
            let bytes: i64 = r.try_get("bytes").unwrap_or_default();
            let name: Option<String> = r.try_get("name").ok();
            let created_at: String = r.try_get("created_at").unwrap_or_default();
            let created_at = parse_dt(&created_at).unwrap_or_else(|_| Utc::now());
            (sha256, mime_type, bytes, name, created_at)
        }))
    }

    // Artifact APIs
    pub async fn list_session_artifacts(&self, session_id: SessionId) -> Result<Vec<Artifact>> {
        let rows = self
            .query(
                r#"SELECT id, session_id, task_id, workspace_id, worktree_id,
                      name, absolute_path, mime_type, bytes, created_at
               FROM artifacts
               WHERE session_id = ?
               ORDER BY position ASC"#,
            )
            .bind(session_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            if let Ok(artifact) = build_artifact_from_row(r) {
                out.push(artifact);
            }
        }
        Ok(out)
    }

    pub async fn upsert_session_git_status_summary(
        &self,
        session_id: SessionId,
        worktree_id: WorktreeId,
        summary: &SessionGitStatusSummary,
    ) -> Result<()> {
        let summary_json =
            serde_json::to_string(summary).context("serializing git status summary")?;
        let now = Utc::now().to_rfc3339();
        self.query(
            r#"INSERT INTO session_git_status_snapshots (
                    session_id, worktree_id, summary_json, created_at, updated_at
               )
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(session_id) DO UPDATE SET
                   worktree_id = excluded.worktree_id,
                   summary_json = excluded.summary_json,
                   updated_at = excluded.updated_at"#,
        )
        .bind(session_id.0.to_string())
        .bind(worktree_id.0.to_string())
        .bind(summary_json)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_session_git_status_summary(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionGitStatusSummary>> {
        let row = self
            .query(
                r#"SELECT summary_json
               FROM session_git_status_snapshots
               WHERE session_id = ?"#,
            )
            .bind(session_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let summary_json: String = row.try_get("summary_json")?;
        Ok(serde_json::from_str(&summary_json).ok())
    }

    pub async fn get_session_state(&self, session_id: SessionId) -> Result<SessionState> {
        let artifacts = self.list_session_artifacts(session_id).await?;
        let git_status = self.get_session_git_status_summary(session_id).await?;
        Ok(SessionState {
            artifacts,
            git_status,
        })
    }

    pub async fn get_artifact(&self, id: ArtifactId) -> Result<Option<Artifact>> {
        let row = self
            .query(
                r#"SELECT id, session_id, task_id, workspace_id, worktree_id,
                      name, absolute_path, mime_type, bytes, created_at
               FROM artifacts
               WHERE id = ?"#,
            )
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.and_then(|r| build_artifact_from_row(r).ok()))
    }

    pub async fn upsert_session_artifact_by_path(&self, artifact: &Artifact) -> Result<Artifact> {
        let existing = self
            .query(
                r#"SELECT id, session_id, task_id, workspace_id, worktree_id,
                          name, absolute_path, mime_type, bytes, created_at
                   FROM artifacts
                   WHERE session_id = ? AND absolute_path = ?"#,
            )
            .bind(artifact.session_id.0.to_string())
            .bind(&artifact.absolute_path)
            .fetch_optional(&self.pool)
            .await?;

        if let Some(row) = existing {
            let existing_artifact = build_artifact_from_row(row)?;
            self.query(
                r#"UPDATE artifacts
                   SET name = ?, mime_type = ?, bytes = ?
                   WHERE id = ?"#,
            )
            .bind(artifact.name.as_deref())
            .bind(&artifact.mime_type)
            .bind(artifact.bytes)
            .bind(existing_artifact.id.0.to_string())
            .execute(&self.pool)
            .await?;

            return Ok(Artifact {
                id: existing_artifact.id,
                session_id: existing_artifact.session_id,
                task_id: existing_artifact.task_id,
                workspace_id: existing_artifact.workspace_id,
                worktree_id: existing_artifact.worktree_id,
                name: artifact.name.clone(),
                absolute_path: artifact.absolute_path.clone(),
                mime_type: artifact.mime_type.clone(),
                bytes: artifact.bytes,
                created_at: existing_artifact.created_at,
                missing: None,
            });
        }

        let position: i64 = self
            .query(
                r#"SELECT COALESCE(MAX(position) + 1, 0) AS position
                   FROM artifacts
                   WHERE session_id = ?"#,
            )
            .bind(artifact.session_id.0.to_string())
            .fetch_one(&self.pool)
            .await?
            .try_get("position")?;

        self.query(
            r#"INSERT INTO artifacts (
                    id, session_id, task_id, workspace_id, worktree_id,
                    position, name, absolute_path, mime_type, bytes, created_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(artifact.id.0.to_string())
        .bind(artifact.session_id.0.to_string())
        .bind(artifact.task_id.0.to_string())
        .bind(artifact.workspace_id.0.to_string())
        .bind(artifact.worktree_id.0.to_string())
        .bind(position)
        .bind(artifact.name.as_deref())
        .bind(&artifact.absolute_path)
        .bind(&artifact.mime_type)
        .bind(artifact.bytes)
        .bind(artifact.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(artifact.clone())
    }

    pub async fn replace_session_artifacts(
        &self,
        session_id: SessionId,
        artifacts: &[Artifact],
    ) -> Result<()> {
        {
            let _write_guard = self.write_gate.lock().await;
            let mut tx = self.pool.begin().await?;
            self.query(r#"DELETE FROM artifacts WHERE session_id = ?"#)
                .bind(session_id.0.to_string())
                .execute(&mut *tx)
                .await?;

            for (idx, artifact) in artifacts.iter().enumerate() {
                self.query(
                    r#"INSERT INTO artifacts (
                            id, session_id, task_id, workspace_id, worktree_id,
                            position, name, absolute_path, mime_type, bytes, created_at
                       )
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
                )
                .bind(artifact.id.0.to_string())
                .bind(artifact.session_id.0.to_string())
                .bind(artifact.task_id.0.to_string())
                .bind(artifact.workspace_id.0.to_string())
                .bind(artifact.worktree_id.0.to_string())
                .bind(idx as i64)
                .bind(artifact.name.as_deref())
                .bind(&artifact.absolute_path)
                .bind(&artifact.mime_type)
                .bind(artifact.bytes)
                .bind(artifact.created_at.to_rfc3339())
                .execute(&mut *tx)
                .await?;
            }

            tx.commit().await?;
        }
        Ok(())
    }
}

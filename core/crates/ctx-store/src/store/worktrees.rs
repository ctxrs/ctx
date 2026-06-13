use super::*;

pub struct WorktreeBootstrapResultUpdate {
    pub worktree_id: WorktreeId,
    pub status: WorktreeBootstrapStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub exit_code: Option<i64>,
    pub timeout_sec: Option<i64>,
    pub error: Option<String>,
    pub log_path: Option<String>,
    pub log_truncated: Option<bool>,
    pub command: Option<String>,
    pub script_path: Option<String>,
}

fn serialize_bootstrap_status(status: &WorktreeBootstrapStatus) -> &'static str {
    match status {
        WorktreeBootstrapStatus::Success => "success",
        WorktreeBootstrapStatus::Failed => "failed",
        WorktreeBootstrapStatus::Timeout => "timeout",
    }
}

fn parse_bootstrap_status(raw: Option<String>) -> Option<WorktreeBootstrapStatus> {
    match raw.as_deref() {
        Some("success") => Some(WorktreeBootstrapStatus::Success),
        Some("failed") => Some(WorktreeBootstrapStatus::Failed),
        Some("timeout") => Some(WorktreeBootstrapStatus::Timeout),
        _ => None,
    }
}

impl Store {
    // Worktree APIs
    pub async fn insert_worktree(&self, worktree: Worktree) -> Result<Worktree> {
        let mut worktree = worktree;
        if worktree.vcs_kind.is_none() {
            worktree.vcs_kind = Some(VcsKind::Git);
        }
        if worktree.base_revision.is_none() {
            worktree.base_revision = Some(worktree.base_commit_sha.clone());
        }
        if worktree.vcs_ref.is_none() {
            worktree.vcs_ref = worktree.git_branch.clone();
        }
        let vcs_kind = worktree.vcs_kind.as_ref().map(vcs_kind_to_str);
        self.query(
            r#"INSERT INTO worktrees (id, workspace_id, root_path, base_commit_sha, git_branch, vcs_kind, base_revision, vcs_ref, created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(worktree.id.0.to_string())
        .bind(worktree.workspace_id.0.to_string())
        .bind(&worktree.root_path)
        .bind(&worktree.base_commit_sha)
        .bind(&worktree.git_branch)
        .bind(vcs_kind)
        .bind(worktree.base_revision.as_deref())
        .bind(worktree.vcs_ref.as_deref())
        .bind(worktree.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(worktree)
    }

    pub async fn create_worktree(
        &self,
        workspace_id: WorkspaceId,
        root_path: String,
        base_commit_sha: String,
        git_branch: Option<String>,
    ) -> Result<Worktree> {
        let worktree = Worktree {
            id: WorktreeId::new(),
            workspace_id,
            root_path,
            base_commit_sha,
            git_branch,
            vcs_kind: None,
            base_revision: None,
            vcs_ref: None,
            created_at: Utc::now(),
            bootstrap_status: None,
            bootstrap_started_at: None,
            bootstrap_finished_at: None,
            bootstrap_exit_code: None,
            bootstrap_timeout_sec: None,
            bootstrap_error: None,
            bootstrap_log_path: None,
            bootstrap_log_truncated: None,
            bootstrap_command: None,
            bootstrap_script_path: None,
        };
        self.insert_worktree(worktree).await
    }

    pub async fn get_worktree(&self, id: WorktreeId) -> Result<Option<Worktree>> {
        let row = self.query(
            r#"SELECT id, workspace_id, root_path, base_commit_sha, git_branch, vcs_kind, base_revision, vcs_ref, created_at,
                      bootstrap_status, bootstrap_started_at, bootstrap_finished_at, bootstrap_exit_code,
                      bootstrap_timeout_sec, bootstrap_error, bootstrap_log_path, bootstrap_log_truncated,
                      bootstrap_command, bootstrap_script_path
               FROM worktrees WHERE id = ?"#,
        )
        .bind(id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| {
            let id: String = r.try_get("id").ok()?;
            let ws_id: String = r.try_get("workspace_id").ok()?;
            let created_at: String = r.try_get("created_at").ok()?;
            let bootstrap_status: Option<String> = r.try_get("bootstrap_status").ok()?;
            let bootstrap_started_at: Option<String> = r.try_get("bootstrap_started_at").ok()?;
            let bootstrap_finished_at: Option<String> = r.try_get("bootstrap_finished_at").ok()?;
            let bootstrap_exit_code: Option<i64> = r.try_get("bootstrap_exit_code").ok()?;
            let bootstrap_timeout_sec: Option<i64> = r.try_get("bootstrap_timeout_sec").ok()?;
            let bootstrap_error: Option<String> = r.try_get("bootstrap_error").ok()?;
            let bootstrap_log_path: Option<String> = r.try_get("bootstrap_log_path").ok()?;
            let bootstrap_log_truncated: Option<i64> = r.try_get("bootstrap_log_truncated").ok()?;
            let bootstrap_command: Option<String> = r.try_get("bootstrap_command").ok()?;
            let bootstrap_script_path: Option<String> = r.try_get("bootstrap_script_path").ok()?;
            let vcs_kind: Option<String> = r.try_get("vcs_kind").ok()?;
            let base_revision: Option<String> = r.try_get("base_revision").ok()?;
            let vcs_ref: Option<String> = r.try_get("vcs_ref").ok()?;
            Some(Worktree {
                id: WorktreeId(uuid::Uuid::parse_str(&id).ok()?),
                workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id).ok()?),
                root_path: r.try_get("root_path").ok()?,
                base_commit_sha: r.try_get("base_commit_sha").ok()?,
                git_branch: r.try_get("git_branch").ok()?,
                vcs_kind: parse_vcs_kind(vcs_kind),
                base_revision,
                vcs_ref,
                created_at: parse_dt(&created_at).ok()?,
                bootstrap_status: parse_bootstrap_status(bootstrap_status),
                bootstrap_started_at: bootstrap_started_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()
                    .ok()?,
                bootstrap_finished_at: bootstrap_finished_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()
                    .ok()?,
                bootstrap_exit_code,
                bootstrap_timeout_sec,
                bootstrap_error,
                bootstrap_log_path,
                bootstrap_log_truncated: bootstrap_log_truncated.map(|v| v != 0),
                bootstrap_command,
                bootstrap_script_path,
            })
        }))
    }

    pub async fn get_local_worktree_for_root(
        &self,
        workspace_id: WorkspaceId,
        root_path: &str,
    ) -> Result<Option<Worktree>> {
        let row = self.query(
            r#"SELECT id, workspace_id, root_path, base_commit_sha, git_branch, vcs_kind, base_revision, vcs_ref, created_at,
                      bootstrap_status, bootstrap_started_at, bootstrap_finished_at, bootstrap_exit_code,
                      bootstrap_timeout_sec, bootstrap_error, bootstrap_log_path, bootstrap_log_truncated,
                      bootstrap_command, bootstrap_script_path
               FROM worktrees
               WHERE workspace_id = ? AND root_path = ? AND git_branch IS NULL
               ORDER BY created_at DESC
               LIMIT 1"#,
        )
        .bind(workspace_id.0.to_string())
        .bind(root_path)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| {
            let id: String = r.try_get("id").ok()?;
            let ws_id: String = r.try_get("workspace_id").ok()?;
            let created_at: String = r.try_get("created_at").ok()?;
            let bootstrap_status: Option<String> = r.try_get("bootstrap_status").ok()?;
            let bootstrap_started_at: Option<String> = r.try_get("bootstrap_started_at").ok()?;
            let bootstrap_finished_at: Option<String> = r.try_get("bootstrap_finished_at").ok()?;
            let bootstrap_exit_code: Option<i64> = r.try_get("bootstrap_exit_code").ok()?;
            let bootstrap_timeout_sec: Option<i64> = r.try_get("bootstrap_timeout_sec").ok()?;
            let bootstrap_error: Option<String> = r.try_get("bootstrap_error").ok()?;
            let bootstrap_log_path: Option<String> = r.try_get("bootstrap_log_path").ok()?;
            let bootstrap_log_truncated: Option<i64> = r.try_get("bootstrap_log_truncated").ok()?;
            let bootstrap_command: Option<String> = r.try_get("bootstrap_command").ok()?;
            let bootstrap_script_path: Option<String> = r.try_get("bootstrap_script_path").ok()?;
            let vcs_kind: Option<String> = r.try_get("vcs_kind").ok()?;
            let base_revision: Option<String> = r.try_get("base_revision").ok()?;
            let vcs_ref: Option<String> = r.try_get("vcs_ref").ok()?;
            Some(Worktree {
                id: WorktreeId(uuid::Uuid::parse_str(&id).ok()?),
                workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id).ok()?),
                root_path: r.try_get("root_path").ok()?,
                base_commit_sha: r.try_get("base_commit_sha").ok()?,
                git_branch: r.try_get("git_branch").ok()?,
                vcs_kind: parse_vcs_kind(vcs_kind),
                base_revision,
                vcs_ref,
                created_at: parse_dt(&created_at).ok()?,
                bootstrap_status: parse_bootstrap_status(bootstrap_status),
                bootstrap_started_at: bootstrap_started_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()
                    .ok()?,
                bootstrap_finished_at: bootstrap_finished_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()
                    .ok()?,
                bootstrap_exit_code,
                bootstrap_timeout_sec,
                bootstrap_error,
                bootstrap_log_path,
                bootstrap_log_truncated: bootstrap_log_truncated.map(|v| v != 0),
                bootstrap_command,
                bootstrap_script_path,
            })
        }))
    }

    pub async fn get_worktree_for_root(
        &self,
        workspace_id: WorkspaceId,
        root_path: &str,
    ) -> Result<Option<Worktree>> {
        let row = self.query(
            r#"SELECT id, workspace_id, root_path, base_commit_sha, git_branch, vcs_kind, base_revision, vcs_ref, created_at,
                      bootstrap_status, bootstrap_started_at, bootstrap_finished_at, bootstrap_exit_code,
                      bootstrap_timeout_sec, bootstrap_error, bootstrap_log_path, bootstrap_log_truncated,
                      bootstrap_command, bootstrap_script_path
               FROM worktrees
               WHERE workspace_id = ? AND root_path = ?
               ORDER BY created_at DESC
               LIMIT 1"#,
        )
        .bind(workspace_id.0.to_string())
        .bind(root_path)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| {
            let id: String = r.try_get("id").ok()?;
            let ws_id: String = r.try_get("workspace_id").ok()?;
            let created_at: String = r.try_get("created_at").ok()?;
            let bootstrap_status: Option<String> = r.try_get("bootstrap_status").ok()?;
            let bootstrap_started_at: Option<String> = r.try_get("bootstrap_started_at").ok()?;
            let bootstrap_finished_at: Option<String> = r.try_get("bootstrap_finished_at").ok()?;
            let bootstrap_exit_code: Option<i64> = r.try_get("bootstrap_exit_code").ok()?;
            let bootstrap_timeout_sec: Option<i64> = r.try_get("bootstrap_timeout_sec").ok()?;
            let bootstrap_error: Option<String> = r.try_get("bootstrap_error").ok()?;
            let bootstrap_log_path: Option<String> = r.try_get("bootstrap_log_path").ok()?;
            let bootstrap_log_truncated: Option<i64> = r.try_get("bootstrap_log_truncated").ok()?;
            let bootstrap_command: Option<String> = r.try_get("bootstrap_command").ok()?;
            let bootstrap_script_path: Option<String> = r.try_get("bootstrap_script_path").ok()?;
            let vcs_kind: Option<String> = r.try_get("vcs_kind").ok()?;
            let base_revision: Option<String> = r.try_get("base_revision").ok()?;
            let vcs_ref: Option<String> = r.try_get("vcs_ref").ok()?;
            Some(Worktree {
                id: WorktreeId(uuid::Uuid::parse_str(&id).ok()?),
                workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id).ok()?),
                root_path: r.try_get("root_path").ok()?,
                base_commit_sha: r.try_get("base_commit_sha").ok()?,
                git_branch: r.try_get("git_branch").ok()?,
                vcs_kind: parse_vcs_kind(vcs_kind),
                base_revision,
                vcs_ref,
                created_at: parse_dt(&created_at).ok()?,
                bootstrap_status: parse_bootstrap_status(bootstrap_status),
                bootstrap_started_at: bootstrap_started_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()
                    .ok()?,
                bootstrap_finished_at: bootstrap_finished_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()
                    .ok()?,
                bootstrap_exit_code,
                bootstrap_timeout_sec,
                bootstrap_error,
                bootstrap_log_path,
                bootstrap_log_truncated: bootstrap_log_truncated.map(|v| v != 0),
                bootstrap_command,
                bootstrap_script_path,
            })
        }))
    }

    pub async fn list_worktrees(&self, workspace_id: WorkspaceId) -> Result<Vec<Worktree>> {
        let rows = self.query(
            r#"SELECT id, workspace_id, root_path, base_commit_sha, git_branch, vcs_kind, base_revision, vcs_ref, created_at,
                      bootstrap_status, bootstrap_started_at, bootstrap_finished_at, bootstrap_exit_code,
                      bootstrap_timeout_sec, bootstrap_error, bootstrap_log_path, bootstrap_log_truncated,
                      bootstrap_command, bootstrap_script_path
               FROM worktrees WHERE workspace_id = ? ORDER BY created_at ASC"#,
        )
        .bind(workspace_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.try_get("id")?;
            let ws_id: String = r.try_get("workspace_id")?;
            let created_at: String = r.try_get("created_at")?;
            let bootstrap_status: Option<String> = r.try_get("bootstrap_status")?;
            let bootstrap_started_at: Option<String> = r.try_get("bootstrap_started_at")?;
            let bootstrap_finished_at: Option<String> = r.try_get("bootstrap_finished_at")?;
            let bootstrap_exit_code: Option<i64> = r.try_get("bootstrap_exit_code")?;
            let bootstrap_timeout_sec: Option<i64> = r.try_get("bootstrap_timeout_sec")?;
            let bootstrap_error: Option<String> = r.try_get("bootstrap_error")?;
            let bootstrap_log_path: Option<String> = r.try_get("bootstrap_log_path")?;
            let bootstrap_log_truncated: Option<i64> = r.try_get("bootstrap_log_truncated")?;
            let bootstrap_command: Option<String> = r.try_get("bootstrap_command")?;
            let bootstrap_script_path: Option<String> = r.try_get("bootstrap_script_path")?;
            let vcs_kind: Option<String> = r.try_get("vcs_kind")?;
            let base_revision: Option<String> = r.try_get("base_revision")?;
            let vcs_ref: Option<String> = r.try_get("vcs_ref")?;
            out.push(Worktree {
                id: WorktreeId(uuid::Uuid::parse_str(&id)?),
                workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id)?),
                root_path: r.try_get("root_path")?,
                base_commit_sha: r.try_get("base_commit_sha")?,
                git_branch: r.try_get("git_branch")?,
                vcs_kind: parse_vcs_kind(vcs_kind),
                base_revision,
                vcs_ref,
                created_at: parse_dt(&created_at)?,
                bootstrap_status: parse_bootstrap_status(bootstrap_status),
                bootstrap_started_at: bootstrap_started_at.as_deref().map(parse_dt).transpose()?,
                bootstrap_finished_at: bootstrap_finished_at
                    .as_deref()
                    .map(parse_dt)
                    .transpose()?,
                bootstrap_exit_code,
                bootstrap_timeout_sec,
                bootstrap_error,
                bootstrap_log_path,
                bootstrap_log_truncated: bootstrap_log_truncated.map(|v| v != 0),
                bootstrap_command,
                bootstrap_script_path,
            });
        }
        Ok(out)
    }

    pub async fn update_worktree_base_commit(
        &self,
        worktree_id: WorktreeId,
        base_commit_sha: &str,
    ) -> Result<bool> {
        let result = self
            .query(
                r#"UPDATE worktrees
               SET base_commit_sha = ?,
                   base_revision = ?
               WHERE id = ?"#,
            )
            .bind(base_commit_sha)
            .bind(base_commit_sha)
            .bind(worktree_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_worktree_root_path(
        &self,
        worktree_id: WorktreeId,
        root_path: &str,
    ) -> Result<bool> {
        let result = self
            .query(
                r#"UPDATE worktrees
               SET root_path = ?
               WHERE id = ?"#,
            )
            .bind(root_path)
            .bind(worktree_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_worktree(&self, worktree_id: WorktreeId) -> Result<bool> {
        let result = self
            .query(
                r#"DELETE FROM worktrees
               WHERE id = ?"#,
            )
            .bind(worktree_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_worktree_bootstrap_result(
        &self,
        update: WorktreeBootstrapResultUpdate,
    ) -> Result<()> {
        self.query(
            r#"UPDATE worktrees
               SET bootstrap_status = ?,
                   bootstrap_started_at = ?,
                   bootstrap_finished_at = ?,
                   bootstrap_exit_code = ?,
                   bootstrap_timeout_sec = ?,
                   bootstrap_error = ?,
                   bootstrap_log_path = ?,
                   bootstrap_log_truncated = ?,
                   bootstrap_command = ?,
                   bootstrap_script_path = ?
               WHERE id = ?"#,
        )
        .bind(serialize_bootstrap_status(&update.status))
        .bind(update.started_at.to_rfc3339())
        .bind(update.finished_at.to_rfc3339())
        .bind(update.exit_code)
        .bind(update.timeout_sec)
        .bind(update.error)
        .bind(update.log_path)
        .bind(update.log_truncated.map(|v| if v { 1 } else { 0 }))
        .bind(update.command)
        .bind(update.script_path)
        .bind(update.worktree_id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

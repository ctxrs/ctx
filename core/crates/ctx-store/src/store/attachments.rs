use super::*;

impl Store {
    // Attachment APIs
    pub async fn list_workspace_attachments(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<WorkspaceAttachment>> {
        let rows = self.query(
            r#"SELECT id, workspace_id, kind, name, source, revision, subpath, mount_relpath, mode,
                      update_policy, status, last_sync_at, error_message, created_at, updated_at
               FROM workspace_attachments
               WHERE workspace_id = ?
               ORDER BY created_at ASC"#,
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
            let kind: String = r.try_get("kind")?;
            let mode: String = r.try_get("mode")?;
            let update_policy: String = r.try_get("update_policy")?;
            let status: String = r.try_get("status")?;
            out.push(WorkspaceAttachment {
                id: WorkspaceAttachmentId(uuid::Uuid::parse_str(&id)?),
                workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id)?),
                kind: parse_attachment_kind(&kind),
                name: r.try_get("name")?,
                source: r.try_get("source")?,
                revision: r.try_get("revision")?,
                subpath: r.try_get("subpath")?,
                mount_relpath: r.try_get("mount_relpath")?,
                mode: parse_attachment_mode(&mode),
                update_policy: parse_attachment_update_policy(&update_policy),
                status: parse_workspace_attachment_status(&status),
                last_sync_at: r
                    .try_get::<Option<String>, _>("last_sync_at")?
                    .and_then(|v| parse_dt(&v).ok()),
                error_message: r.try_get("error_message")?,
                created_at: parse_dt(&created_at)?,
                updated_at: parse_dt(&updated_at)?,
            });
        }
        Ok(out)
    }

    pub async fn get_workspace_attachment(
        &self,
        id: WorkspaceAttachmentId,
    ) -> Result<Option<WorkspaceAttachment>> {
        let row = self
            .query(
                r#"SELECT id, workspace_id, kind, name, source, revision, subpath, mount_relpath, mode,
                          update_policy, status, last_sync_at, error_message, created_at, updated_at
                   FROM workspace_attachments
                   WHERE id = ?"#,
            )
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        let Some(r) = row else {
            return Ok(None);
        };
        let id: String = r.try_get("id")?;
        let ws_id: String = r.try_get("workspace_id")?;
        let created_at: String = r.try_get("created_at")?;
        let updated_at: String = r.try_get("updated_at")?;
        let kind: String = r.try_get("kind")?;
        let mode: String = r.try_get("mode")?;
        let update_policy: String = r.try_get("update_policy")?;
        let status: String = r.try_get("status")?;
        Ok(Some(WorkspaceAttachment {
            id: WorkspaceAttachmentId(uuid::Uuid::parse_str(&id)?),
            workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id)?),
            kind: parse_attachment_kind(&kind),
            name: r.try_get("name")?,
            source: r.try_get("source")?,
            revision: r.try_get("revision")?,
            subpath: r.try_get("subpath")?,
            mount_relpath: r.try_get("mount_relpath")?,
            mode: parse_attachment_mode(&mode),
            update_policy: parse_attachment_update_policy(&update_policy),
            status: parse_workspace_attachment_status(&status),
            last_sync_at: r
                .try_get::<Option<String>, _>("last_sync_at")?
                .and_then(|v| parse_dt(&v).ok()),
            error_message: r.try_get("error_message")?,
            created_at: parse_dt(&created_at)?,
            updated_at: parse_dt(&updated_at)?,
        }))
    }

    pub async fn upsert_workspace_attachment(
        &self,
        attachment: &WorkspaceAttachment,
    ) -> Result<()> {
        self.query(
            r#"INSERT INTO workspace_attachments
               (id, workspace_id, kind, name, source, revision, subpath, mount_relpath, mode, update_policy, status, last_sync_at, error_message, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                 kind = excluded.kind,
                 name = excluded.name,
                 source = excluded.source,
                 revision = excluded.revision,
                 subpath = excluded.subpath,
                 mount_relpath = excluded.mount_relpath,
                 mode = excluded.mode,
                 update_policy = excluded.update_policy,
                 status = excluded.status,
                 last_sync_at = excluded.last_sync_at,
                 error_message = excluded.error_message,
                 updated_at = excluded.updated_at"#,
        )
        .bind(attachment.id.0.to_string())
        .bind(attachment.workspace_id.0.to_string())
        .bind(attachment_kind_to_str(&attachment.kind))
        .bind(&attachment.name)
        .bind(&attachment.source)
        .bind(&attachment.revision)
        .bind(&attachment.subpath)
        .bind(&attachment.mount_relpath)
        .bind(attachment_mode_to_str(&attachment.mode))
        .bind(attachment_update_policy_to_str(&attachment.update_policy))
        .bind(workspace_attachment_status_to_str(&attachment.status))
        .bind(attachment.last_sync_at.map(|v| v.to_rfc3339()))
        .bind(&attachment.error_message)
        .bind(attachment.created_at.to_rfc3339())
        .bind(attachment.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_workspace_attachment_status(
        &self,
        id: WorkspaceAttachmentId,
        status: WorkspaceAttachmentStatus,
        last_sync_at: Option<DateTime<Utc>>,
        error_message: Option<String>,
        updated_at: DateTime<Utc>,
    ) -> Result<()> {
        self.query(
            r#"UPDATE workspace_attachments
               SET status = ?,
                   last_sync_at = COALESCE(?, last_sync_at),
                   error_message = ?,
                   updated_at = ?
               WHERE id = ?"#,
        )
        .bind(workspace_attachment_status_to_str(&status))
        .bind(last_sync_at.map(|v| v.to_rfc3339()))
        .bind(error_message)
        .bind(updated_at.to_rfc3339())
        .bind(id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_workspace_attachment(&self, id: WorkspaceAttachmentId) -> Result<()> {
        self.query(r#"DELETE FROM workspace_attachments WHERE id = ?"#)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_worktree_attachment_mounts(
        &self,
        worktree_id: WorktreeId,
    ) -> Result<Vec<WorktreeAttachmentMount>> {
        let rows = self
            .query(
                r#"SELECT worktree_id, attachment_id, mount_abs_path, materialized_id, status,
                      last_sync_at, error_message, created_at, updated_at
               FROM worktree_attachment_mounts
               WHERE worktree_id = ?
               ORDER BY created_at ASC"#,
            )
            .bind(worktree_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let worktree_id: String = r.try_get("worktree_id")?;
            let attachment_id: String = r.try_get("attachment_id")?;
            let status: String = r.try_get("status")?;
            let created_at: String = r.try_get("created_at")?;
            let updated_at: String = r.try_get("updated_at")?;
            out.push(WorktreeAttachmentMount {
                worktree_id: WorktreeId(uuid::Uuid::parse_str(&worktree_id)?),
                attachment_id: WorkspaceAttachmentId(uuid::Uuid::parse_str(&attachment_id)?),
                mount_abs_path: r.try_get("mount_abs_path")?,
                materialized_id: r.try_get("materialized_id")?,
                status: parse_worktree_attachment_status(&status),
                last_sync_at: r
                    .try_get::<Option<String>, _>("last_sync_at")?
                    .and_then(|v| parse_dt(&v).ok()),
                error_message: r.try_get("error_message")?,
                created_at: parse_dt(&created_at)?,
                updated_at: parse_dt(&updated_at)?,
            });
        }
        Ok(out)
    }

    pub async fn list_worktree_attachment_mounts_for_attachment(
        &self,
        attachment_id: WorkspaceAttachmentId,
    ) -> Result<Vec<WorktreeAttachmentMount>> {
        let rows = self
            .query(
                r#"SELECT worktree_id, attachment_id, mount_abs_path, materialized_id, status,
                      last_sync_at, error_message, created_at, updated_at
               FROM worktree_attachment_mounts
               WHERE attachment_id = ?
               ORDER BY created_at ASC"#,
            )
            .bind(attachment_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let worktree_id: String = r.try_get("worktree_id")?;
            let attachment_id: String = r.try_get("attachment_id")?;
            let status: String = r.try_get("status")?;
            let created_at: String = r.try_get("created_at")?;
            let updated_at: String = r.try_get("updated_at")?;
            out.push(WorktreeAttachmentMount {
                worktree_id: WorktreeId(uuid::Uuid::parse_str(&worktree_id)?),
                attachment_id: WorkspaceAttachmentId(uuid::Uuid::parse_str(&attachment_id)?),
                mount_abs_path: r.try_get("mount_abs_path")?,
                materialized_id: r.try_get("materialized_id")?,
                status: parse_worktree_attachment_status(&status),
                last_sync_at: r
                    .try_get::<Option<String>, _>("last_sync_at")?
                    .and_then(|v| parse_dt(&v).ok()),
                error_message: r.try_get("error_message")?,
                created_at: parse_dt(&created_at)?,
                updated_at: parse_dt(&updated_at)?,
            });
        }
        Ok(out)
    }

    pub async fn upsert_worktree_attachment_mount(
        &self,
        mount: &WorktreeAttachmentMount,
    ) -> Result<()> {
        self.query(
            r#"INSERT INTO worktree_attachment_mounts
               (worktree_id, attachment_id, mount_abs_path, materialized_id, status, last_sync_at, error_message, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(worktree_id, attachment_id) DO UPDATE SET
                 mount_abs_path = excluded.mount_abs_path,
                 materialized_id = excluded.materialized_id,
                 status = excluded.status,
                 last_sync_at = excluded.last_sync_at,
                 error_message = excluded.error_message,
                 updated_at = excluded.updated_at"#,
        )
        .bind(mount.worktree_id.0.to_string())
        .bind(mount.attachment_id.0.to_string())
        .bind(&mount.mount_abs_path)
        .bind(&mount.materialized_id)
        .bind(worktree_attachment_status_to_str(&mount.status))
        .bind(mount.last_sync_at.map(|v| v.to_rfc3339()))
        .bind(&mount.error_message)
        .bind(mount.created_at.to_rfc3339())
        .bind(mount.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_worktree_attachment_mounts_for_attachment(
        &self,
        attachment_id: WorkspaceAttachmentId,
    ) -> Result<()> {
        self.query(r#"DELETE FROM worktree_attachment_mounts WHERE attachment_id = ?"#)
            .bind(attachment_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn create_merge_queue_entry(&self, entry: &MergeQueueEntry) -> Result<()> {
        self.query(
            r#"INSERT INTO merge_queue_entries (
                   id, workspace_id, worktree_id, session_id, target_branch, message, patch_source,
                   base_commit_sha, head_commit_sha, patch_path, patch_size, status,
                   result_commit_sha, error_message, created_at, updated_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(entry.id.0.to_string())
        .bind(entry.workspace_id.0.to_string())
        .bind(entry.worktree_id.map(|id| id.0.to_string()))
        .bind(entry.session_id.map(|id| id.0.to_string()))
        .bind(&entry.target_branch)
        .bind(entry.message.as_deref())
        .bind(merge_queue_patch_source_to_str(&entry.patch_source))
        .bind(entry.base_commit_sha.as_deref())
        .bind(entry.head_commit_sha.as_deref())
        .bind(&entry.patch_path)
        .bind(entry.patch_size)
        .bind(merge_queue_entry_status_to_str(&entry.status))
        .bind(entry.result_commit_sha.as_deref())
        .bind(entry.error_message.as_deref())
        .bind(entry.created_at.to_rfc3339())
        .bind(entry.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_merge_queue_entry(&self, entry: &MergeQueueEntry) -> Result<()> {
        self.query(
            r#"UPDATE merge_queue_entries
               SET worktree_id = ?,
                   session_id = ?,
                   target_branch = ?,
                   message = ?,
                   patch_source = ?,
                   base_commit_sha = ?,
                   head_commit_sha = ?,
                   patch_path = ?,
                   patch_size = ?,
                   status = ?,
                   result_commit_sha = ?,
                   error_message = ?,
                   updated_at = ?
               WHERE id = ?"#,
        )
        .bind(entry.worktree_id.map(|id| id.0.to_string()))
        .bind(entry.session_id.map(|id| id.0.to_string()))
        .bind(&entry.target_branch)
        .bind(entry.message.as_deref())
        .bind(merge_queue_patch_source_to_str(&entry.patch_source))
        .bind(entry.base_commit_sha.as_deref())
        .bind(entry.head_commit_sha.as_deref())
        .bind(&entry.patch_path)
        .bind(entry.patch_size)
        .bind(merge_queue_entry_status_to_str(&entry.status))
        .bind(entry.result_commit_sha.as_deref())
        .bind(entry.error_message.as_deref())
        .bind(entry.updated_at.to_rfc3339())
        .bind(entry.id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_merge_queue_entry(
        &self,
        id: MergeQueueEntryId,
    ) -> Result<Option<MergeQueueEntry>> {
        let row = self
            .query(
                r#"SELECT id, workspace_id, worktree_id, session_id, target_branch, message,
                      patch_source, base_commit_sha, head_commit_sha, patch_path, patch_size,
                      status, result_commit_sha, error_message, created_at, updated_at
               FROM merge_queue_entries WHERE id = ?"#,
            )
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(map_merge_queue_entry))
    }

    pub async fn list_merge_queue_entries(
        &self,
        workspace_id: WorkspaceId,
        limit: Option<i64>,
    ) -> Result<Vec<MergeQueueEntry>> {
        let mut sql = String::from(
            r#"SELECT id, workspace_id, worktree_id, session_id, target_branch, message,
                      patch_source, base_commit_sha, head_commit_sha, patch_path, patch_size,
                      status, result_commit_sha, error_message, created_at, updated_at
               FROM merge_queue_entries WHERE workspace_id = ?"#,
        );
        sql.push_str(" ORDER BY created_at DESC");
        if limit.is_some() {
            sql.push_str(" LIMIT ?");
        }
        let sql = self.rewrite_sql(&sql);
        let mut query = sqlx::query(sql.as_ref()).bind(workspace_id.0.to_string());
        if let Some(limit) = limit {
            query = query.bind(limit);
        }
        let rows = query.fetch_all(&self.pool).await?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(entry) = map_merge_queue_entry(row) {
                out.push(entry);
            }
        }
        Ok(out)
    }

    pub async fn list_queued_merge_queue_entries(&self) -> Result<Vec<MergeQueueEntry>> {
        let rows = self
            .query(
                r#"SELECT id, workspace_id, worktree_id, session_id, target_branch, message,
                      patch_source, base_commit_sha, head_commit_sha, patch_path, patch_size,
                      status, result_commit_sha, error_message, created_at, updated_at
               FROM merge_queue_entries
               WHERE status = ?
               ORDER BY created_at ASC"#,
            )
            .bind("queued")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(entry) = map_merge_queue_entry(row) {
                out.push(entry);
            }
        }
        Ok(out)
    }

    pub async fn claim_merge_queue_entry(
        &self,
        entry_id: MergeQueueEntryId,
        updated_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = self
            .query(
                r#"UPDATE merge_queue_entries
               SET status = ?, updated_at = ?
               WHERE id = ? AND status = ?"#,
            )
            .bind("running")
            .bind(updated_at.to_rfc3339())
            .bind(entry_id.0.to_string())
            .bind("queued")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn has_merge_queue_blocking_failure(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<bool> {
        let row = self
            .query(
                r#"SELECT 1 FROM merge_queue_entries
               WHERE workspace_id = ? AND status IN ("failed", "conflict")
               LIMIT 1"#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn create_merge_queue_run(&self, run: &MergeQueueRun) -> Result<()> {
        self.query(
            r#"INSERT INTO merge_queue_runs (
                   id, entry_id, status, started_at, finished_at, exit_code,
                   log_path, error_message, result_commit_sha
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(run.id.0.to_string())
        .bind(run.entry_id.0.to_string())
        .bind(merge_queue_run_status_to_str(&run.status))
        .bind(run.started_at.to_rfc3339())
        .bind(run.finished_at.map(|dt| dt.to_rfc3339()))
        .bind(run.exit_code)
        .bind(run.log_path.as_deref())
        .bind(run.error_message.as_deref())
        .bind(run.result_commit_sha.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_merge_queue_run(&self, run: &MergeQueueRun) -> Result<()> {
        self.query(
            r#"UPDATE merge_queue_runs
               SET status = ?,
                   finished_at = ?,
                   exit_code = ?,
                   log_path = ?,
                   error_message = ?,
                   result_commit_sha = ?
               WHERE id = ?"#,
        )
        .bind(merge_queue_run_status_to_str(&run.status))
        .bind(run.finished_at.map(|dt| dt.to_rfc3339()))
        .bind(run.exit_code)
        .bind(run.log_path.as_deref())
        .bind(run.error_message.as_deref())
        .bind(run.result_commit_sha.as_deref())
        .bind(run.id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_merge_queue_runs(
        &self,
        entry_id: MergeQueueEntryId,
    ) -> Result<Vec<MergeQueueRun>> {
        let rows = self
            .query(
                r#"SELECT id, entry_id, status, started_at, finished_at, exit_code,
                      log_path, error_message, result_commit_sha
               FROM merge_queue_runs WHERE entry_id = ?
               ORDER BY started_at DESC"#,
            )
            .bind(entry_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(run) = map_merge_queue_run(row) {
                out.push(run);
            }
        }
        Ok(out)
    }

    pub async fn get_latest_merge_queue_run(
        &self,
        entry_id: MergeQueueEntryId,
    ) -> Result<Option<MergeQueueRun>> {
        let row = self
            .query(
                r#"SELECT id, entry_id, status, started_at, finished_at, exit_code,
                      log_path, error_message, result_commit_sha
               FROM merge_queue_runs WHERE entry_id = ?
               ORDER BY started_at DESC
               LIMIT 1"#,
            )
            .bind(entry_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(map_merge_queue_run))
    }
}

impl Store {
    pub async fn upsert_worktree_vcs_snapshot(
        &self,
        workspace_id: WorkspaceId,
        vcs_kind: Option<VcsKind>,
        snapshot: &WorktreeVcsSnapshot,
    ) -> Result<()> {
        let snapshot_json =
            serde_json::to_string(snapshot).context("serializing worktree vcs snapshot")?;
        let now = Utc::now().to_rfc3339();
        self.query(
            r#"INSERT INTO worktree_vcs_snapshot_cache (
                    worktree_id, workspace_id, vcs_kind, snapshot_json, updated_at
               )
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(worktree_id) DO UPDATE SET
                   workspace_id = excluded.workspace_id,
                   vcs_kind = excluded.vcs_kind,
                   snapshot_json = excluded.snapshot_json,
                   updated_at = excluded.updated_at"#,
        )
        .bind(snapshot.worktree_id.0.to_string())
        .bind(workspace_id.0.to_string())
        .bind(
            vcs_kind
                .map(|kind| serde_json::to_string(&kind))
                .transpose()?,
        )
        .bind(snapshot_json)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_worktree_vcs_snapshot(
        &self,
        worktree_id: WorktreeId,
    ) -> Result<Option<WorktreeVcsSnapshot>> {
        let row = self
            .query(
                r#"SELECT snapshot_json
                   FROM worktree_vcs_snapshot_cache
                   WHERE worktree_id = ?"#,
            )
            .bind(worktree_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let snapshot_json: String = row.try_get("snapshot_json")?;
        match serde_json::from_str(&snapshot_json) {
            Ok(snapshot) => Ok(Some(snapshot)),
            Err(err) => {
                tracing::warn!(
                    worktree_id = %worktree_id.0,
                    "ignoring malformed durable worktree vcs snapshot: {err}"
                );
                Ok(None)
            }
        }
    }

    pub async fn list_workspace_worktree_vcs_snapshots(
        &self,
        workspace_id: WorkspaceId,
        worktree_ids: &HashSet<WorktreeId>,
    ) -> Result<Vec<WorktreeVcsSnapshot>> {
        if worktree_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut ordered_ids = worktree_ids.iter().copied().collect::<Vec<_>>();
        ordered_ids.sort_by_key(|worktree_id| worktree_id.0);

        let mut sql = String::from(
            r#"SELECT worktree_id, snapshot_json
               FROM worktree_vcs_snapshot_cache
               WHERE workspace_id = ?
                 AND worktree_id IN ("#,
        );
        for (idx, _) in ordered_ids.iter().enumerate() {
            if idx > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
        }
        sql.push_str(") ORDER BY updated_at DESC, worktree_id ASC");

        let sql = self.rewrite_sql(&sql);
        let mut query = sqlx::query(sql.as_ref()).bind(workspace_id.0.to_string());
        for worktree_id in ordered_ids {
            query = query.bind(worktree_id.0.to_string());
        }
        let rows = query.fetch_all(&self.pool).await?;

        let mut snapshots = Vec::with_capacity(rows.len());
        for row in rows {
            let worktree_id: String = row.try_get("worktree_id")?;
            let snapshot_json: String = row.try_get("snapshot_json")?;
            match serde_json::from_str(&snapshot_json) {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(err) => {
                    tracing::warn!(
                        worktree_id,
                        "ignoring malformed durable worktree vcs snapshot row: {err}"
                    );
                }
            }
        }
        Ok(snapshots)
    }
}

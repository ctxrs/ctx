use super::*;

impl Store {
    pub async fn get_worktree_vcs_snapshot_cache(
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

        row.map(|row| -> Result<WorktreeVcsSnapshot> {
            let snapshot_json: String = row.try_get("snapshot_json")?;
            let snapshot = serde_json::from_str::<WorktreeVcsSnapshot>(&snapshot_json)
                .context("deserializing worktree vcs snapshot cache row")?;
            Ok(snapshot)
        })
        .transpose()
    }

    pub async fn upsert_worktree_vcs_snapshot_cache(
        &self,
        worktree: &Worktree,
        snapshot: &WorktreeVcsSnapshot,
    ) -> Result<()> {
        let snapshot_json =
            serde_json::to_string(snapshot).context("serializing worktree vcs snapshot cache")?;
        let vcs_kind = worktree.vcs_kind.as_ref().map(vcs_kind_to_str);
        self.query(
            r#"INSERT INTO worktree_vcs_snapshot_cache (
                   worktree_id,
                   workspace_id,
                   vcs_kind,
                   snapshot_json,
                   updated_at
               )
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(worktree_id) DO UPDATE SET
                   workspace_id = excluded.workspace_id,
                   vcs_kind = excluded.vcs_kind,
                   snapshot_json = excluded.snapshot_json,
                   updated_at = excluded.updated_at"#,
        )
        .bind(worktree.id.0.to_string())
        .bind(worktree.workspace_id.0.to_string())
        .bind(vcs_kind)
        .bind(snapshot_json)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_worktree_vcs_snapshot_cache(&self, worktree_id: WorktreeId) -> Result<()> {
        self.query(
            r#"DELETE FROM worktree_vcs_snapshot_cache
               WHERE worktree_id = ?"#,
        )
        .bind(worktree_id.0.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

use super::*;
use ctx_core::models::SandboxBinding;

impl Store {
    pub async fn upsert_sandbox_binding(&self, binding: SandboxBinding) -> Result<SandboxBinding> {
        self.query(
            r#"INSERT INTO sandbox_bindings (
                   worktree_id,
                   workspace_id,
                   sandbox_instance_id,
                   runtime_family,
                   guest_platform,
                   isolation_kind,
                   guest_runtime,
                   profile,
                   live_workspace_root,
                   live_worktree_root,
                   execution_settings_json,
                   container_name,
                   host_projection_root,
                   created_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(worktree_id) DO UPDATE SET
                   workspace_id = excluded.workspace_id,
                   sandbox_instance_id = excluded.sandbox_instance_id,
                   runtime_family = excluded.runtime_family,
                   guest_platform = excluded.guest_platform,
                   isolation_kind = excluded.isolation_kind,
                   guest_runtime = excluded.guest_runtime,
                   profile = excluded.profile,
                   live_workspace_root = excluded.live_workspace_root,
                   live_worktree_root = excluded.live_worktree_root,
                   execution_settings_json = excluded.execution_settings_json,
                   container_name = excluded.container_name,
                   host_projection_root = excluded.host_projection_root"#,
        )
        .bind(binding.worktree_id.0.to_string())
        .bind(binding.workspace_id.0.to_string())
        .bind(binding.sandbox_instance_id.0.to_string())
        .bind(sandbox_substrate_to_str(&binding.substrate))
        .bind(sandbox_guest_platform_to_str(
            binding.guest_identity.platform,
        ))
        .bind(sandbox_isolation_kind_to_str(
            binding.guest_identity.isolation_kind,
        ))
        .bind(sandbox_guest_runtime_to_str(binding.guest_identity.runtime))
        .bind(sandbox_profile_to_str(&binding.profile))
        .bind(&binding.live_workspace_root)
        .bind(&binding.live_worktree_root)
        .bind(&binding.execution_settings_json)
        .bind(&binding.container_name)
        .bind(&binding.host_materialization_root)
        .bind(binding.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(binding)
    }

    pub async fn get_sandbox_binding(
        &self,
        worktree_id: WorktreeId,
    ) -> Result<Option<SandboxBinding>> {
        let row = self
            .query(
                r#"SELECT worktree_id, workspace_id, sandbox_instance_id, runtime_family, guest_platform,
                          isolation_kind, guest_runtime, profile, live_workspace_root,
                          live_worktree_root, execution_settings_json, container_name,
                          host_projection_root, created_at
                   FROM sandbox_bindings
                   WHERE worktree_id = ?"#,
            )
            .bind(worktree_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.and_then(map_sandbox_binding))
    }

    pub async fn delete_sandbox_binding(&self, worktree_id: WorktreeId) -> Result<()> {
        self.query("DELETE FROM sandbox_bindings WHERE worktree_id = ?")
            .bind(worktree_id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

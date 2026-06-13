use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ctx_core::ids::{MergeQueueEntryId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    MergeQueueEntry, MergeQueueEntryStatus, MergeQueuePatchSource, MergeQueueRun, Worktree,
};

use crate::daemon;

use super::TestDaemon;

impl TestDaemon {
    pub fn spawn_merge_queue_runner(&self) {
        daemon::merge_queue::spawn_merge_queue_runner(Arc::clone(&self.state));
    }

    pub async fn configure_merge_queue_for_test(
        &self,
        workspace_id: WorkspaceId,
        target_branch: &str,
        canonical_sync: &str,
        verify_commands: &[&str],
        push_on_success: bool,
        push_remote: Option<&str>,
        push_branch: Option<&str>,
    ) -> anyhow::Result<()> {
        let canonical_sync = match canonical_sync {
            "never" => ctx_workspace_config::MergeQueueCanonicalSync::Never,
            "clean_only" => ctx_workspace_config::MergeQueueCanonicalSync::CleanOnly,
            "force" => ctx_workspace_config::MergeQueueCanonicalSync::Force,
            _ => anyhow::bail!("unsupported canonical sync mode: {canonical_sync}"),
        };
        let store = self.state.store_for_workspace(workspace_id).await?;
        ctx_workspace_config::update_merge_queue_config(
            &store,
            ctx_workspace_config::MergeQueueConfigUpdate {
                enabled: true,
                target_branch: Some(target_branch.to_string()),
                verify_commands: verify_commands
                    .iter()
                    .map(|command| (*command).to_string())
                    .collect(),
                push_on_success: Some(push_on_success),
                push_remote: push_remote.map(ToString::to_string),
                push_branch: push_branch.map(ToString::to_string),
                canonical_sync: Some(canonical_sync),
            },
        )
        .await?;
        Ok(())
    }

    pub async fn seed_merge_queue_worktree_for_test(
        &self,
        workspace_id: WorkspaceId,
        root_path: &Path,
        commit_sha: &str,
        branch: Option<&str>,
    ) -> anyhow::Result<Worktree> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        let worktree = store
            .create_worktree(
                workspace_id,
                root_path.to_string_lossy().to_string(),
                commit_sha.to_string(),
                branch.map(ToString::to_string),
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace_id)
            .await?;
        Ok(worktree)
    }

    pub async fn seed_workspace_merge_queue_queued_entry_for_test(
        &self,
        workspace_id: WorkspaceId,
        name: &str,
    ) -> anyhow::Result<MergeQueueEntry> {
        let now = chrono::Utc::now();
        let entry = MergeQueueEntry {
            id: MergeQueueEntryId::new(),
            workspace_id,
            worktree_id: None,
            session_id: None,
            target_branch: "main".to_string(),
            message: Some(name.to_string()),
            patch_source: MergeQueuePatchSource::Generated,
            base_commit_sha: Some(format!("{name}-base")),
            head_commit_sha: Some(format!("{name}-head")),
            patch_path: format!("/tmp/{name}.patch"),
            patch_size: 1,
            status: MergeQueueEntryStatus::Queued,
            result_commit_sha: None,
            error_message: None,
            created_at: now,
            updated_at: now,
        };
        let store = self.uncached_store_for_workspace(workspace_id).await?;
        store.create_merge_queue_entry(&entry).await?;
        store.close().await;
        Ok(entry)
    }

    pub async fn load_workspace_merge_queue_entry_for_test(
        &self,
        workspace_id: WorkspaceId,
        entry_id: MergeQueueEntryId,
    ) -> anyhow::Result<MergeQueueEntry> {
        let store = self.uncached_store_for_workspace(workspace_id).await?;
        let entry = store
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("merge queue entry {entry_id:?} should exist"))?;
        if entry.workspace_id != workspace_id {
            anyhow::bail!(
                "merge queue entry {entry_id:?} belongs to {:?}, not {:?}",
                entry.workspace_id,
                workspace_id
            );
        }
        store.close().await;
        Ok(entry)
    }

    pub async fn latest_merge_queue_entry_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<MergeQueueEntry> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        store
            .list_merge_queue_entries(workspace_id, Some(1))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("expected merge queue entry for {workspace_id:?}"))
    }

    pub async fn wait_for_workspace_merge_queue_entry_to_leave_queued_for_test(
        &self,
        workspace_id: WorkspaceId,
        entry_id: MergeQueueEntryId,
        timeout: Duration,
    ) -> anyhow::Result<MergeQueueEntry> {
        tokio::time::timeout(timeout, async {
            loop {
                let entry = self
                    .load_workspace_merge_queue_entry_for_test(workspace_id, entry_id)
                    .await?;
                if entry.status != MergeQueueEntryStatus::Queued {
                    break Ok(entry);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for merge queue entry to resume"))?
    }

    pub async fn wait_for_merge_queue_entry_for_test(
        &self,
        workspace_id: WorkspaceId,
        entry_id: MergeQueueEntryId,
        timeout: Duration,
    ) -> anyhow::Result<MergeQueueEntry> {
        tokio::time::timeout(timeout, async {
            loop {
                let entry = self
                    .load_workspace_merge_queue_entry_for_test(workspace_id, entry_id)
                    .await?;
                match entry.status {
                    MergeQueueEntryStatus::Queued | MergeQueueEntryStatus::Running => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    _ => break Ok(entry),
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for merge queue entry {entry_id:?}"))?
    }

    pub async fn latest_merge_queue_run_for_test(
        &self,
        workspace_id: WorkspaceId,
        entry_id: MergeQueueEntryId,
    ) -> anyhow::Result<MergeQueueRun> {
        let entry = self
            .load_workspace_merge_queue_entry_for_test(workspace_id, entry_id)
            .await?;
        let store = self.state.store_for_workspace(entry.workspace_id).await?;
        store
            .get_latest_merge_queue_run(entry.id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("expected merge queue run for {entry_id:?}"))
    }

    pub async fn merge_queue_worktree_for_test(
        &self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<Worktree> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        let worktree = store
            .get_worktree(worktree_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("expected merge queue worktree {worktree_id:?}"))?;
        if worktree.workspace_id != workspace_id {
            anyhow::bail!(
                "merge queue worktree {worktree_id:?} belongs to {:?}, not {:?}",
                worktree.workspace_id,
                workspace_id
            );
        }
        Ok(worktree)
    }
}

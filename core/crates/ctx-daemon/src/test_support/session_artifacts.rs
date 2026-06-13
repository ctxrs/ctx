use std::path::{Path, PathBuf};

use ctx_core::ids::{MergeQueueEntryId, MergeQueueRunId, SessionId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    Artifact, MergeQueueEntry, MergeQueueEntryStatus, MergeQueuePatchSource, MergeQueueRun,
    MergeQueueRunStatus, Session, WorktreeBootstrapStatus,
};
use ctx_store::WorktreeBootstrapResultUpdate;
use sha2::Digest;

use super::TestDaemon;

impl TestDaemon {
    pub async fn seed_legacy_session_artifact_by_path_for_test(
        &self,
        session: &Session,
        absolute_path: &Path,
        name: &str,
        mime_type: &str,
        bytes: i64,
    ) -> anyhow::Result<Artifact> {
        let artifact = Artifact {
            id: ctx_core::ids::ArtifactId::new(),
            session_id: session.id,
            task_id: session.task_id,
            workspace_id: session.workspace_id,
            worktree_id: session.worktree_id,
            name: Some(name.to_string()),
            absolute_path: absolute_path.to_string_lossy().to_string(),
            mime_type: mime_type.to_string(),
            bytes,
            created_at: chrono::Utc::now(),
            missing: None,
        };
        self.state
            .store_for_session(session.id)
            .await?
            .upsert_session_artifact_by_path(&artifact)
            .await
            .map_err(Into::into)
    }

    pub async fn record_worktree_bootstrap_log_for_test(
        &self,
        session: &Session,
        status: WorktreeBootstrapStatus,
        log_path: &Path,
        error: Option<&str>,
        command: &str,
    ) -> anyhow::Result<WorktreeId> {
        let worktree = self.load_worktree_for_test(session.worktree_id).await?;
        let now = chrono::Utc::now();
        self.state
            .store_for_worktree(worktree.id)
            .await?
            .update_worktree_bootstrap_result(WorktreeBootstrapResultUpdate {
                worktree_id: worktree.id,
                status,
                started_at: now,
                finished_at: now,
                exit_code: Some(if error.is_some() { 1 } else { 0 }),
                timeout_sec: Some(60),
                error: error.map(str::to_string),
                log_path: Some(log_path.to_string_lossy().to_string()),
                log_truncated: Some(false),
                command: Some(command.to_string()),
                script_path: None,
            })
            .await?;
        Ok(worktree.id)
    }

    pub async fn seed_failed_merge_queue_log_run_for_test(
        &self,
        workspace_id: WorkspaceId,
        message: &str,
        log_path: &Path,
        error_message: &str,
    ) -> anyhow::Result<MergeQueueEntryId> {
        let now = chrono::Utc::now();
        let entry = MergeQueueEntry {
            id: MergeQueueEntryId::new(),
            workspace_id,
            worktree_id: None,
            session_id: None,
            target_branch: "main".to_string(),
            message: Some(message.to_string()),
            patch_source: MergeQueuePatchSource::Generated,
            base_commit_sha: Some("base".to_string()),
            head_commit_sha: Some("head".to_string()),
            patch_path: "/tmp/log-path-boundary.patch".to_string(),
            patch_size: 1,
            status: MergeQueueEntryStatus::Failed,
            result_commit_sha: None,
            error_message: Some("failed".to_string()),
            created_at: now,
            updated_at: now,
        };
        let run = MergeQueueRun {
            id: MergeQueueRunId::new(),
            entry_id: entry.id,
            status: MergeQueueRunStatus::Failed,
            started_at: now,
            finished_at: Some(now),
            exit_code: Some(1),
            log_path: Some(log_path.to_string_lossy().to_string()),
            error_message: Some(error_message.to_string()),
            result_commit_sha: None,
        };
        let store = self.state.store_for_workspace(workspace_id).await?;
        store.create_merge_queue_entry(&entry).await?;
        store.create_merge_queue_run(&run).await?;
        Ok(entry.id)
    }

    pub async fn session_has_no_persisted_messages_for_test(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<bool> {
        Ok(self
            .state
            .store_for_session(session_id)
            .await?
            .list_messages_for_session(session_id)
            .await?
            .is_empty())
    }

    pub async fn seed_non_image_attachment_blob_for_test(
        &self,
        blob_id: &str,
        bytes: &[u8],
        name: &str,
    ) -> anyhow::Result<PathBuf> {
        let blob_path = self.state.core.data_root.join("blobs").join(blob_id);
        if let Some(parent) = blob_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&blob_path, bytes).await?;

        let sha256 = hex::encode(sha2::Sha256::digest(bytes));
        self.state
            .global_store()
            .insert_blob(
                blob_id,
                &sha256,
                bytes.len() as i64,
                "text/plain",
                Some(name),
                chrono::Utc::now(),
            )
            .await?;
        Ok(blob_path)
    }

    pub async fn seed_oversized_image_attachment_blob_metadata_for_test(
        &self,
        blob_id: &str,
        byte_count: i64,
        name: &str,
    ) -> anyhow::Result<()> {
        let sha_input = format!("{blob_id}:{byte_count}:image/png");
        let sha256 = hex::encode(sha2::Sha256::digest(sha_input.as_bytes()));
        self.state
            .global_store()
            .insert_blob(
                blob_id,
                &sha256,
                byte_count,
                "image/png",
                Some(name),
                chrono::Utc::now(),
            )
            .await?;
        Ok(())
    }
}

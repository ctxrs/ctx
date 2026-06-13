use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use anyhow::{Context, Result};
use ctx_core::ids::{WorkspaceAttachmentId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    Workspace, WorkspaceAttachment, WorkspaceAttachmentKind, Worktree, WorktreeAttachmentMount,
};
use ctx_store::Store;
use ctx_workspace_runtime::HarnessRuntimeManager;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::daemon::ProtectedWorkspaceStoreLookup;

use super::materialization::{cancel_attachment_materialization, spawn_attachment_materialization};
use super::mounts::{
    ensure_workspace_attachments_for_worktrees_with_attachments,
    ensure_worktree_attachment_mounts_if_materialized,
};

pub(crate) struct WorkspaceAttachmentMaterializationRuntime {
    pub(in crate::daemon::workspaces::attachments) tasks:
        Mutex<HashMap<WorkspaceAttachmentId, AttachmentMaterializationTask>>,
    pub(in crate::daemon::workspaces::attachments) generation: AtomicU64,
}

impl WorkspaceAttachmentMaterializationRuntime {
    pub(crate) fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
        }
    }
}

pub(in crate::daemon::workspaces::attachments) struct AttachmentMaterializationTask {
    pub generation: u64,
    pub handle: JoinHandle<()>,
}

#[derive(Clone)]
pub(crate) struct WorkspaceAttachmentsRuntime {
    data_root: PathBuf,
    daemon_url: String,
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    harness: Arc<HarnessRuntimeManager>,
    materialization: Arc<WorkspaceAttachmentMaterializationRuntime>,
}

impl WorkspaceAttachmentsRuntime {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        daemon_url: String,
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        harness: Arc<HarnessRuntimeManager>,
        materialization: Arc<WorkspaceAttachmentMaterializationRuntime>,
    ) -> Self {
        Self {
            data_root,
            daemon_url,
            global_store,
            workspace_stores,
            harness,
            materialization,
        }
    }

    pub(crate) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(crate) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub(crate) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(crate) async fn store_for_workspace(&self, workspace_id: WorkspaceId) -> Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    pub(crate) async fn store_for_worktree(&self, worktree_id: WorktreeId) -> Result<Store> {
        let workspace_id = self
            .global_store
            .get_workspace_id_for_worktree(worktree_id)
            .await?
            .with_context(|| format!("workspace missing for worktree {}", worktree_id.0))?;
        self.store_for_workspace(workspace_id).await
    }

    pub(crate) fn harness(&self) -> &HarnessRuntimeManager {
        self.harness.as_ref()
    }

    pub(in crate::daemon::workspaces::attachments) fn materialization(
        &self,
    ) -> &WorkspaceAttachmentMaterializationRuntime {
        self.materialization.as_ref()
    }

    pub(crate) async fn sync_workspace_attachments(
        self: &Arc<Self>,
        workspace: &Workspace,
        refresh: bool,
    ) -> Result<Vec<WorkspaceAttachment>> {
        let result = ctx_workspace_attachments::sync_workspace_attachments(
            self.as_ref(),
            workspace,
            refresh,
        )
        .await?;
        for plan in result.plans {
            spawn_attachment_materialization(
                Arc::clone(self),
                workspace.clone(),
                plan.id,
                plan.refresh,
            )
            .await;
        }
        let attachments = result.attachments;
        let _ = ensure_workspace_attachments_for_worktrees_with_attachments(
            self.as_ref(),
            workspace,
            &attachments,
            false,
            false,
        )
        .await;
        Ok(attachments)
    }

    pub(crate) async fn upsert_workspace_attachment(
        &self,
        workspace_id: WorkspaceId,
        cfg: ctx_workspace_attachments::AttachmentConfig,
    ) -> Result<WorkspaceAttachment> {
        ctx_workspace_attachments::upsert_workspace_attachment(self, workspace_id, cfg).await
    }

    pub(crate) async fn delete_workspace_attachment(
        &self,
        workspace_id: WorkspaceId,
        kind: WorkspaceAttachmentKind,
        name: &str,
    ) -> Result<bool> {
        let Some(target) =
            ctx_workspace_attachments::find_workspace_attachment(self, workspace_id, kind, name)
                .await?
        else {
            return Ok(false);
        };
        cancel_attachment_materialization(self, target.id).await;
        ctx_workspace_attachments::delete_workspace_attachment(self, &target).await?;
        Ok(true)
    }

    pub(crate) async fn ensure_worktree_attachment_mounts_if_materialized(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> Result<Vec<WorktreeAttachmentMount>> {
        ensure_worktree_attachment_mounts_if_materialized(self, workspace, worktree).await
    }
}

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ctx_core::ids::{SessionId, WorktreeId};
use ctx_core::models::{Worktree, WorktreeVcsSnapshot};
use ctx_session_vcs_service::vcs::SessionVcsDiffBaseQuery;
use ctx_store::Store;
use ctx_worktree_vcs_service::{
    GitStatusSnapshot, WorktreeDiffBaseResolution, WorktreeVcsDiffSummaryCounts,
};

use crate::daemon::state::{session_store_access_anyhow, SessionStoreLookup};

pub(in crate::daemon) type SessionVcsFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(in crate::daemon) type SessionVcsWorktreeBoolEffect =
    Arc<dyn Fn(Worktree) -> SessionVcsFuture<anyhow::Result<bool>> + Send + Sync>;
pub(in crate::daemon) type SessionVcsGitStatusEffect = Arc<
    dyn Fn(Worktree, bool, bool) -> SessionVcsFuture<anyhow::Result<GitStatusSnapshot>>
        + Send
        + Sync,
>;
pub(in crate::daemon) type SessionVcsCommitEffect =
    Arc<dyn Fn(Worktree, String) -> SessionVcsFuture<anyhow::Result<String>> + Send + Sync>;
pub(in crate::daemon) type SessionVcsDiffEffect =
    Arc<dyn Fn(Worktree, String) -> SessionVcsFuture<anyhow::Result<String>> + Send + Sync>;
pub(in crate::daemon) type SessionVcsDiffSummaryEffect = Arc<
    dyn Fn(Worktree, String) -> SessionVcsFuture<anyhow::Result<WorktreeVcsDiffSummaryCounts>>
        + Send
        + Sync,
>;
pub(in crate::daemon) type SessionVcsDiffBaseEffect = Arc<
    dyn Fn(Worktree, SessionVcsDiffBaseQuery) -> SessionVcsFuture<WorktreeDiffBaseResolution>
        + Send
        + Sync,
>;
pub(in crate::daemon) type SessionVcsPatchEffect =
    Arc<dyn Fn(Worktree, String, bool) -> SessionVcsFuture<anyhow::Result<()>> + Send + Sync>;
pub(in crate::daemon) type SessionVcsSnapshotEffect =
    Arc<dyn Fn(WorktreeId) -> SessionVcsFuture<Option<WorktreeVcsSnapshot>> + Send + Sync>;
pub(in crate::daemon) type SessionVcsCompatMetricEffect =
    Arc<dyn Fn(&'static str, &'static str) -> SessionVcsFuture<()> + Send + Sync>;
pub(in crate::daemon) type SessionVcsNoRepoClassifier =
    Arc<dyn Fn(&anyhow::Error) -> bool + Send + Sync>;

pub(in crate::daemon) struct SessionVcsEffectsParts {
    pub(in crate::daemon) worktree_has_vcs_repo: SessionVcsWorktreeBoolEffect,
    pub(in crate::daemon) load_git_status_snapshot: SessionVcsGitStatusEffect,
    pub(in crate::daemon) resolve_worktree_commit: SessionVcsCommitEffect,
    pub(in crate::daemon) diff_worktree_for_session: SessionVcsDiffEffect,
    pub(in crate::daemon) diff_worktree_summary_for_session: SessionVcsDiffSummaryEffect,
    pub(in crate::daemon) resolve_worktree_diff_base: SessionVcsDiffBaseEffect,
    pub(in crate::daemon) apply_worktree_vcs_session_patch: SessionVcsPatchEffect,
    pub(in crate::daemon) cached_worktree_vcs_snapshot: SessionVcsSnapshotEffect,
    pub(in crate::daemon) emit_compat_payload_reject_counter: SessionVcsCompatMetricEffect,
    pub(in crate::daemon) is_no_vcs_repo_error: SessionVcsNoRepoClassifier,
}

pub(in crate::daemon) struct SessionVcsEffects {
    worktree_has_vcs_repo: SessionVcsWorktreeBoolEffect,
    load_git_status_snapshot: SessionVcsGitStatusEffect,
    resolve_worktree_commit: SessionVcsCommitEffect,
    diff_worktree_for_session: SessionVcsDiffEffect,
    diff_worktree_summary_for_session: SessionVcsDiffSummaryEffect,
    resolve_worktree_diff_base: SessionVcsDiffBaseEffect,
    apply_worktree_vcs_session_patch: SessionVcsPatchEffect,
    cached_worktree_vcs_snapshot: SessionVcsSnapshotEffect,
    emit_compat_payload_reject_counter: SessionVcsCompatMetricEffect,
    is_no_vcs_repo_error: SessionVcsNoRepoClassifier,
}

impl SessionVcsEffects {
    pub(in crate::daemon) fn new(parts: SessionVcsEffectsParts) -> Arc<Self> {
        Arc::new(Self {
            worktree_has_vcs_repo: parts.worktree_has_vcs_repo,
            load_git_status_snapshot: parts.load_git_status_snapshot,
            resolve_worktree_commit: parts.resolve_worktree_commit,
            diff_worktree_for_session: parts.diff_worktree_for_session,
            diff_worktree_summary_for_session: parts.diff_worktree_summary_for_session,
            resolve_worktree_diff_base: parts.resolve_worktree_diff_base,
            apply_worktree_vcs_session_patch: parts.apply_worktree_vcs_session_patch,
            cached_worktree_vcs_snapshot: parts.cached_worktree_vcs_snapshot,
            emit_compat_payload_reject_counter: parts.emit_compat_payload_reject_counter,
            is_no_vcs_repo_error: parts.is_no_vcs_repo_error,
        })
    }
}

#[derive(Clone)]
pub struct SessionVcsHandle {
    lookup: SessionStoreLookup,
    effects: Arc<SessionVcsEffects>,
}

impl SessionVcsHandle {
    pub(in crate::daemon) fn new(
        lookup: SessionStoreLookup,
        effects: Arc<SessionVcsEffects>,
    ) -> Self {
        Self { lookup, effects }
    }

    pub(in crate::daemon) async fn session_store_or_none(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<Store>> {
        match self.lookup.existing_session_store(session_id).await {
            Ok(store) => Ok(Some(store)),
            Err(crate::daemon::SessionStoreAccessError::NotFound) => Ok(None),
            Err(error) => Err(session_store_access_anyhow(error)),
        }
    }

    pub(in crate::daemon) async fn session_store_for_write_or_none(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<Store>> {
        match self
            .lookup
            .existing_session_store_for_write(session_id)
            .await
        {
            Ok(store) => Ok(Some(store)),
            Err(crate::daemon::SessionStoreAccessError::NotFound) => Ok(None),
            Err(error) => Err(session_store_access_anyhow(error)),
        }
    }

    pub(in crate::daemon) async fn worktree_has_vcs_repo(
        &self,
        worktree: &Worktree,
    ) -> anyhow::Result<bool> {
        (self.effects.worktree_has_vcs_repo)(worktree.clone()).await
    }

    pub(in crate::daemon) async fn load_git_status_snapshot(
        &self,
        worktree: &Worktree,
        include_untracked_files: bool,
        include_entries: bool,
    ) -> anyhow::Result<GitStatusSnapshot> {
        (self.effects.load_git_status_snapshot)(
            worktree.clone(),
            include_untracked_files,
            include_entries,
        )
        .await
    }

    pub(in crate::daemon) async fn resolve_worktree_commit(
        &self,
        worktree: &Worktree,
        revision: &str,
    ) -> anyhow::Result<String> {
        (self.effects.resolve_worktree_commit)(worktree.clone(), revision.to_string()).await
    }

    pub(in crate::daemon) async fn diff_worktree_for_session(
        &self,
        worktree: &Worktree,
        base_commit_sha: &str,
    ) -> anyhow::Result<String> {
        (self.effects.diff_worktree_for_session)(worktree.clone(), base_commit_sha.to_string())
            .await
    }

    pub(in crate::daemon) async fn diff_worktree_summary_for_session(
        &self,
        worktree: &Worktree,
        base_commit_sha: &str,
    ) -> anyhow::Result<WorktreeVcsDiffSummaryCounts> {
        (self.effects.diff_worktree_summary_for_session)(
            worktree.clone(),
            base_commit_sha.to_string(),
        )
        .await
    }

    pub(in crate::daemon) async fn resolve_worktree_diff_base(
        &self,
        worktree: &Worktree,
        query: SessionVcsDiffBaseQuery,
    ) -> WorktreeDiffBaseResolution {
        (self.effects.resolve_worktree_diff_base)(worktree.clone(), query).await
    }

    pub(in crate::daemon) async fn apply_worktree_vcs_session_patch(
        &self,
        worktree: &Worktree,
        patch: &str,
        reverse_patch: bool,
    ) -> anyhow::Result<()> {
        (self.effects.apply_worktree_vcs_session_patch)(
            worktree.clone(),
            patch.to_string(),
            reverse_patch,
        )
        .await
    }

    pub(in crate::daemon) async fn cached_worktree_vcs_snapshot(
        &self,
        worktree_id: WorktreeId,
    ) -> Option<WorktreeVcsSnapshot> {
        (self.effects.cached_worktree_vcs_snapshot)(worktree_id).await
    }

    pub(in crate::daemon) async fn emit_compat_payload_reject_counter(
        &self,
        surface: &'static str,
        issue: &'static str,
    ) {
        (self.effects.emit_compat_payload_reject_counter)(surface, issue).await;
    }

    pub(in crate::daemon) fn is_no_vcs_repo_error(&self, error: &anyhow::Error) -> bool {
        (self.effects.is_no_vcs_repo_error)(error)
    }
}

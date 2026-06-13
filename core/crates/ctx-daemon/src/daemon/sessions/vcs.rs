use anyhow::Error;
use async_trait::async_trait;
use ctx_core::ids::{SessionId, WorktreeId};
use ctx_core::models::{Session, SessionGitStatusSummary, Worktree};
pub use ctx_session_vcs_service::vcs::{
    SessionVcsApplyAction, SessionVcsDiff, SessionVcsDiffQuery, SessionVcsDiffSummary,
    SessionVcsError, SessionVcsGitStatus, SessionVcsGitStatusEntry,
};
use ctx_session_vcs_service::vcs::{
    SessionVcsDataPlane, SessionVcsDiffBaseQuery, SessionVcsDiffBaseResolution,
    SessionVcsDiffSummaryCounts, SessionVcsDiffSummaryMismatch, SessionVcsGitStatusSnapshot,
    SessionVcsService,
};
use ctx_worktree_vcs_service::{
    GitStatusEntry, GitStatusSnapshot, WorktreeDiffBaseResolution, WorktreeVcsDiffSummaryCounts,
    WorktreeVcsDiffSummaryMismatch,
};

use crate::daemon::SessionVcsHandle;

impl SessionVcsHandle {
    pub async fn get_session_vcs_diff_for_request(
        &self,
        session_id: SessionId,
        query: SessionVcsDiffQuery,
    ) -> Result<SessionVcsDiff, SessionVcsError> {
        SessionVcsService::new(self)
            .get_session_vcs_diff(session_id, query)
            .await
    }

    pub async fn get_session_vcs_diff_summary_for_request(
        &self,
        session_id: SessionId,
        query: SessionVcsDiffQuery,
    ) -> Result<SessionVcsDiffSummary, SessionVcsError> {
        SessionVcsService::new(self)
            .get_session_vcs_diff_summary(session_id, query)
            .await
    }

    pub async fn apply_session_vcs_diff_patch_for_request(
        &self,
        session_id: SessionId,
        action: SessionVcsApplyAction,
        patch: &str,
    ) -> Result<SessionVcsDiff, SessionVcsError> {
        SessionVcsService::new(self)
            .apply_session_vcs_diff_patch(session_id, action, patch)
            .await
    }

    pub async fn get_session_vcs_git_status_for_request(
        &self,
        session_id: SessionId,
    ) -> Result<SessionVcsGitStatus, SessionVcsError> {
        SessionVcsService::new(self)
            .get_session_vcs_git_status(session_id)
            .await
    }
}

#[async_trait]
impl SessionVcsDataPlane for SessionVcsHandle {
    async fn load_session_vcs_parts(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<(Session, Worktree)>> {
        let Some(store) = self.session_store_or_none(session_id).await? else {
            return Ok(None);
        };
        let Some(session) = store.get_session(session_id).await? else {
            return Ok(None);
        };
        let Some(worktree) = store.get_worktree(session.worktree_id).await? else {
            return Ok(None);
        };
        Ok(Some((session, worktree)))
    }

    async fn persist_session_git_status_summary(
        &self,
        session_id: SessionId,
        worktree_id: WorktreeId,
        summary: &SessionGitStatusSummary,
    ) -> anyhow::Result<()> {
        let Some(store) = self.session_store_for_write_or_none(session_id).await? else {
            anyhow::bail!("session not found");
        };
        store
            .upsert_session_git_status_summary(session_id, worktree_id, summary)
            .await
    }

    async fn worktree_has_vcs_repo(&self, worktree: &Worktree) -> anyhow::Result<bool> {
        self.worktree_has_vcs_repo(worktree).await
    }

    async fn load_git_status_snapshot(
        &self,
        worktree: &Worktree,
        include_untracked_files: bool,
        include_entries: bool,
    ) -> anyhow::Result<SessionVcsGitStatusSnapshot> {
        self.load_git_status_snapshot(worktree, include_untracked_files, include_entries)
            .await
            .map(session_vcs_git_status_snapshot)
    }

    async fn resolve_worktree_commit(
        &self,
        worktree: &Worktree,
        revision: &str,
    ) -> anyhow::Result<String> {
        self.resolve_worktree_commit(worktree, revision).await
    }

    async fn diff_worktree_for_session(
        &self,
        worktree: &Worktree,
        base_commit_sha: &str,
    ) -> anyhow::Result<String> {
        self.diff_worktree_for_session(worktree, base_commit_sha)
            .await
    }

    async fn diff_worktree_summary_for_session(
        &self,
        worktree: &Worktree,
        base_commit_sha: &str,
    ) -> anyhow::Result<SessionVcsDiffSummaryCounts> {
        self.diff_worktree_summary_for_session(worktree, base_commit_sha)
            .await
            .map(session_vcs_diff_summary_counts)
    }

    async fn resolve_worktree_diff_base(
        &self,
        worktree: &Worktree,
        query: SessionVcsDiffBaseQuery,
    ) -> SessionVcsDiffBaseResolution {
        let resolution = self.resolve_worktree_diff_base(worktree, query).await;
        session_vcs_diff_base_resolution(resolution)
    }

    async fn apply_worktree_vcs_session_patch(
        &self,
        worktree: &Worktree,
        patch: &str,
        reverse_patch: bool,
    ) -> anyhow::Result<()> {
        self.apply_worktree_vcs_session_patch(worktree, patch, reverse_patch)
            .await
    }

    async fn session_vcs_diff_summary_mismatch(
        &self,
        worktree: &Worktree,
        base_commit_sha: &str,
        counts: SessionVcsDiffSummaryCounts,
    ) -> Option<SessionVcsDiffSummaryMismatch> {
        let snapshot = self.cached_worktree_vcs_snapshot(worktree.id).await?;
        ctx_worktree_vcs_service::worktree_vcs_diff_summary_mismatch(
            &snapshot,
            base_commit_sha,
            workspace_vcs_diff_summary_counts(counts),
        )
        .map(|mismatch| session_vcs_diff_summary_mismatch(snapshot.rev, mismatch))
    }

    async fn emit_compat_payload_reject_counter(&self, surface: &'static str, issue: &'static str) {
        self.emit_compat_payload_reject_counter(surface, issue)
            .await;
    }

    fn is_no_vcs_repo_error(&self, error: &Error) -> bool {
        self.is_no_vcs_repo_error(error)
    }
}

fn session_vcs_diff_base_resolution(
    resolution: WorktreeDiffBaseResolution,
) -> SessionVcsDiffBaseResolution {
    SessionVcsDiffBaseResolution {
        base_commit_sha: resolution.base_commit_sha,
        unavailable_reason: resolution.unavailable_reason,
        explicit_target: resolution.explicit_target,
        error: resolution.error,
    }
}

fn session_vcs_diff_summary_counts(
    counts: WorktreeVcsDiffSummaryCounts,
) -> SessionVcsDiffSummaryCounts {
    SessionVcsDiffSummaryCounts {
        file_count: counts.file_count,
        line_additions: counts.line_additions,
        line_deletions: counts.line_deletions,
    }
}

fn workspace_vcs_diff_summary_counts(
    counts: SessionVcsDiffSummaryCounts,
) -> WorktreeVcsDiffSummaryCounts {
    WorktreeVcsDiffSummaryCounts {
        file_count: counts.file_count,
        line_additions: counts.line_additions,
        line_deletions: counts.line_deletions,
    }
}

fn session_vcs_diff_summary_mismatch(
    snapshot_rev: i64,
    mismatch: WorktreeVcsDiffSummaryMismatch,
) -> SessionVcsDiffSummaryMismatch {
    SessionVcsDiffSummaryMismatch {
        snapshot_rev,
        snapshot_file_count: mismatch.snapshot_file_count,
        snapshot_line_additions: mismatch.snapshot_line_additions,
        snapshot_line_deletions: mismatch.snapshot_line_deletions,
        actual_file_count: mismatch.actual_file_count,
        actual_line_additions: mismatch.actual_line_additions,
        actual_line_deletions: mismatch.actual_line_deletions,
    }
}

fn session_vcs_git_status_snapshot(snapshot: GitStatusSnapshot) -> SessionVcsGitStatusSnapshot {
    SessionVcsGitStatusSnapshot {
        raw: snapshot.raw,
        summary_line: snapshot.summary_line,
        branch: snapshot.branch,
        upstream: snapshot.upstream,
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        detached: snapshot.detached,
        staged: snapshot.staged,
        unstaged: snapshot.unstaged,
        untracked: snapshot.untracked,
        entries: snapshot
            .entries
            .into_iter()
            .map(session_vcs_git_status_entry)
            .collect(),
        entries_total_count: snapshot.entries_total_count,
        entries_truncated: snapshot.entries_truncated,
    }
}

fn session_vcs_git_status_entry(entry: GitStatusEntry) -> SessionVcsGitStatusEntry {
    SessionVcsGitStatusEntry {
        path: entry.path,
        orig_path: entry.orig_path,
        index_status: entry.index_status,
        worktree_status: entry.worktree_status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDaemon;
    use ctx_core::models::{
        SessionGitStatusSummary, WorktreeVcsBaseResolution, WorktreeVcsBaseResolutionKind,
        WorktreeVcsComputeState, WorktreeVcsFreshness, WorktreeVcsGitStatusSummary,
        WorktreeVcsSummary, WorktreeVcsTouchedFiles, WorktreeVcsTouchedFilesState,
    };

    async fn seeded_daemon() -> (tempfile::TempDir, TestDaemon) {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon = TestDaemon::new_for_test(
            temp.path().join("data"),
            "http://127.0.0.1:4310".to_string(),
        )
        .await
        .expect("daemon");
        (temp, daemon)
    }

    fn git_summary() -> SessionGitStatusSummary {
        SessionGitStatusSummary {
            summary_line: "M file.rs".to_string(),
            branch: Some("main".to_string()),
            upstream: Some("origin/main".to_string()),
            ahead: 1,
            behind: 2,
            detached: false,
            staged: 3,
            unstaged: 4,
            untracked: 5,
        }
    }

    #[tokio::test]
    async fn load_session_vcs_parts_preserves_missing_and_deleting_not_found_mapping() {
        let (_temp, daemon) = seeded_daemon().await;
        let fixture = daemon
            .seed_cache_rehydration_session_for_test(true, true)
            .await
            .expect("fixture");
        let handle = daemon.session_vcs_handle_for_test();

        let loaded = handle
            .load_session_vcs_parts(fixture.session.id)
            .await
            .expect("load session parts")
            .expect("session parts");
        assert_eq!(loaded.0.id, fixture.session.id);
        assert_eq!(loaded.1.id, fixture.worktree.id);

        daemon
            .cache_rehydration_begin_workspace_delete_for_test(fixture.workspace.id)
            .await;
        let deleted = handle
            .load_session_vcs_parts(fixture.session.id)
            .await
            .expect("deleting workspace should not be internal");
        daemon
            .cache_rehydration_finish_workspace_delete_for_test(fixture.workspace.id)
            .await;
        assert!(deleted.is_none());

        let missing = handle
            .load_session_vcs_parts(SessionId::new())
            .await
            .expect("missing session should not be internal");
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn load_session_vcs_parts_hides_archived_subagents() {
        let (_temp, daemon) = seeded_daemon().await;
        let fixture = daemon
            .seed_cache_rehydration_primary_and_subagent_for_test()
            .await
            .expect("fixture");
        assert!(daemon
            .archive_task_lifecycle_subagent_session_for_test(
                fixture.workspace.id,
                fixture.primary.id,
                fixture.subagent.id,
            )
            .await
            .expect("archive subagent"));

        let loaded = daemon
            .session_vcs_handle_for_test()
            .load_session_vcs_parts(fixture.subagent.id)
            .await
            .expect("archived subagent should not be internal");
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn load_session_vcs_parts_reports_unavailable_store_as_internal_error() {
        let (_temp, daemon) = seeded_daemon().await;
        let fixture = daemon
            .seed_cache_rehydration_session_for_test(true, true)
            .await
            .expect("fixture");
        daemon
            .cache_rehydration_make_workspace_store_unopenable_for_test(fixture.workspace.id)
            .await
            .expect("make workspace store unopenable");

        let error = daemon
            .session_vcs_handle_for_test()
            .load_session_vcs_parts(fixture.session.id)
            .await
            .expect_err("unavailable store should stay internal");
        assert!(
            !error.to_string().contains("session not found"),
            "unavailable store must stay internal, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn git_status_summary_persistence_uses_session_vcs_write_lookup() {
        let (_temp, daemon) = seeded_daemon().await;
        let fixture = daemon
            .seed_cache_rehydration_primary_and_subagent_for_test()
            .await
            .expect("fixture");
        let handle = daemon.session_vcs_handle_for_test();
        let summary = git_summary();

        handle
            .persist_session_git_status_summary(fixture.primary.id, fixture.worktree.id, &summary)
            .await
            .expect("persist git summary");
        let store = daemon
            .store_for_session(fixture.primary.id)
            .await
            .expect("session store");
        let stored = store
            .get_session_git_status_summary(fixture.primary.id)
            .await
            .expect("stored summary")
            .expect("summary row");
        assert_eq!(stored.summary_line, summary.summary_line);
        assert_eq!(stored.branch, summary.branch);

        assert!(daemon
            .archive_task_lifecycle_subagent_session_for_test(
                fixture.workspace.id,
                fixture.primary.id,
                fixture.subagent.id,
            )
            .await
            .expect("archive subagent"));
        let error = handle
            .persist_session_git_status_summary(fixture.subagent.id, fixture.worktree.id, &summary)
            .await
            .expect_err("archived subagent write should be hidden");
        assert!(error.to_string().contains("session not found"));
    }

    #[tokio::test]
    async fn session_vcs_summary_mismatch_reads_cached_worktree_snapshot() {
        let (_temp, daemon) = seeded_daemon().await;
        let fixture = daemon
            .seed_cache_rehydration_session_for_test(true, true)
            .await
            .expect("fixture");
        daemon
            .cache_worktree_vcs_snapshot_for_test(ctx_core::models::WorktreeVcsSnapshot {
                worktree_id: fixture.worktree.id,
                rev: 7,
                emitted_at_ms: 1,
                base_commit_sha: "base".to_string(),
                head_commit_sha: "head".to_string(),
                target_branch: None,
                target_branch_commit_sha: None,
                base_resolution: WorktreeVcsBaseResolution {
                    kind: WorktreeVcsBaseResolutionKind::WorktreeBase,
                    target_source: None,
                    error: None,
                },
                compute_state: WorktreeVcsComputeState::Ready,
                summary: WorktreeVcsSummary {
                    file_count: Some(1),
                    line_additions: Some(2),
                    line_deletions: Some(3),
                    line_count: None,
                },
                git_status: WorktreeVcsGitStatusSummary::default(),
                touched_files: WorktreeVcsTouchedFiles::default(),
                touched_files_state: WorktreeVcsTouchedFilesState::NotLoaded,
                freshness: WorktreeVcsFreshness::Fresh,
                available: true,
                unavailable_reason: None,
                schema_version: 1,
            })
            .await;

        let mismatch = daemon
            .session_vcs_handle_for_test()
            .session_vcs_diff_summary_mismatch(
                &fixture.worktree,
                "base",
                SessionVcsDiffSummaryCounts {
                    file_count: 4,
                    line_additions: 5,
                    line_deletions: 6,
                },
            )
            .await
            .expect("mismatch should be detected");
        assert_eq!(mismatch.snapshot_rev, 7);
        assert_eq!(mismatch.snapshot_file_count, Some(1));
        assert_eq!(mismatch.actual_file_count, 4);
    }

    #[tokio::test]
    async fn session_vcs_compat_counter_effect_records_metric() {
        let (_temp, daemon) = seeded_daemon().await;
        let handle = daemon.session_vcs_handle_for_test();
        let before = daemon.telemetry_handle_for_test().perf_telemetry().stats();

        handle
            .emit_compat_payload_reject_counter("sessions.diff", "no_target_branch")
            .await;
        handle
            .emit_compat_payload_reject_counter("sessions.diff_summary", "no_target_branch")
            .await;
        handle
            .emit_compat_payload_reject_counter("sessions.diff_apply", "no_target_branch")
            .await;

        let after = daemon.telemetry_handle_for_test().perf_telemetry().stats();
        assert!(after.total_samples >= before.total_samples + 3);
    }
}

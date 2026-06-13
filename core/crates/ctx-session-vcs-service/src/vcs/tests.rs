use super::*;

use anyhow::{anyhow, bail};
use chrono::Utc;
use ctx_core::ids::{TaskId, WorkspaceId};
use ctx_core::models::{ExecutionEnvironment, SessionStatus};
use std::sync::Mutex;

#[derive(Default)]
struct FakeSessionVcsDataPlane {
    state: Mutex<FakeState>,
}

struct FakeState {
    session: Session,
    worktree: Worktree,
    has_repo: bool,
    diff_base_resolution: SessionVcsDiffBaseResolution,
    diff: String,
    diff_summary: SessionVcsDiffSummaryCounts,
    git_status: SessionVcsGitStatusSnapshot,
    persist_git_status_result: anyhow::Result<()>,
    patch_calls: Vec<bool>,
    compat_counters: Vec<(&'static str, &'static str)>,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            session: test_session(),
            worktree: test_worktree(),
            has_repo: true,
            diff_base_resolution: SessionVcsDiffBaseResolution {
                base_commit_sha: "base".to_string(),
                unavailable_reason: None,
                explicit_target: false,
                error: None,
            },
            diff: "diff --git a/file b/file".to_string(),
            diff_summary: SessionVcsDiffSummaryCounts {
                file_count: 1,
                line_additions: 2,
                line_deletions: 3,
            },
            git_status: SessionVcsGitStatusSnapshot {
                raw: "raw".to_string(),
                summary_line: "summary".to_string(),
                branch: Some("main".to_string()),
                upstream: Some("origin/main".to_string()),
                ahead: 1,
                behind: 2,
                detached: false,
                staged: 3,
                unstaged: 4,
                untracked: 5,
                entries: vec![SessionVcsGitStatusEntry {
                    path: "src/lib.rs".to_string(),
                    orig_path: None,
                    index_status: "M".to_string(),
                    worktree_status: " ".to_string(),
                }],
                entries_total_count: 1,
                entries_truncated: false,
            },
            persist_git_status_result: Ok(()),
            patch_calls: Vec::new(),
            compat_counters: Vec::new(),
        }
    }
}

#[async_trait]
impl SessionVcsDataPlane for FakeSessionVcsDataPlane {
    async fn load_session_vcs_parts(
        &self,
        _session_id: SessionId,
    ) -> anyhow::Result<Option<(Session, Worktree)>> {
        let state = self.state.lock().unwrap();
        Ok(Some((state.session.clone(), state.worktree.clone())))
    }

    async fn persist_session_git_status_summary(
        &self,
        _session_id: SessionId,
        _worktree_id: WorktreeId,
        _summary: &SessionGitStatusSummary,
    ) -> anyhow::Result<()> {
        let state = self.state.lock().unwrap();
        if state.persist_git_status_result.is_ok() {
            Ok(())
        } else {
            bail!("persist failed")
        }
    }

    async fn worktree_has_vcs_repo(&self, _worktree: &Worktree) -> anyhow::Result<bool> {
        Ok(self.state.lock().unwrap().has_repo)
    }

    async fn load_git_status_snapshot(
        &self,
        _worktree: &Worktree,
        _include_untracked_files: bool,
        _include_entries: bool,
    ) -> anyhow::Result<SessionVcsGitStatusSnapshot> {
        Ok(self.state.lock().unwrap().git_status.clone())
    }

    async fn resolve_worktree_commit(
        &self,
        _worktree: &Worktree,
        revision: &str,
    ) -> anyhow::Result<String> {
        Ok(format!("{revision}-sha"))
    }

    async fn diff_worktree_for_session(
        &self,
        _worktree: &Worktree,
        _base_commit_sha: &str,
    ) -> anyhow::Result<String> {
        Ok(self.state.lock().unwrap().diff.clone())
    }

    async fn diff_worktree_summary_for_session(
        &self,
        _worktree: &Worktree,
        _base_commit_sha: &str,
    ) -> anyhow::Result<SessionVcsDiffSummaryCounts> {
        Ok(self.state.lock().unwrap().diff_summary)
    }

    async fn resolve_worktree_diff_base(
        &self,
        _worktree: &Worktree,
        _query: SessionVcsDiffBaseQuery,
    ) -> SessionVcsDiffBaseResolution {
        self.state.lock().unwrap().diff_base_resolution.clone()
    }

    async fn apply_worktree_vcs_session_patch(
        &self,
        _worktree: &Worktree,
        _patch: &str,
        reverse_patch: bool,
    ) -> anyhow::Result<()> {
        self.state.lock().unwrap().patch_calls.push(reverse_patch);
        Ok(())
    }

    async fn session_vcs_diff_summary_mismatch(
        &self,
        _worktree: &Worktree,
        _base_commit_sha: &str,
        _counts: SessionVcsDiffSummaryCounts,
    ) -> Option<SessionVcsDiffSummaryMismatch> {
        None
    }

    async fn emit_compat_payload_reject_counter(&self, surface: &'static str, issue: &'static str) {
        self.state
            .lock()
            .unwrap()
            .compat_counters
            .push((surface, issue));
    }

    fn is_no_vcs_repo_error(&self, error: &Error) -> bool {
        error.to_string().contains("not a git repository")
    }
}

#[tokio::test]
async fn diff_returns_unavailable_without_repo() {
    let data_plane = FakeSessionVcsDataPlane::default();
    data_plane.state.lock().unwrap().has_repo = false;

    let diff = SessionVcsService::new(&data_plane)
        .get_session_vcs_diff(SessionId::new(), SessionVcsDiffQuery::default())
        .await
        .unwrap();

    assert_eq!(
        diff,
        SessionVcsDiff {
            diff: String::new(),
            available: false,
            unavailable_reason: Some(DiffUnavailableReason::NoRepo),
        }
    );
}

#[tokio::test]
async fn explicit_target_errors_are_bad_request_classification() {
    let data_plane = FakeSessionVcsDataPlane::default();
    data_plane.state.lock().unwrap().diff_base_resolution = SessionVcsDiffBaseResolution {
        base_commit_sha: "base".to_string(),
        unavailable_reason: None,
        explicit_target: true,
        error: Some("bad target".to_string()),
    };

    let error = SessionVcsService::new(&data_plane)
        .get_session_vcs_diff(SessionId::new(), SessionVcsDiffQuery::default())
        .await
        .unwrap_err();

    match error {
        SessionVcsError::InvalidExplicitTarget(message) => assert_eq!(message, "bad target"),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn git_status_persist_failure_does_not_fail_request() {
    let data_plane = FakeSessionVcsDataPlane::default();
    data_plane.state.lock().unwrap().persist_git_status_result = Err(anyhow!("persist failed"));

    let status = SessionVcsService::new(&data_plane)
        .get_session_vcs_git_status(SessionId::new())
        .await
        .unwrap();

    assert_eq!(status.summary_line, "summary");
    assert_eq!(status.entries.len(), 1);
}

#[tokio::test]
async fn reject_patch_uses_reverse_patch_and_refreshes_diff() {
    let data_plane = FakeSessionVcsDataPlane::default();

    let diff = SessionVcsService::new(&data_plane)
        .apply_session_vcs_diff_patch(
            SessionId::new(),
            SessionVcsApplyAction::Reject,
            "diff --git a/file b/file",
        )
        .await
        .unwrap();

    assert_eq!(diff.diff, "diff --git a/file b/file");
    assert_eq!(data_plane.state.lock().unwrap().patch_calls, vec![true]);
}

#[tokio::test]
async fn no_target_branch_requests_compat_counter_for_diff_summary_and_apply() {
    let data_plane = FakeSessionVcsDataPlane::default();
    data_plane.state.lock().unwrap().diff_base_resolution = SessionVcsDiffBaseResolution {
        base_commit_sha: "base".to_string(),
        unavailable_reason: Some(DiffUnavailableReason::NoTargetBranch),
        explicit_target: false,
        error: None,
    };

    let service = SessionVcsService::new(&data_plane);
    let diff = service
        .get_session_vcs_diff(SessionId::new(), SessionVcsDiffQuery::default())
        .await
        .unwrap();
    assert_eq!(
        diff.unavailable_reason,
        Some(DiffUnavailableReason::NoTargetBranch)
    );

    let summary = service
        .get_session_vcs_diff_summary(SessionId::new(), SessionVcsDiffQuery::default())
        .await
        .unwrap();
    assert_eq!(
        summary.unavailable_reason,
        Some(DiffUnavailableReason::NoTargetBranch)
    );

    let applied = service
        .apply_session_vcs_diff_patch(
            SessionId::new(),
            SessionVcsApplyAction::Accept,
            "diff --git a/file b/file",
        )
        .await
        .unwrap();
    assert_eq!(
        applied.unavailable_reason,
        Some(DiffUnavailableReason::NoTargetBranch)
    );

    assert_eq!(
        data_plane.state.lock().unwrap().compat_counters,
        vec![
            ("sessions.diff", "no_target_branch"),
            ("sessions.diff_summary", "no_target_branch"),
            ("sessions.diff_apply", "no_target_branch"),
        ]
    );
}

fn test_session() -> Session {
    let now = Utc::now();
    Session {
        id: SessionId::new(),
        task_id: TaskId::new(),
        workspace_id: WorkspaceId::new(),
        worktree_id: WorktreeId::new(),
        execution_environment: ExecutionEnvironment::Host,
        parent_session_id: None,
        relationship: None,
        provider_id: "fake".to_string(),
        model_id: "model".to_string(),
        reasoning_effort: None,
        title: "session".to_string(),
        agent_role: "agent".to_string(),
        status: SessionStatus::Active,
        provider_session_ref: None,
        created_at: now,
        updated_at: now,
    }
}

fn test_worktree() -> Worktree {
    Worktree {
        id: WorktreeId::new(),
        workspace_id: WorkspaceId::new(),
        root_path: "/tmp/worktree".to_string(),
        base_commit_sha: "base".to_string(),
        git_branch: Some("main".to_string()),
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
    }
}

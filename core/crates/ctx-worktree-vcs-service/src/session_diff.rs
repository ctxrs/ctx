use ctx_core::models::{DiffUnavailableReason, WorktreeVcsComputeState, WorktreeVcsSnapshot};

use super::WorktreeVcsDiffSummaryCounts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeVcsSessionDiffOutcome {
    pub diff: String,
    pub available: bool,
    pub unavailable_reason: Option<DiffUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeVcsSessionDiffSummaryOutcome {
    pub base_commit_sha: String,
    pub head_commit_sha: String,
    pub counts: WorktreeVcsDiffSummaryCounts,
    pub available: bool,
    pub unavailable_reason: Option<DiffUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeVcsDiffSummaryMismatch {
    pub snapshot_file_count: Option<i64>,
    pub snapshot_line_additions: Option<i64>,
    pub snapshot_line_deletions: Option<i64>,
    pub actual_file_count: i64,
    pub actual_line_additions: i64,
    pub actual_line_deletions: i64,
}

pub fn worktree_vcs_session_diff_available(diff: String) -> WorktreeVcsSessionDiffOutcome {
    WorktreeVcsSessionDiffOutcome {
        diff,
        available: true,
        unavailable_reason: None,
    }
}

pub fn worktree_vcs_session_diff_unavailable(
    reason: DiffUnavailableReason,
) -> WorktreeVcsSessionDiffOutcome {
    WorktreeVcsSessionDiffOutcome {
        diff: String::new(),
        available: false,
        unavailable_reason: Some(reason),
    }
}

pub fn worktree_vcs_session_diff_summary_available(
    base_commit_sha: String,
    head_commit_sha: String,
    counts: WorktreeVcsDiffSummaryCounts,
) -> WorktreeVcsSessionDiffSummaryOutcome {
    WorktreeVcsSessionDiffSummaryOutcome {
        base_commit_sha,
        head_commit_sha,
        counts,
        available: true,
        unavailable_reason: None,
    }
}

pub fn worktree_vcs_session_diff_summary_unavailable(
    base_commit_sha: String,
    head_commit_sha: String,
    reason: DiffUnavailableReason,
) -> WorktreeVcsSessionDiffSummaryOutcome {
    WorktreeVcsSessionDiffSummaryOutcome {
        base_commit_sha,
        head_commit_sha,
        counts: WorktreeVcsDiffSummaryCounts::default(),
        available: false,
        unavailable_reason: Some(reason),
    }
}

pub fn worktree_vcs_session_diff_summary_no_repo(
    base_commit_sha: String,
) -> WorktreeVcsSessionDiffSummaryOutcome {
    worktree_vcs_session_diff_summary_unavailable(
        base_commit_sha.clone(),
        base_commit_sha,
        DiffUnavailableReason::NoRepo,
    )
}

pub fn worktree_vcs_diff_summary_mismatch(
    snapshot: &WorktreeVcsSnapshot,
    base_commit_sha: &str,
    counts: WorktreeVcsDiffSummaryCounts,
) -> Option<WorktreeVcsDiffSummaryMismatch> {
    if snapshot.compute_state != WorktreeVcsComputeState::Ready
        || snapshot.base_commit_sha != base_commit_sha
    {
        return None;
    }

    let summary = &snapshot.summary;
    let mismatch = summary.file_count != Some(counts.file_count)
        || summary.line_additions != Some(counts.line_additions)
        || summary.line_deletions != Some(counts.line_deletions);
    mismatch.then_some(WorktreeVcsDiffSummaryMismatch {
        snapshot_file_count: summary.file_count,
        snapshot_line_additions: summary.line_additions,
        snapshot_line_deletions: summary.line_deletions,
        actual_file_count: counts.file_count,
        actual_line_additions: counts.line_additions,
        actual_line_deletions: counts.line_deletions,
    })
}

#[cfg(test)]
mod tests {
    use ctx_core::ids::WorktreeId;
    use ctx_core::models::WorktreeVcsSummary;

    use super::*;

    #[test]
    fn session_diff_outcomes_preserve_availability_shape() {
        assert_eq!(
            worktree_vcs_session_diff_available("patch".to_string()),
            WorktreeVcsSessionDiffOutcome {
                diff: "patch".to_string(),
                available: true,
                unavailable_reason: None,
            }
        );
        assert_eq!(
            worktree_vcs_session_diff_unavailable(DiffUnavailableReason::NoRepo),
            WorktreeVcsSessionDiffOutcome {
                diff: String::new(),
                available: false,
                unavailable_reason: Some(DiffUnavailableReason::NoRepo),
            }
        );
    }

    #[test]
    fn session_diff_summary_no_repo_uses_base_as_head_with_zero_counts() {
        assert_eq!(
            worktree_vcs_session_diff_summary_no_repo("base".to_string()),
            WorktreeVcsSessionDiffSummaryOutcome {
                base_commit_sha: "base".to_string(),
                head_commit_sha: "base".to_string(),
                counts: WorktreeVcsDiffSummaryCounts::default(),
                available: false,
                unavailable_reason: Some(DiffUnavailableReason::NoRepo),
            }
        );
    }

    #[test]
    fn diff_summary_mismatch_checks_ready_snapshot_for_same_base() {
        let counts = WorktreeVcsDiffSummaryCounts {
            file_count: 2,
            line_additions: 10,
            line_deletions: 1,
        };
        let mut snapshot = snapshot_with_summary("base", counts);

        assert_eq!(
            worktree_vcs_diff_summary_mismatch(&snapshot, "other-base", counts),
            None
        );
        snapshot.compute_state = WorktreeVcsComputeState::Computing;
        assert_eq!(
            worktree_vcs_diff_summary_mismatch(&snapshot, "base", counts),
            None
        );
        snapshot.compute_state = WorktreeVcsComputeState::Ready;
        assert_eq!(
            worktree_vcs_diff_summary_mismatch(&snapshot, "base", counts),
            None
        );

        let actual = WorktreeVcsDiffSummaryCounts {
            file_count: 3,
            line_additions: 10,
            line_deletions: 1,
        };
        assert_eq!(
            worktree_vcs_diff_summary_mismatch(&snapshot, "base", actual),
            Some(WorktreeVcsDiffSummaryMismatch {
                snapshot_file_count: Some(2),
                snapshot_line_additions: Some(10),
                snapshot_line_deletions: Some(1),
                actual_file_count: 3,
                actual_line_additions: 10,
                actual_line_deletions: 1,
            })
        );
    }

    fn snapshot_with_summary(
        base_commit_sha: &str,
        counts: WorktreeVcsDiffSummaryCounts,
    ) -> WorktreeVcsSnapshot {
        WorktreeVcsSnapshot {
            worktree_id: WorktreeId::new(),
            rev: 1,
            emitted_at_ms: 0,
            base_commit_sha: base_commit_sha.to_string(),
            head_commit_sha: "head".to_string(),
            target_branch: None,
            target_branch_commit_sha: None,
            base_resolution: Default::default(),
            compute_state: WorktreeVcsComputeState::Ready,
            summary: WorktreeVcsSummary {
                file_count: Some(counts.file_count),
                line_additions: Some(counts.line_additions),
                line_deletions: Some(counts.line_deletions),
                line_count: None,
            },
            git_status: Default::default(),
            touched_files: Default::default(),
            touched_files_state: Default::default(),
            freshness: Default::default(),
            available: true,
            unavailable_reason: None,
            schema_version: 1,
        }
    }
}

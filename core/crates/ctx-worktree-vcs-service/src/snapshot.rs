use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use ctx_core::ids::WorktreeId;
use ctx_core::models::{
    DiffUnavailableReason, Worktree, WorktreeVcsBaseResolution, WorktreeVcsComputeState,
    WorktreeVcsFreshness, WorktreeVcsGitStatusSummary, WorktreeVcsSnapshot, WorktreeVcsSummary,
    WorktreeVcsTouchedFile, WorktreeVcsTouchedFiles, WorktreeVcsTouchedFilesState,
};

use super::{
    resolve_worktree_diff_base_from_source, resolve_worktree_vcs_commit_lookup_from_source,
    GitStatusEntry, GitStatusSnapshot, WorktreeDiffBaseResolution, WorktreeVcsCommitLookupSource,
    WorktreeVcsDiffBaseQuery, WorktreeVcsDiffBaseSource, WORKTREE_VCS_SNAPSHOT_SCHEMA_VERSION,
    WORKTREE_VCS_TOUCHED_FILES_CAP,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeVcsCommitLookup {
    Resolved(String),
    Head,
    TargetBranch(String),
    Missing,
}

#[derive(Clone, Debug)]
pub struct WorktreeVcsCommitInfoPlan {
    pub base_commit_sha: String,
    pub target_branch: Option<String>,
    pub base_resolution: WorktreeVcsBaseResolution,
    pub head_commit_sha: WorktreeVcsCommitLookup,
    pub target_branch_commit_sha: WorktreeVcsCommitLookup,
}

impl WorktreeVcsCommitInfoPlan {
    pub fn into_commit_info(
        self,
        head_commit_sha: String,
        target_branch_commit_sha: Option<String>,
    ) -> WorktreeVcsSnapshotCommitInfo {
        WorktreeVcsSnapshotCommitInfo {
            base_commit_sha: self.base_commit_sha,
            head_commit_sha,
            target_branch: self.target_branch,
            target_branch_commit_sha,
            base_resolution: self.base_resolution,
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorktreeVcsSnapshotCommitInfo {
    pub base_commit_sha: String,
    pub head_commit_sha: String,
    pub target_branch: Option<String>,
    pub target_branch_commit_sha: Option<String>,
    pub base_resolution: WorktreeVcsBaseResolution,
}

#[derive(Clone, Debug)]
pub struct WorktreeVcsSnapshotBuildParts {
    pub worktree_id: WorktreeId,
    pub commit_info: WorktreeVcsSnapshotCommitInfo,
    pub git_status: WorktreeVcsGitStatusSummary,
    pub touched_files: WorktreeVcsTouchedFiles,
    pub touched_files_state: WorktreeVcsTouchedFilesState,
    pub summary: WorktreeVcsSummary,
    pub compute_state: WorktreeVcsComputeState,
    pub available: bool,
    pub unavailable_reason: Option<DiffUnavailableReason>,
}

pub fn build_touched_files(entries: &[WorktreeVcsTouchedFile]) -> WorktreeVcsTouchedFiles {
    let total_count = entries.len() as i64;
    let truncated = entries.len() > WORKTREE_VCS_TOUCHED_FILES_CAP;
    let mut items = Vec::new();
    for entry in entries.iter().take(WORKTREE_VCS_TOUCHED_FILES_CAP) {
        items.push(entry.clone());
    }
    WorktreeVcsTouchedFiles {
        items,
        truncated,
        total_count: Some(total_count),
    }
}

pub fn build_large_change_set_touched_files(file_count: i64) -> WorktreeVcsTouchedFiles {
    WorktreeVcsTouchedFiles {
        items: Vec::new(),
        truncated: true,
        total_count: Some(file_count),
    }
}

pub fn build_git_status_entries(entries: &[GitStatusEntry]) -> Vec<WorktreeVcsTouchedFile> {
    let mut out = Vec::new();
    for entry in entries.iter().take(WORKTREE_VCS_TOUCHED_FILES_CAP) {
        out.push(WorktreeVcsTouchedFile {
            path: entry.path.clone(),
            orig_path: entry.orig_path.clone(),
            index_status: Some(entry.index_status.clone()),
            worktree_status: Some(entry.worktree_status.clone()),
        });
    }
    out
}

pub fn build_git_status_summary(
    snapshot: &GitStatusSnapshot,
    entries: Vec<WorktreeVcsTouchedFile>,
) -> WorktreeVcsGitStatusSummary {
    WorktreeVcsGitStatusSummary {
        raw: String::new(),
        summary_line: snapshot.summary_line.clone(),
        branch: snapshot.branch.clone(),
        upstream: snapshot.upstream.clone(),
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        detached: snapshot.detached,
        staged: snapshot.staged,
        unstaged: snapshot.unstaged,
        untracked: snapshot.untracked,
        entries,
    }
}

pub fn summary_from_file_count(file_count: i64) -> WorktreeVcsSummary {
    WorktreeVcsSummary {
        file_count: Some(file_count),
        line_additions: None,
        line_deletions: None,
        line_count: None,
    }
}

pub fn snapshot_for_durable_cache(snapshot: &WorktreeVcsSnapshot) -> WorktreeVcsSnapshot {
    let mut durable = snapshot.clone();
    durable.touched_files = WorktreeVcsTouchedFiles::default();
    durable.touched_files_state = WorktreeVcsTouchedFilesState::NotLoaded;
    durable.git_status.raw.clear();
    durable.git_status.entries.clear();
    durable
}

pub fn summary_has_counts(summary: &WorktreeVcsSummary) -> bool {
    summary.file_count.is_some()
        || summary.line_additions.is_some()
        || summary.line_deletions.is_some()
        || summary.line_count.is_some()
}

pub fn plan_worktree_vcs_commit_info(
    resolution: WorktreeDiffBaseResolution,
    unavailable_reason: Option<DiffUnavailableReason>,
) -> WorktreeVcsCommitInfoPlan {
    let base_commit_sha = resolution.base_commit_sha;
    let target_branch = resolution.target_branch;
    let resolved_head_commit_sha = resolution.head_commit_sha;
    let resolved_target_branch_commit_sha = resolution.target_branch_commit_sha;
    let base_resolution = WorktreeVcsBaseResolution {
        kind: resolution.kind,
        target_source: resolution.target_source,
        error: resolution.error,
    };

    if matches!(unavailable_reason, Some(DiffUnavailableReason::NoRepo)) {
        return WorktreeVcsCommitInfoPlan {
            head_commit_sha: WorktreeVcsCommitLookup::Resolved(base_commit_sha.clone()),
            base_commit_sha,
            target_branch,
            target_branch_commit_sha: WorktreeVcsCommitLookup::Missing,
            base_resolution,
        };
    }

    let allow_live_target_lookup = unavailable_reason.is_none();
    let head_commit_sha = resolved_head_commit_sha
        .map(WorktreeVcsCommitLookup::Resolved)
        .unwrap_or(WorktreeVcsCommitLookup::Head);
    let target_branch_commit_sha = match (
        resolved_target_branch_commit_sha,
        target_branch.clone(),
        allow_live_target_lookup,
    ) {
        (Some(commit), _, _) => WorktreeVcsCommitLookup::Resolved(commit),
        (None, Some(target_branch), true) => WorktreeVcsCommitLookup::TargetBranch(target_branch),
        (None, _, _) => WorktreeVcsCommitLookup::Missing,
    };

    WorktreeVcsCommitInfoPlan {
        base_commit_sha,
        target_branch,
        base_resolution,
        head_commit_sha,
        target_branch_commit_sha,
    }
}

pub fn derive_worktree_vcs_freshness(
    compute_state: &WorktreeVcsComputeState,
    summary: &WorktreeVcsSummary,
) -> WorktreeVcsFreshness {
    match compute_state {
        WorktreeVcsComputeState::Ready => WorktreeVcsFreshness::Fresh,
        WorktreeVcsComputeState::Error => WorktreeVcsFreshness::Error,
        WorktreeVcsComputeState::Computing => {
            if summary_has_counts(summary) {
                WorktreeVcsFreshness::Stale
            } else {
                WorktreeVcsFreshness::Refreshing
            }
        }
    }
}

pub fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

pub fn snapshot_fingerprint(snapshot: &WorktreeVcsSnapshot) -> String {
    let mut copy = snapshot.clone();
    copy.rev = 0;
    copy.emitted_at_ms = 0;
    serde_json::to_string(&copy).unwrap_or_default()
}

pub fn build_worktree_vcs_snapshot(parts: WorktreeVcsSnapshotBuildParts) -> WorktreeVcsSnapshot {
    let freshness = derive_worktree_vcs_freshness(&parts.compute_state, &parts.summary);
    WorktreeVcsSnapshot {
        worktree_id: parts.worktree_id,
        rev: 0,
        emitted_at_ms: 0,
        base_commit_sha: parts.commit_info.base_commit_sha,
        head_commit_sha: parts.commit_info.head_commit_sha,
        target_branch: parts.commit_info.target_branch,
        target_branch_commit_sha: parts.commit_info.target_branch_commit_sha,
        base_resolution: parts.commit_info.base_resolution,
        compute_state: parts.compute_state,
        summary: parts.summary,
        git_status: parts.git_status,
        touched_files: parts.touched_files,
        touched_files_state: parts.touched_files_state,
        freshness,
        available: parts.available,
        unavailable_reason: parts.unavailable_reason,
        schema_version: WORKTREE_VCS_SNAPSHOT_SCHEMA_VERSION,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn build_worktree_vcs_snapshot_from_source<S>(
    source: &S,
    worktree: &Worktree,
    git_status: WorktreeVcsGitStatusSummary,
    touched_files: WorktreeVcsTouchedFiles,
    touched_files_state: WorktreeVcsTouchedFilesState,
    summary: WorktreeVcsSummary,
    compute_state: WorktreeVcsComputeState,
    resolution: Option<WorktreeDiffBaseResolution>,
    available: bool,
    unavailable_reason: Option<DiffUnavailableReason>,
) -> Result<WorktreeVcsSnapshot>
where
    S: WorktreeVcsDiffBaseSource + WorktreeVcsCommitLookupSource,
{
    let resolution = match resolution {
        Some(resolution) => resolution,
        None => {
            resolve_worktree_diff_base_from_source(
                source,
                worktree,
                WorktreeVcsDiffBaseQuery::default(),
            )
            .await
        }
    };
    let commit_plan = plan_worktree_vcs_commit_info(resolution, unavailable_reason.clone());
    let head_commit_sha =
        resolve_worktree_vcs_commit_lookup_from_source(source, &commit_plan.head_commit_sha)
            .await?
            .ok_or_else(|| anyhow::anyhow!("worktree vcs head commit lookup was missing"))?;
    let target_branch_commit_sha = resolve_worktree_vcs_commit_lookup_from_source(
        source,
        &commit_plan.target_branch_commit_sha,
    )
    .await?;
    let commit_info = commit_plan.into_commit_info(head_commit_sha, target_branch_commit_sha);
    Ok(build_worktree_vcs_snapshot(WorktreeVcsSnapshotBuildParts {
        worktree_id: worktree.id,
        commit_info,
        compute_state,
        summary,
        git_status,
        touched_files,
        touched_files_state,
        available,
        unavailable_reason,
    }))
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use chrono::Utc;
    use ctx_core::ids::WorkspaceId;
    use ctx_core::models::{Worktree, WorktreeVcsBaseResolutionKind};

    use super::super::WORKTREE_VCS_REVIEWABLE_FILE_LIMIT;
    use super::*;

    #[test]
    fn large_change_set_touched_files_stays_truthful_without_rows() {
        let touched_files =
            build_large_change_set_touched_files(WORKTREE_VCS_REVIEWABLE_FILE_LIMIT + 1);

        assert!(touched_files.items.is_empty());
        assert!(touched_files.truncated);
        assert_eq!(
            touched_files.total_count,
            Some(WORKTREE_VCS_REVIEWABLE_FILE_LIMIT + 1)
        );
    }

    #[test]
    fn build_snapshot_derives_schema_and_freshness() {
        let worktree_id = WorktreeId::new();

        let snapshot = build_worktree_vcs_snapshot(WorktreeVcsSnapshotBuildParts {
            worktree_id,
            commit_info: WorktreeVcsSnapshotCommitInfo {
                base_commit_sha: "base".to_string(),
                head_commit_sha: "head".to_string(),
                target_branch: Some("main".to_string()),
                target_branch_commit_sha: Some("target".to_string()),
                base_resolution: WorktreeVcsBaseResolution::default(),
            },
            git_status: WorktreeVcsGitStatusSummary::default(),
            touched_files: WorktreeVcsTouchedFiles::default(),
            touched_files_state: WorktreeVcsTouchedFilesState::Ready,
            summary: WorktreeVcsSummary {
                file_count: Some(2),
                ..Default::default()
            },
            compute_state: WorktreeVcsComputeState::Computing,
            available: true,
            unavailable_reason: None,
        });

        assert_eq!(snapshot.worktree_id, worktree_id);
        assert_eq!(snapshot.base_commit_sha, "base");
        assert_eq!(snapshot.head_commit_sha, "head");
        assert_eq!(snapshot.target_branch.as_deref(), Some("main"));
        assert_eq!(snapshot.target_branch_commit_sha.as_deref(), Some("target"));
        assert_eq!(snapshot.freshness, WorktreeVcsFreshness::Stale);
        assert_eq!(
            snapshot.schema_version,
            WORKTREE_VCS_SNAPSHOT_SCHEMA_VERSION
        );
        assert_eq!(snapshot.rev, 0);
        assert_eq!(snapshot.emitted_at_ms, 0);
    }

    #[test]
    fn commit_info_plan_uses_base_as_head_for_no_repo() {
        let plan = plan_worktree_vcs_commit_info(
            WorktreeDiffBaseResolution {
                base_commit_sha: "base".to_string(),
                head_commit_sha: None,
                target_branch: Some("main".to_string()),
                target_branch_commit_sha: None,
                target_source: None,
                kind: Default::default(),
                error: Some("not a repo".to_string()),
                unavailable_reason: Some(DiffUnavailableReason::NoRepo),
                explicit_target: false,
            },
            Some(DiffUnavailableReason::NoRepo),
        );

        assert_eq!(
            plan.head_commit_sha,
            WorktreeVcsCommitLookup::Resolved("base".to_string())
        );
        assert_eq!(
            plan.target_branch_commit_sha,
            WorktreeVcsCommitLookup::Missing
        );
    }

    #[test]
    fn commit_info_plan_requests_live_target_only_when_available() {
        let plan = plan_worktree_vcs_commit_info(
            WorktreeDiffBaseResolution {
                base_commit_sha: "base".to_string(),
                head_commit_sha: Some("head".to_string()),
                target_branch: Some("main".to_string()),
                target_branch_commit_sha: None,
                target_source: None,
                kind: Default::default(),
                error: None,
                unavailable_reason: None,
                explicit_target: false,
            },
            None,
        );

        assert_eq!(
            plan.head_commit_sha,
            WorktreeVcsCommitLookup::Resolved("head".to_string())
        );
        assert_eq!(
            plan.target_branch_commit_sha,
            WorktreeVcsCommitLookup::TargetBranch("main".to_string())
        );
    }

    #[test]
    fn commit_info_plan_skips_live_target_for_unavailable_diff() {
        let plan = plan_worktree_vcs_commit_info(
            WorktreeDiffBaseResolution {
                base_commit_sha: "base".to_string(),
                head_commit_sha: Some("head".to_string()),
                target_branch: Some("main".to_string()),
                target_branch_commit_sha: None,
                target_source: None,
                kind: Default::default(),
                error: Some("missing target".to_string()),
                unavailable_reason: Some(DiffUnavailableReason::NoTargetBranch),
                explicit_target: true,
            },
            Some(DiffUnavailableReason::NoTargetBranch),
        );

        assert_eq!(
            plan.target_branch_commit_sha,
            WorktreeVcsCommitLookup::Missing
        );
    }

    #[test]
    fn snapshot_fingerprint_ignores_transient_revision_fields() {
        let mut snapshot = build_worktree_vcs_snapshot(WorktreeVcsSnapshotBuildParts {
            worktree_id: WorktreeId::new(),
            commit_info: WorktreeVcsSnapshotCommitInfo {
                base_commit_sha: "base".to_string(),
                head_commit_sha: "head".to_string(),
                target_branch: None,
                target_branch_commit_sha: None,
                base_resolution: WorktreeVcsBaseResolution::default(),
            },
            git_status: WorktreeVcsGitStatusSummary::default(),
            touched_files: WorktreeVcsTouchedFiles::default(),
            touched_files_state: WorktreeVcsTouchedFilesState::NotLoaded,
            summary: WorktreeVcsSummary::default(),
            compute_state: WorktreeVcsComputeState::Ready,
            available: true,
            unavailable_reason: None,
        });
        let first = snapshot_fingerprint(&snapshot);

        snapshot.rev = 42;
        snapshot.emitted_at_ms = 1234;
        let same = snapshot_fingerprint(&snapshot);
        snapshot.head_commit_sha = "new-head".to_string();
        let changed = snapshot_fingerprint(&snapshot);

        assert_eq!(first, same);
        assert_ne!(same, changed);
    }

    #[tokio::test]
    async fn build_snapshot_from_source_resolves_planned_live_commits() {
        let worktree = test_worktree();
        let snapshot = build_worktree_vcs_snapshot_from_source(
            &FakeSnapshotSource,
            &worktree,
            WorktreeVcsGitStatusSummary::default(),
            WorktreeVcsTouchedFiles::default(),
            WorktreeVcsTouchedFilesState::Ready,
            WorktreeVcsSummary {
                file_count: Some(2),
                ..Default::default()
            },
            WorktreeVcsComputeState::Ready,
            Some(WorktreeDiffBaseResolution {
                base_commit_sha: "base".to_string(),
                head_commit_sha: None,
                target_branch: Some("main".to_string()),
                target_branch_commit_sha: None,
                target_source: None,
                kind: WorktreeVcsBaseResolutionKind::WorktreeBase,
                error: None,
                unavailable_reason: None,
                explicit_target: false,
            }),
            true,
            None,
        )
        .await
        .unwrap();

        assert_eq!(snapshot.worktree_id, worktree.id);
        assert_eq!(snapshot.base_commit_sha, "base");
        assert_eq!(snapshot.head_commit_sha, "resolved-HEAD");
        assert_eq!(snapshot.target_branch.as_deref(), Some("main"));
        assert_eq!(
            snapshot.target_branch_commit_sha.as_deref(),
            Some("resolved-main")
        );
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

    struct FakeSnapshotSource;

    #[async_trait::async_trait]
    impl WorktreeVcsCommitLookupSource for FakeSnapshotSource {
        async fn resolve_commit(&self, reference: &str) -> Result<String> {
            Ok(format!("resolved-{reference}"))
        }
    }

    #[async_trait::async_trait]
    impl WorktreeVcsDiffBaseSource for FakeSnapshotSource {
        async fn load_primary_branch(&self) -> Result<Option<String>> {
            Ok(Some("main".to_string()))
        }

        async fn rev_parse_head(&self) -> Result<String> {
            self.resolve_commit("HEAD").await
        }

        async fn rev_parse_refs(&self, references: &[&str]) -> Result<Vec<String>> {
            Ok(references
                .iter()
                .map(|reference| format!("resolved-{reference}"))
                .collect())
        }

        async fn merge_base(&self, target_branch: &str) -> Result<String> {
            Ok(format!("merge-base-{target_branch}"))
        }
    }
}

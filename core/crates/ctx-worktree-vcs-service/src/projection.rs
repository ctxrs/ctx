use ctx_core::models::{
    DiffUnavailableReason, WorktreeVcsComputeState, WorktreeVcsSnapshot, WorktreeVcsSummary,
    WorktreeVcsTouchedFile, WorktreeVcsTouchedFiles, WorktreeVcsTouchedFilesState,
};

use super::{
    build_large_change_set_touched_files, build_touched_files, summary_from_file_count,
    summary_has_counts, WORKTREE_VCS_REVIEWABLE_FILE_LIMIT,
};

#[derive(Clone, Debug)]
pub struct WorktreeVcsProjectionCacheState {
    pub summary: WorktreeVcsSummary,
    pub touched_files: WorktreeVcsTouchedFiles,
    pub touched_files_state: WorktreeVcsTouchedFilesState,
    pub available: bool,
    pub unavailable_reason: Option<DiffUnavailableReason>,
}

impl Default for WorktreeVcsProjectionCacheState {
    fn default() -> Self {
        Self {
            summary: WorktreeVcsSummary::default(),
            touched_files: WorktreeVcsTouchedFiles::default(),
            touched_files_state: WorktreeVcsTouchedFilesState::default(),
            available: true,
            unavailable_reason: None,
        }
    }
}

pub fn worktree_vcs_projection_cache_state(
    snapshot: Option<&WorktreeVcsSnapshot>,
) -> WorktreeVcsProjectionCacheState {
    let Some(snapshot) = snapshot else {
        return WorktreeVcsProjectionCacheState::default();
    };
    WorktreeVcsProjectionCacheState {
        summary: snapshot.summary.clone(),
        touched_files: snapshot.touched_files.clone(),
        touched_files_state: snapshot.touched_files_state.clone(),
        available: snapshot.available,
        unavailable_reason: snapshot.unavailable_reason.clone(),
    }
}

#[derive(Clone, Debug)]
pub struct WorktreeVcsSummaryRefreshResult {
    pub summary: WorktreeVcsSummary,
    pub compute_state: WorktreeVcsComputeState,
    pub available: bool,
    pub unavailable_reason: Option<DiffUnavailableReason>,
}

#[derive(Clone, Debug)]
pub enum WorktreeVcsSummaryRefreshPlan {
    LoadFileCount,
    Reuse(WorktreeVcsSummaryRefreshResult),
}

pub fn plan_worktree_vcs_summary_refresh(
    cache: &WorktreeVcsProjectionCacheState,
    refresh_summary: bool,
) -> WorktreeVcsSummaryRefreshPlan {
    if refresh_summary || !summary_has_counts(&cache.summary) {
        return WorktreeVcsSummaryRefreshPlan::LoadFileCount;
    }
    WorktreeVcsSummaryRefreshPlan::Reuse(WorktreeVcsSummaryRefreshResult {
        summary: cache.summary.clone(),
        compute_state: WorktreeVcsComputeState::Ready,
        available: cache.available,
        unavailable_reason: cache.unavailable_reason.clone(),
    })
}

pub fn worktree_vcs_summary_refresh_from_file_count(
    file_count: i64,
) -> WorktreeVcsSummaryRefreshResult {
    WorktreeVcsSummaryRefreshResult {
        summary: summary_from_file_count(file_count),
        compute_state: WorktreeVcsComputeState::Ready,
        available: true,
        unavailable_reason: None,
    }
}

pub fn worktree_vcs_summary_refresh_no_repo() -> WorktreeVcsSummaryRefreshResult {
    WorktreeVcsSummaryRefreshResult {
        summary: WorktreeVcsSummary::default(),
        compute_state: WorktreeVcsComputeState::Ready,
        available: false,
        unavailable_reason: Some(DiffUnavailableReason::NoRepo),
    }
}

pub fn worktree_vcs_summary_refresh_error_fallback(
    cache: &WorktreeVcsProjectionCacheState,
) -> WorktreeVcsSummaryRefreshResult {
    WorktreeVcsSummaryRefreshResult {
        summary: cache.summary.clone(),
        compute_state: WorktreeVcsComputeState::Error,
        available: cache.available,
        unavailable_reason: cache.unavailable_reason.clone(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeVcsTouchedFilesRefreshPlan {
    LargeChangeSet { file_count: i64 },
    LoadDiff,
    Reuse,
}

impl WorktreeVcsTouchedFilesRefreshPlan {
    pub fn include_status_inventory(self) -> bool {
        matches!(self, Self::LoadDiff)
    }
}

pub fn plan_worktree_vcs_touched_files_refresh(
    summary: &WorktreeVcsSummary,
    refresh_touched_files: bool,
) -> WorktreeVcsTouchedFilesRefreshPlan {
    if let Some(file_count) = summary
        .file_count
        .filter(|count| *count > WORKTREE_VCS_REVIEWABLE_FILE_LIMIT)
    {
        return WorktreeVcsTouchedFilesRefreshPlan::LargeChangeSet { file_count };
    }
    if refresh_touched_files {
        WorktreeVcsTouchedFilesRefreshPlan::LoadDiff
    } else {
        WorktreeVcsTouchedFilesRefreshPlan::Reuse
    }
}

#[derive(Clone, Debug)]
pub struct WorktreeVcsTouchedFilesRefreshResult {
    pub touched_files: WorktreeVcsTouchedFiles,
    pub touched_files_state: WorktreeVcsTouchedFilesState,
}

pub fn worktree_vcs_touched_files_large_change_set(
    file_count: i64,
) -> WorktreeVcsTouchedFilesRefreshResult {
    WorktreeVcsTouchedFilesRefreshResult {
        touched_files: build_large_change_set_touched_files(file_count),
        touched_files_state: WorktreeVcsTouchedFilesState::Ready,
    }
}

pub fn worktree_vcs_touched_files_from_entries(
    entries: &[WorktreeVcsTouchedFile],
) -> WorktreeVcsTouchedFilesRefreshResult {
    WorktreeVcsTouchedFilesRefreshResult {
        touched_files: build_touched_files(entries),
        touched_files_state: WorktreeVcsTouchedFilesState::Ready,
    }
}

pub fn worktree_vcs_touched_files_error_fallback(
    cache: &WorktreeVcsProjectionCacheState,
) -> WorktreeVcsTouchedFilesRefreshResult {
    WorktreeVcsTouchedFilesRefreshResult {
        touched_files: cache.touched_files.clone(),
        touched_files_state: WorktreeVcsTouchedFilesState::Error,
    }
}

pub fn worktree_vcs_touched_files_reuse(
    cache: &WorktreeVcsProjectionCacheState,
) -> WorktreeVcsTouchedFilesRefreshResult {
    let touched_files_state = match &cache.touched_files_state {
        WorktreeVcsTouchedFilesState::Loading => WorktreeVcsTouchedFilesState::NotLoaded,
        other => other.clone(),
    };
    WorktreeVcsTouchedFilesRefreshResult {
        touched_files: cache.touched_files.clone(),
        touched_files_state,
    }
}

pub fn worktree_vcs_refresh_transient_snapshot(
    mut snapshot: WorktreeVcsSnapshot,
    refresh_summary: bool,
    refresh_touched_files: bool,
) -> WorktreeVcsSnapshot {
    if refresh_summary {
        snapshot.compute_state = WorktreeVcsComputeState::Computing;
        snapshot.freshness = super::derive_worktree_vcs_freshness(
            &WorktreeVcsComputeState::Computing,
            &snapshot.summary,
        );
    }
    if refresh_touched_files {
        snapshot.touched_files_state = match snapshot.touched_files_state {
            WorktreeVcsTouchedFilesState::Ready => WorktreeVcsTouchedFilesState::Stale,
            WorktreeVcsTouchedFilesState::Stale => WorktreeVcsTouchedFilesState::Stale,
            _ => WorktreeVcsTouchedFilesState::Loading,
        };
    }
    snapshot
}

pub fn worktree_vcs_dirty_transient_snapshot(
    mut snapshot: WorktreeVcsSnapshot,
) -> WorktreeVcsSnapshot {
    snapshot.compute_state = WorktreeVcsComputeState::Computing;
    snapshot.freshness = super::derive_worktree_vcs_freshness(
        &WorktreeVcsComputeState::Computing,
        &snapshot.summary,
    );
    if matches!(
        snapshot.touched_files_state,
        WorktreeVcsTouchedFilesState::Ready
    ) {
        snapshot.touched_files_state = WorktreeVcsTouchedFilesState::Stale;
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use ctx_core::ids::WorktreeId;
    use ctx_core::models::{
        WorktreeVcsBaseResolution, WorktreeVcsFreshness, WorktreeVcsGitStatusSummary,
    };

    use super::*;

    #[test]
    fn cache_state_defaults_to_available_empty_snapshot() {
        let cache = worktree_vcs_projection_cache_state(None);

        assert!(cache.available);
        assert_eq!(cache.summary.file_count, None);
        assert_eq!(cache.summary.line_additions, None);
        assert_eq!(cache.summary.line_deletions, None);
        assert_eq!(cache.summary.line_count, None);
        assert_eq!(
            cache.touched_files_state,
            WorktreeVcsTouchedFilesState::NotLoaded
        );
        assert_eq!(cache.unavailable_reason, None);
    }

    #[test]
    fn summary_plan_reuses_cached_counts_when_not_forced() {
        let cache = WorktreeVcsProjectionCacheState {
            summary: WorktreeVcsSummary {
                file_count: Some(3),
                ..Default::default()
            },
            available: false,
            unavailable_reason: Some(DiffUnavailableReason::NoTargetBranch),
            ..Default::default()
        };

        let plan = plan_worktree_vcs_summary_refresh(&cache, false);

        let WorktreeVcsSummaryRefreshPlan::Reuse(result) = plan else {
            panic!("expected cached summary reuse");
        };
        assert_eq!(result.summary.file_count, Some(3));
        assert_eq!(result.compute_state, WorktreeVcsComputeState::Ready);
        assert!(!result.available);
        assert_eq!(
            result.unavailable_reason,
            Some(DiffUnavailableReason::NoTargetBranch)
        );
    }

    #[test]
    fn summary_plan_loads_when_counts_missing_or_forced() {
        let empty = WorktreeVcsProjectionCacheState::default();
        assert!(matches!(
            plan_worktree_vcs_summary_refresh(&empty, false),
            WorktreeVcsSummaryRefreshPlan::LoadFileCount
        ));

        let counted = WorktreeVcsProjectionCacheState {
            summary: WorktreeVcsSummary {
                file_count: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            plan_worktree_vcs_summary_refresh(&counted, true),
            WorktreeVcsSummaryRefreshPlan::LoadFileCount
        ));
    }

    #[test]
    fn summary_error_fallback_preserves_cached_availability() {
        let cache = WorktreeVcsProjectionCacheState {
            summary: WorktreeVcsSummary {
                file_count: Some(9),
                ..Default::default()
            },
            available: false,
            unavailable_reason: Some(DiffUnavailableReason::NoRepo),
            ..Default::default()
        };

        let result = worktree_vcs_summary_refresh_error_fallback(&cache);

        assert_eq!(result.summary.file_count, Some(9));
        assert_eq!(result.compute_state, WorktreeVcsComputeState::Error);
        assert!(!result.available);
        assert_eq!(
            result.unavailable_reason,
            Some(DiffUnavailableReason::NoRepo)
        );
    }

    #[test]
    fn touched_files_plan_uses_large_change_set_before_loading_diff() {
        let summary = WorktreeVcsSummary {
            file_count: Some(WORKTREE_VCS_REVIEWABLE_FILE_LIMIT + 1),
            ..Default::default()
        };

        let plan = plan_worktree_vcs_touched_files_refresh(&summary, true);

        assert_eq!(
            plan,
            WorktreeVcsTouchedFilesRefreshPlan::LargeChangeSet {
                file_count: WORKTREE_VCS_REVIEWABLE_FILE_LIMIT + 1,
            }
        );
        assert!(!plan.include_status_inventory());
    }

    #[test]
    fn touched_files_plan_loads_diff_only_when_requested() {
        let summary = WorktreeVcsSummary {
            file_count: Some(1),
            ..Default::default()
        };

        let plan = plan_worktree_vcs_touched_files_refresh(&summary, true);

        assert_eq!(plan, WorktreeVcsTouchedFilesRefreshPlan::LoadDiff);
        assert!(plan.include_status_inventory());
        assert_eq!(
            plan_worktree_vcs_touched_files_refresh(&summary, false),
            WorktreeVcsTouchedFilesRefreshPlan::Reuse
        );
    }

    #[test]
    fn touched_files_reuse_demotes_stale_loading_state() {
        let cache = WorktreeVcsProjectionCacheState {
            touched_files_state: WorktreeVcsTouchedFilesState::Loading,
            ..Default::default()
        };

        let result = worktree_vcs_touched_files_reuse(&cache);

        assert_eq!(
            result.touched_files_state,
            WorktreeVcsTouchedFilesState::NotLoaded
        );
    }

    #[test]
    fn cache_state_clones_snapshot_fields() {
        let snapshot = WorktreeVcsSnapshot {
            worktree_id: WorktreeId::new(),
            rev: 7,
            emitted_at_ms: 42,
            base_commit_sha: "base".to_string(),
            head_commit_sha: "head".to_string(),
            target_branch: None,
            target_branch_commit_sha: None,
            base_resolution: WorktreeVcsBaseResolution::default(),
            compute_state: WorktreeVcsComputeState::Ready,
            summary: WorktreeVcsSummary {
                file_count: Some(5),
                ..Default::default()
            },
            git_status: WorktreeVcsGitStatusSummary::default(),
            touched_files: WorktreeVcsTouchedFiles {
                total_count: Some(2),
                ..Default::default()
            },
            touched_files_state: WorktreeVcsTouchedFilesState::Ready,
            freshness: WorktreeVcsFreshness::Fresh,
            available: false,
            unavailable_reason: Some(DiffUnavailableReason::NoRepo),
            schema_version: 2,
        };

        let cache = worktree_vcs_projection_cache_state(Some(&snapshot));

        assert_eq!(cache.summary.file_count, Some(5));
        assert_eq!(cache.touched_files.total_count, Some(2));
        assert_eq!(
            cache.touched_files_state,
            WorktreeVcsTouchedFilesState::Ready
        );
        assert!(!cache.available);
        assert_eq!(
            cache.unavailable_reason,
            Some(DiffUnavailableReason::NoRepo)
        );
    }

    fn ready_snapshot() -> WorktreeVcsSnapshot {
        WorktreeVcsSnapshot {
            worktree_id: WorktreeId::new(),
            rev: 7,
            emitted_at_ms: 42,
            base_commit_sha: "base".to_string(),
            head_commit_sha: "head".to_string(),
            target_branch: None,
            target_branch_commit_sha: None,
            base_resolution: WorktreeVcsBaseResolution::default(),
            compute_state: WorktreeVcsComputeState::Ready,
            summary: WorktreeVcsSummary {
                file_count: Some(2),
                ..Default::default()
            },
            git_status: WorktreeVcsGitStatusSummary::default(),
            touched_files: WorktreeVcsTouchedFiles::default(),
            touched_files_state: WorktreeVcsTouchedFilesState::Ready,
            freshness: WorktreeVcsFreshness::Fresh,
            available: true,
            unavailable_reason: None,
            schema_version: 2,
        }
    }

    #[test]
    fn refresh_transient_snapshot_marks_requested_summary_and_touched_files() {
        let snapshot = worktree_vcs_refresh_transient_snapshot(ready_snapshot(), true, true);

        assert_eq!(snapshot.compute_state, WorktreeVcsComputeState::Computing);
        assert_eq!(snapshot.freshness, WorktreeVcsFreshness::Stale);
        assert_eq!(
            snapshot.touched_files_state,
            WorktreeVcsTouchedFilesState::Stale
        );
    }

    #[test]
    fn refresh_transient_snapshot_marks_uncached_touched_files_loading() {
        let mut initial = ready_snapshot();
        initial.summary = WorktreeVcsSummary::default();
        initial.touched_files_state = WorktreeVcsTouchedFilesState::NotLoaded;

        let snapshot = worktree_vcs_refresh_transient_snapshot(initial, true, true);

        assert_eq!(snapshot.compute_state, WorktreeVcsComputeState::Computing);
        assert_eq!(snapshot.freshness, WorktreeVcsFreshness::Refreshing);
        assert_eq!(
            snapshot.touched_files_state,
            WorktreeVcsTouchedFilesState::Loading
        );
    }

    #[test]
    fn dirty_transient_snapshot_only_stales_ready_touched_files() {
        let ready = worktree_vcs_dirty_transient_snapshot(ready_snapshot());

        assert_eq!(ready.compute_state, WorktreeVcsComputeState::Computing);
        assert_eq!(ready.freshness, WorktreeVcsFreshness::Stale);
        assert_eq!(
            ready.touched_files_state,
            WorktreeVcsTouchedFilesState::Stale
        );

        let mut not_loaded = ready_snapshot();
        not_loaded.touched_files_state = WorktreeVcsTouchedFilesState::NotLoaded;
        let not_loaded = worktree_vcs_dirty_transient_snapshot(not_loaded);
        assert_eq!(
            not_loaded.touched_files_state,
            WorktreeVcsTouchedFilesState::NotLoaded
        );
    }
}

use std::time::{Duration, Instant};

use ctx_core::models::{WorktreeVcsSnapshot, WorktreeVcsTouchedFilesState};

use super::{snapshot_fingerprint, WORKTREE_VCS_SNAPSHOT_SCHEMA_VERSION};

pub const WORKTREE_VCS_DEBOUNCE_MS: u64 = 500;
pub const WORKTREE_VCS_MAX_INTERVAL_MS: u64 = 2000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorktreeVcsSnapshotPublishPolicy {
    pub debounce: Duration,
    pub max_interval: Duration,
}

impl Default for WorktreeVcsSnapshotPublishPolicy {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(WORKTREE_VCS_DEBOUNCE_MS),
            max_interval: Duration::from_millis(WORKTREE_VCS_MAX_INTERVAL_MS),
        }
    }
}

pub struct WorktreeVcsSnapshotCacheEntry {
    pub snapshot: WorktreeVcsSnapshot,
    pub fingerprint: String,
    pub emitted_at: Instant,
    pub last_change_at: Instant,
    pub last_summary_at: Option<Instant>,
}

pub struct GitStatusSnapshotCacheEntry {
    pub payload: String,
    pub emitted_at: Instant,
    pub last_change_at: Instant,
}

pub fn published_worktree_vcs_snapshot_cache_entry(
    snapshot: WorktreeVcsSnapshot,
    now: Instant,
) -> WorktreeVcsSnapshotCacheEntry {
    WorktreeVcsSnapshotCacheEntry {
        fingerprint: serde_json::to_string(&snapshot).unwrap_or_default(),
        snapshot,
        emitted_at: now,
        last_change_at: now,
        last_summary_at: Some(now),
    }
}

pub fn hydrated_worktree_vcs_snapshot_cache_entry(
    snapshot: WorktreeVcsSnapshot,
    seed_instant: Instant,
) -> WorktreeVcsSnapshotCacheEntry {
    WorktreeVcsSnapshotCacheEntry {
        fingerprint: serde_json::to_string(&snapshot).unwrap_or_default(),
        snapshot,
        emitted_at: seed_instant,
        last_change_at: seed_instant,
        last_summary_at: None,
    }
}

pub fn pending_worktree_vcs_snapshot_cache_entry(
    snapshot: WorktreeVcsSnapshot,
    now: Instant,
    policy: WorktreeVcsSnapshotPublishPolicy,
) -> WorktreeVcsSnapshotCacheEntry {
    let stale_offset = policy.max_interval.saturating_add(Duration::from_millis(1));
    let stale_at = match now.checked_sub(stale_offset) {
        Some(value) => value,
        None => now,
    };
    WorktreeVcsSnapshotCacheEntry {
        snapshot,
        fingerprint: String::new(),
        emitted_at: stale_at,
        last_change_at: stale_at,
        last_summary_at: None,
    }
}

pub fn publish_worktree_vcs_snapshot_cache_entry(
    entry: &mut WorktreeVcsSnapshotCacheEntry,
    mut snapshot: WorktreeVcsSnapshot,
    now: Instant,
    force_emit: bool,
    summary_at: Option<Instant>,
    policy: WorktreeVcsSnapshotPublishPolicy,
) -> Option<WorktreeVcsSnapshot> {
    let fingerprint = snapshot_fingerprint(&snapshot);
    let is_first = entry.fingerprint.is_empty();
    if entry.fingerprint == fingerprint && !force_emit {
        return None;
    }
    let since_change = now.saturating_duration_since(entry.last_change_at);
    let since_emit = now.saturating_duration_since(entry.emitted_at);
    let previous_snapshot = &entry.snapshot;
    let must_publish_state_transition = previous_snapshot.available != snapshot.available
        || previous_snapshot.unavailable_reason != snapshot.unavailable_reason
        || previous_snapshot.compute_state != snapshot.compute_state
        || previous_snapshot.freshness != snapshot.freshness
        || previous_snapshot.touched_files_state != snapshot.touched_files_state
        || (matches!(
            snapshot.touched_files_state,
            WorktreeVcsTouchedFilesState::Ready
        ) && previous_snapshot.touched_files != snapshot.touched_files);
    if !force_emit
        && !is_first
        && !must_publish_state_transition
        && since_emit < policy.max_interval
        && since_change < policy.debounce
    {
        return None;
    }
    let next_rev = entry.snapshot.rev.saturating_add(1);
    snapshot.rev = next_rev;
    snapshot.emitted_at_ms = super::now_epoch_ms();
    if snapshot.schema_version == 0 {
        snapshot.schema_version = WORKTREE_VCS_SNAPSHOT_SCHEMA_VERSION;
    }
    entry.snapshot = snapshot.clone();
    entry.fingerprint = fingerprint;
    entry.emitted_at = now;
    entry.last_change_at = now;
    if let Some(summary_at) = summary_at {
        entry.last_summary_at = Some(summary_at);
    }
    Some(snapshot)
}

#[cfg(test)]
mod tests {
    use ctx_core::ids::WorktreeId;
    use ctx_core::models::{
        WorktreeVcsBaseResolution, WorktreeVcsComputeState, WorktreeVcsFreshness,
        WorktreeVcsGitStatusSummary, WorktreeVcsSummary, WorktreeVcsTouchedFiles,
    };

    use super::*;

    fn test_snapshot(state: WorktreeVcsComputeState) -> WorktreeVcsSnapshot {
        let freshness = match state {
            WorktreeVcsComputeState::Ready => WorktreeVcsFreshness::Fresh,
            WorktreeVcsComputeState::Computing => WorktreeVcsFreshness::Stale,
            WorktreeVcsComputeState::Error => WorktreeVcsFreshness::Error,
        };
        WorktreeVcsSnapshot {
            worktree_id: WorktreeId::new(),
            rev: 0,
            emitted_at_ms: 0,
            base_commit_sha: "base".to_string(),
            head_commit_sha: "head".to_string(),
            target_branch: None,
            target_branch_commit_sha: None,
            base_resolution: WorktreeVcsBaseResolution::default(),
            compute_state: state,
            summary: WorktreeVcsSummary {
                file_count: Some(1),
                ..Default::default()
            },
            git_status: WorktreeVcsGitStatusSummary::default(),
            touched_files: WorktreeVcsTouchedFiles::default(),
            touched_files_state: WorktreeVcsTouchedFilesState::NotLoaded,
            freshness,
            available: true,
            unavailable_reason: None,
            schema_version: WORKTREE_VCS_SNAPSHOT_SCHEMA_VERSION,
        }
    }

    #[test]
    fn pending_cache_entry_publishes_first_snapshot() {
        let now = Instant::now();
        let policy = WorktreeVcsSnapshotPublishPolicy::default();
        let snapshot = test_snapshot(WorktreeVcsComputeState::Ready);
        let mut entry = pending_worktree_vcs_snapshot_cache_entry(snapshot.clone(), now, policy);

        let published = publish_worktree_vcs_snapshot_cache_entry(
            &mut entry, snapshot, now, false, None, policy,
        );

        let published = match published {
            Some(value) => value,
            None => panic!("first pending snapshot should publish"),
        };
        assert_eq!(published.rev, 1);
        assert!(!entry.fingerprint.is_empty());
    }

    #[test]
    fn unchanged_snapshot_is_not_republished_without_force() {
        let now = Instant::now();
        let policy = WorktreeVcsSnapshotPublishPolicy::default();
        let snapshot = test_snapshot(WorktreeVcsComputeState::Ready);
        let mut entry = pending_worktree_vcs_snapshot_cache_entry(snapshot.clone(), now, policy);
        assert!(publish_worktree_vcs_snapshot_cache_entry(
            &mut entry,
            snapshot.clone(),
            now,
            false,
            None,
            policy,
        )
        .is_some());

        let republished = publish_worktree_vcs_snapshot_cache_entry(
            &mut entry,
            snapshot,
            now + policy.debounce,
            false,
            None,
            policy,
        );

        assert!(republished.is_none());
    }

    #[test]
    fn state_transition_publishes_inside_debounce_window() {
        let now = Instant::now();
        let policy = WorktreeVcsSnapshotPublishPolicy::default();
        let ready = test_snapshot(WorktreeVcsComputeState::Ready);
        let mut entry = pending_worktree_vcs_snapshot_cache_entry(ready.clone(), now, policy);
        assert!(publish_worktree_vcs_snapshot_cache_entry(
            &mut entry, ready, now, false, None, policy,
        )
        .is_some());

        let computing = test_snapshot(WorktreeVcsComputeState::Computing);
        let published = publish_worktree_vcs_snapshot_cache_entry(
            &mut entry,
            computing,
            now + Duration::from_millis(1),
            false,
            None,
            policy,
        );

        assert!(published.is_some());
    }
}

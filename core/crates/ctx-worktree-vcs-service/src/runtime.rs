use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ctx_core::ids::WorktreeId;
use ctx_core::models::{WorktreeVcsTouchedFiles, WorktreeVcsTouchedFilesState};
use tokio::sync::{Notify, Semaphore};

use super::{GitStatusSnapshot, DEFAULT_WORKTREE_VCS_SCHEDULER_CONCURRENCY};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorktreeVcsDirtyBits {
    pub worktree_fs: bool,
    pub vcs_meta: bool,
}

impl WorktreeVcsDirtyBits {
    pub fn any(self) -> bool {
        self.worktree_fs || self.vcs_meta
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorktreeVcsRuntimeState {
    pub generation: u64,
    pub dirty_bits: WorktreeVcsDirtyBits,
    pub pending_summary: bool,
    pub pending_touched_files: bool,
    pub running: bool,
    pub require_full_summary_rebuild: bool,
    pub summary_paths: HashSet<String>,
    pub candidate_paths: BTreeSet<String>,
    pub last_git_status: Option<GitStatusSnapshot>,
    pub touched_files: WorktreeVcsTouchedFiles,
    pub touched_files_state: WorktreeVcsTouchedFilesState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorktreeVcsSchedulerJob {
    pub worktree_id: WorktreeId,
    pub refresh_summary: bool,
    pub refresh_touched_files: bool,
}

#[derive(Clone)]
pub struct WorktreeVcsSchedulerRuntime {
    pub started: Arc<AtomicBool>,
    pub notify: Arc<Notify>,
    pub permits: Arc<Semaphore>,
}

impl WorktreeVcsSchedulerRuntime {
    pub fn with_concurrency(concurrency: usize) -> Self {
        Self {
            started: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorktreeVcsInvalidation {
    pub dirty_bits: WorktreeVcsDirtyBits,
    pub candidate_paths: BTreeSet<String>,
}

impl WorktreeVcsInvalidation {
    pub fn any(&self) -> bool {
        self.dirty_bits.any() || !self.candidate_paths.is_empty()
    }

    pub fn mark_vcs_meta(&mut self) {
        self.dirty_bits.vcs_meta = true;
    }

    pub fn mark_worktree_fs_path(&mut self, path: impl AsRef<str>) {
        let trimmed = path.as_ref().trim();
        if trimmed.is_empty() {
            return;
        }
        self.dirty_bits.worktree_fs = true;
        self.candidate_paths.insert(trimmed.to_string());
    }

    pub fn merge(&mut self, next: WorktreeVcsInvalidation) {
        self.dirty_bits.worktree_fs |= next.dirty_bits.worktree_fs;
        self.dirty_bits.vcs_meta |= next.dirty_bits.vcs_meta;
        self.candidate_paths.extend(next.candidate_paths);
    }

    pub fn into_parts(self) -> (WorktreeVcsDirtyBits, Vec<String>) {
        (
            self.dirty_bits,
            self.candidate_paths.into_iter().collect::<Vec<_>>(),
        )
    }
}

fn parse_worktree_vcs_enabled(raw: &str) -> Option<bool> {
    ctx_core::boolish::parse_boolish(raw).or_else(|| {
        match raw.trim().to_ascii_lowercase().as_str() {
            "enabled" => Some(true),
            "disabled" => Some(false),
            _ => None,
        }
    })
}

pub fn worktree_vcs_enabled_from_env() -> bool {
    for key in ["CTX_WORKTREE_VCS_ENABLED", "CTX_WORKTREE_VCS"] {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        if let Some(parsed) = parse_worktree_vcs_enabled(&value) {
            return parsed;
        }
        tracing::warn!(
            env_var = key,
            value = %value,
            "ignoring invalid worktree VCS mode; expected on/off, true/false, enabled/disabled, or 1/0"
        );
    }
    true
}

pub fn worktree_vcs_scheduler_concurrency_from_env() -> usize {
    std::env::var("CTX_WORKTREE_VCS_SCHEDULER_CONCURRENCY")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_WORKTREE_VCS_SCHEDULER_CONCURRENCY)
}

pub fn queue_worktree_vcs_refresh(
    entry: &mut WorktreeVcsRuntimeState,
    summary: bool,
    touched_files: bool,
) {
    entry.pending_summary |= summary;
    entry.pending_touched_files |= touched_files;
}

pub fn mark_worktree_vcs_runtime_dirty(
    entry: &mut WorktreeVcsRuntimeState,
    dirty_bits: WorktreeVcsDirtyBits,
    candidate_paths: impl IntoIterator<Item = String>,
    pane_open: bool,
) {
    entry.generation = entry.generation.saturating_add(1);
    entry.dirty_bits.worktree_fs |= dirty_bits.worktree_fs;
    entry.dirty_bits.vcs_meta |= dirty_bits.vcs_meta;
    entry.require_full_summary_rebuild |= dirty_bits.vcs_meta;
    for path in candidate_paths {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            entry.candidate_paths.insert(trimmed.to_string());
        }
    }
    entry.pending_summary = true;
    entry.pending_touched_files |= pane_open;
}

pub fn finish_worktree_vcs_refresh(
    entry: &mut WorktreeVcsRuntimeState,
    git_snapshot: GitStatusSnapshot,
    touched_files: WorktreeVcsTouchedFiles,
    touched_files_state: WorktreeVcsTouchedFilesState,
) {
    entry.last_git_status = Some(git_snapshot);
    entry.touched_files = touched_files;
    entry.touched_files_state = touched_files_state;
    entry.dirty_bits = WorktreeVcsDirtyBits::default();
    entry.require_full_summary_rebuild = false;
    entry.candidate_paths.clear();
}

pub fn finish_worktree_vcs_job(
    runtime: &mut HashMap<WorktreeId, WorktreeVcsRuntimeState>,
    worktree_id: WorktreeId,
) -> bool {
    match runtime.get_mut(&worktree_id) {
        Some(entry) => {
            entry.running = false;
            entry.pending_summary || entry.pending_touched_files
        }
        None => false,
    }
}

pub fn claim_next_worktree_vcs_job(
    runtime: &mut HashMap<WorktreeId, WorktreeVcsRuntimeState>,
    active: &HashMap<WorktreeId, usize>,
    open: &HashMap<WorktreeId, usize>,
) -> Option<WorktreeVcsSchedulerJob> {
    let mut selected: Option<(WorktreeId, u8)> = None;
    for (worktree_id, entry) in runtime.iter() {
        if entry.running || (!entry.pending_summary && !entry.pending_touched_files) {
            continue;
        }
        if active.get(worktree_id).copied().unwrap_or(0) == 0 {
            continue;
        }
        let pane_open = open.get(worktree_id).copied().unwrap_or(0) > 0;
        let priority = if entry.pending_touched_files && pane_open {
            0
        } else if entry.pending_summary && pane_open {
            1
        } else if entry.pending_summary {
            2
        } else {
            3
        };
        match selected {
            Some((current_id, current_priority))
                if current_priority < priority
                    || (current_priority == priority && current_id.0 <= worktree_id.0) => {}
            _ => selected = Some((*worktree_id, priority)),
        }
    }
    let (worktree_id, _) = selected?;
    let entry = runtime.get_mut(&worktree_id)?;
    let refresh_summary = entry.pending_summary;
    let refresh_touched_files = entry.pending_touched_files;
    entry.pending_summary = false;
    entry.pending_touched_files = false;
    entry.running = true;
    Some(WorktreeVcsSchedulerJob {
        worktree_id,
        refresh_summary,
        refresh_touched_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn scheduler_claim_prioritizes_open_touched_files() {
        let touched_worktree = WorktreeId::new();
        let summary_worktree = WorktreeId::new();
        let mut runtime = HashMap::from([
            (
                summary_worktree,
                WorktreeVcsRuntimeState {
                    pending_summary: true,
                    ..Default::default()
                },
            ),
            (
                touched_worktree,
                WorktreeVcsRuntimeState {
                    pending_touched_files: true,
                    ..Default::default()
                },
            ),
        ]);
        let active = HashMap::from([(summary_worktree, 1), (touched_worktree, 1)]);
        let open = HashMap::from([(touched_worktree, 1)]);

        let job = claim_next_worktree_vcs_job(&mut runtime, &active, &open).unwrap();

        assert_eq!(job.worktree_id, touched_worktree);
        assert!(!job.refresh_summary);
        assert!(job.refresh_touched_files);
        assert!(runtime.get(&touched_worktree).unwrap().running);
    }

    #[test]
    fn scheduler_claim_skips_inactive_worktrees() {
        let inactive_worktree = WorktreeId::new();
        let active_worktree = WorktreeId::new();
        let mut runtime = HashMap::from([
            (
                inactive_worktree,
                WorktreeVcsRuntimeState {
                    pending_touched_files: true,
                    ..Default::default()
                },
            ),
            (
                active_worktree,
                WorktreeVcsRuntimeState {
                    pending_summary: true,
                    ..Default::default()
                },
            ),
        ]);
        let active = HashMap::from([(active_worktree, 1)]);
        let open = HashMap::new();

        let job = claim_next_worktree_vcs_job(&mut runtime, &active, &open).unwrap();

        assert_eq!(job.worktree_id, active_worktree);
        assert!(job.refresh_summary);
        assert!(!runtime.get(&inactive_worktree).unwrap().running);
    }

    #[test]
    fn scheduler_finish_reports_pending_follow_up() {
        let worktree_id = WorktreeId::new();
        let mut runtime = HashMap::from([(
            worktree_id,
            WorktreeVcsRuntimeState {
                running: true,
                pending_summary: true,
                ..Default::default()
            },
        )]);

        assert!(finish_worktree_vcs_job(&mut runtime, worktree_id));
        let entry = runtime.get(&worktree_id).unwrap();
        assert!(!entry.running);
        assert!(entry.pending_summary);
    }

    #[test]
    fn invalidation_merge_deduplicates_and_trims_candidate_paths() {
        let mut first = WorktreeVcsInvalidation::default();
        first.mark_worktree_fs_path(" src/lib.rs ");
        let mut second = WorktreeVcsInvalidation::default();
        second.mark_worktree_fs_path("src/lib.rs");
        second.mark_vcs_meta();

        first.merge(second);
        let (dirty_bits, candidate_paths) = first.into_parts();

        assert!(dirty_bits.worktree_fs);
        assert!(dirty_bits.vcs_meta);
        assert_eq!(candidate_paths, vec!["src/lib.rs".to_string()]);
    }

    #[test]
    fn parses_worktree_vcs_mode_values() {
        for raw in ["1", "true", "yes", "on", "enabled"] {
            assert_eq!(parse_worktree_vcs_enabled(raw), Some(true), "raw={raw}");
        }
        for raw in ["0", "false", "no", "off", "disabled"] {
            assert_eq!(parse_worktree_vcs_enabled(raw), Some(false), "raw={raw}");
        }
        for raw in ["", "maybe", "summary-only"] {
            assert_eq!(parse_worktree_vcs_enabled(raw), None, "raw={raw}");
        }
    }
}

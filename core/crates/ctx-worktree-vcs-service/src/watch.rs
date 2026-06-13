use std::path::{Path, PathBuf};

use anyhow::Result;
use ctx_core::models::{VcsKind, Worktree};
use ctx_fs::patch::should_ignore_path;

use super::WorktreeVcsInvalidation;

pub const WORKTREE_VCS_WATCH_DEBOUNCE_MS: u64 = 500;
pub const WORKTREE_VCS_POLL_INTERVAL_MS: u64 = 60_000;

#[derive(Debug, Default)]
pub struct WorktreeVcsWatchDebounceState {
    invalidation: WorktreeVcsInvalidation,
    scheduled: bool,
}

impl WorktreeVcsWatchDebounceState {
    pub fn merge_invalidation(&mut self, invalidation: WorktreeVcsInvalidation) -> bool {
        if !invalidation.any() {
            return false;
        }
        self.invalidation.merge(invalidation);
        if self.scheduled {
            false
        } else {
            self.scheduled = true;
            true
        }
    }

    pub fn take_invalidation(&mut self) -> WorktreeVcsInvalidation {
        std::mem::take(&mut self.invalidation)
    }

    pub fn finish_dispatch_cycle(&mut self) -> bool {
        if self.invalidation.any() {
            true
        } else {
            self.scheduled = false;
            false
        }
    }
}

pub fn normalize_worktree_vcs_watch_path(path: &Path) -> PathBuf {
    let mut suffix = Vec::new();
    let mut cursor = path;
    loop {
        match std::fs::canonicalize(cursor) {
            Ok(canonical) => {
                let mut normalized = canonical;
                for component in suffix.iter().rev() {
                    normalized.push(component);
                }
                return normalized;
            }
            Err(_) => {
                let Some(parent) = cursor.parent() else {
                    return path.to_path_buf();
                };
                let Some(name) = cursor.file_name() else {
                    return path.to_path_buf();
                };
                suffix.push(name.to_os_string());
                cursor = parent;
            }
        }
    }
}

async fn resolve_git_dir(worktree_root: &Path) -> Result<PathBuf> {
    let dotgit = worktree_root.join(".git");
    let meta = tokio::fs::metadata(&dotgit).await?;
    if meta.is_dir() {
        return Ok(dotgit);
    }
    let txt = tokio::fs::read_to_string(&dotgit).await?;
    let line = txt
        .lines()
        .find(|l| l.trim_start().starts_with("gitdir:"))
        .ok_or_else(|| anyhow::anyhow!("invalid .git file: missing gitdir"))?;
    let raw = line.trim_start().trim_start_matches("gitdir:").trim();
    let path = PathBuf::from(raw);
    let resolved = if path.is_absolute() {
        path
    } else {
        worktree_root.join(path)
    };
    match tokio::fs::canonicalize(&resolved).await {
        Ok(path) => Ok(path),
        Err(_) => Ok(resolved),
    }
}

async fn resolve_common_git_dir(git_dir: &Path) -> Result<PathBuf> {
    let commondir = git_dir.join("commondir");
    let meta = match tokio::fs::symlink_metadata(&commondir).await {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(git_dir.to_path_buf());
        }
        Err(err) => return Err(err.into()),
    };
    if !meta.is_file() {
        return Ok(git_dir.to_path_buf());
    }
    let raw = tokio::fs::read_to_string(&commondir).await?;
    let path = PathBuf::from(raw.trim());
    let resolved = if path.is_absolute() {
        path
    } else {
        git_dir.join(path)
    };
    match tokio::fs::canonicalize(&resolved).await {
        Ok(path) => Ok(path),
        Err(_) => Ok(resolved),
    }
}

pub async fn resolve_worktree_vcs_metadata_roots(
    worktree: &Worktree,
    worktree_root: &Path,
) -> Result<Vec<PathBuf>> {
    match worktree.vcs_kind.clone().unwrap_or(VcsKind::Git) {
        VcsKind::Jj => Ok(vec![worktree_root.join(".jj")]),
        _ => {
            let git_dir = resolve_git_dir(worktree_root).await?;
            let common_git_dir = resolve_common_git_dir(&git_dir).await?;
            let mut roots = vec![git_dir];
            if !roots.contains(&common_git_dir) {
                roots.push(common_git_dir);
            }
            Ok(roots)
        }
    }
}

fn should_ignore_watch_path(
    normalized: &Path,
    worktree_root: &Path,
    metadata_roots: &[PathBuf],
) -> bool {
    if metadata_roots
        .iter()
        .any(|metadata_root| normalized.starts_with(metadata_root))
    {
        return false;
    }
    if let Ok(relative) = normalized.strip_prefix(worktree_root) {
        return !relative.as_os_str().is_empty() && should_ignore_path(relative);
    }
    should_ignore_path(normalized)
}

pub fn worktree_vcs_invalidation_for_watch_paths(
    paths: &[PathBuf],
    worktree_root: &Path,
    metadata_roots: &[PathBuf],
) -> WorktreeVcsInvalidation {
    if paths.iter().all(|path| {
        let normalized = normalize_worktree_vcs_watch_path(path);
        should_ignore_watch_path(&normalized, worktree_root, metadata_roots)
    }) {
        return WorktreeVcsInvalidation::default();
    }

    let mut invalidation = WorktreeVcsInvalidation::default();
    for path in paths {
        let normalized = normalize_worktree_vcs_watch_path(path);
        if metadata_roots
            .iter()
            .any(|metadata_root| normalized.starts_with(metadata_root))
        {
            invalidation.mark_vcs_meta();
            continue;
        }
        if let Ok(relative) = normalized.strip_prefix(worktree_root) {
            invalidation.mark_worktree_fs_path(relative.to_string_lossy());
        } else {
            invalidation.mark_vcs_meta();
        }
    }
    invalidation
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    #[cfg(unix)]
    fn metadata_event_survives_symlink_alias() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real_root = temp.path().join("real");
        std::fs::create_dir_all(real_root.join(".git/refs/heads")).expect("create repo dirs");
        let alias_root = temp.path().join("alias");
        std::os::unix::fs::symlink(&real_root, &alias_root).expect("create alias symlink");

        let worktree_root = normalize_worktree_vcs_watch_path(&real_root);
        let metadata_root = normalize_worktree_vcs_watch_path(&real_root.join(".git"));
        let invalidation = worktree_vcs_invalidation_for_watch_paths(
            &[alias_root.join(".git/refs/heads/merge-target")],
            &worktree_root,
            &[metadata_root],
        );

        assert!(invalidation.dirty_bits.vcs_meta);
        assert!(!invalidation.dirty_bits.worktree_fs);
    }

    #[test]
    fn managed_worktree_files_under_ctx_parent_still_invalidate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree_root = temp.path().join(".ctx/worktrees/workspace/task");
        std::fs::create_dir_all(worktree_root.join("src")).expect("create worktree");

        let worktree_root = normalize_worktree_vcs_watch_path(&worktree_root);
        let invalidation = worktree_vcs_invalidation_for_watch_paths(
            &[worktree_root.join("src/main.rs")],
            &worktree_root,
            &[],
        );

        assert!(invalidation.dirty_bits.worktree_fs);
        assert!(invalidation.candidate_paths.contains("src/main.rs"));
    }

    #[test]
    fn normalize_watch_path_preserves_missing_suffix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).expect("create root");
        let missing = root.join(".git/refs/heads/missing");

        let normalized = normalize_worktree_vcs_watch_path(&missing);

        assert!(
            normalized.ends_with(Path::new(".git/refs/heads/missing")),
            "missing suffix should be preserved after normalization",
        );
    }

    #[test]
    fn debounce_state_coalesces_and_preserves_pending_follow_up() {
        let mut state = WorktreeVcsWatchDebounceState::default();
        let mut first = WorktreeVcsInvalidation::default();
        first.mark_worktree_fs_path("src/lib.rs");

        assert!(state.merge_invalidation(first));

        let mut coalesced = WorktreeVcsInvalidation::default();
        coalesced.mark_vcs_meta();
        assert!(!state.merge_invalidation(coalesced));

        let dispatch = state.take_invalidation();
        assert!(dispatch.dirty_bits.worktree_fs);
        assert!(dispatch.dirty_bits.vcs_meta);
        assert!(dispatch.candidate_paths.contains("src/lib.rs"));
        assert!(!state.finish_dispatch_cycle());

        let mut next = WorktreeVcsInvalidation::default();
        next.mark_worktree_fs_path("src/main.rs");
        assert!(state.merge_invalidation(next));

        let _dispatch = state.take_invalidation();
        let mut follow_up = WorktreeVcsInvalidation::default();
        follow_up.mark_vcs_meta();
        assert!(!state.merge_invalidation(follow_up));

        assert!(state.finish_dispatch_cycle());
        let dispatch = state.take_invalidation();
        assert!(!dispatch.dirty_bits.worktree_fs);
        assert!(dispatch.dirty_bits.vcs_meta);
        assert!(!state.finish_dispatch_cycle());
    }
}

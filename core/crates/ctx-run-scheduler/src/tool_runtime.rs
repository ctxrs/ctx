use std::path::{Path, PathBuf};

pub fn cwd_outside_worktree(
    cwd: &str,
    workdir_root: &Path,
    workdir_canonical: Option<&PathBuf>,
) -> bool {
    if cwd.trim().is_empty() {
        return false;
    }
    let cwd_path = Path::new(cwd);
    if cwd_path.is_relative() {
        return false;
    }
    if cwd_path.starts_with(workdir_root) {
        return false;
    }
    if let Some(root) = workdir_canonical {
        if cwd_path.starts_with(root) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::cwd_outside_worktree;

    #[test]
    fn cwd_guardrail_allows_in_tree_and_relative_paths() {
        let worktree_root = PathBuf::from("/repo/worktree");
        let canonical_root = PathBuf::from("/private/repo/worktree");

        assert!(!cwd_outside_worktree(
            "",
            &worktree_root,
            Some(&canonical_root)
        ));
        assert!(!cwd_outside_worktree(
            "relative/path",
            &worktree_root,
            Some(&canonical_root)
        ));
        assert!(!cwd_outside_worktree(
            "/repo/worktree/subdir",
            &worktree_root,
            Some(&canonical_root)
        ));
        assert!(!cwd_outside_worktree(
            "/private/repo/worktree/subdir",
            &worktree_root,
            Some(&canonical_root)
        ));
    }

    #[test]
    fn cwd_guardrail_flags_absolute_paths_outside_worktree() {
        let worktree_root = PathBuf::from("/repo/worktree");
        assert!(cwd_outside_worktree("/tmp/outside", &worktree_root, None));
    }
}

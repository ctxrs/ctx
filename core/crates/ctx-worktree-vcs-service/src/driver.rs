use std::sync::Arc;

use ctx_core::models::VcsKind;

pub type WorktreeVcsDriver = dyn ctx_fs::vcs::VcsDriver;

pub fn worktree_vcs_driver_for_kind(kind: Option<VcsKind>) -> Arc<WorktreeVcsDriver> {
    ctx_fs::vcs::driver_for_kind(kind)
}

pub fn effective_worktree_vcs_kind(kind: Option<VcsKind>) -> VcsKind {
    worktree_vcs_driver_for_kind(kind).kind()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_worktree_vcs_kind_preserves_driver_for_kind_defaulting() {
        assert_eq!(effective_worktree_vcs_kind(Some(VcsKind::Jj)), VcsKind::Jj);
        assert_eq!(
            effective_worktree_vcs_kind(Some(VcsKind::Git)),
            VcsKind::Git
        );
        assert_eq!(effective_worktree_vcs_kind(Some(VcsKind::Hg)), VcsKind::Git);
        assert_eq!(effective_worktree_vcs_kind(None), VcsKind::Git);
    }
}

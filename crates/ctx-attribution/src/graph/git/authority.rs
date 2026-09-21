//! Work-owned Git authority carriers.

use std::path::PathBuf;

use crate::git_executable::GitExecutable;
use crate::query::{Citation, QueryError, ResourceId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::graph) struct RepositoryWorktreeIdentity {
    pub(in crate::graph) repository_id: String,
    pub(in crate::graph) worktree_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::graph) struct ExactGitBlameFile {
    pub(in crate::graph) relative_path: String,
    pub(in crate::graph) repository: RepositoryWorktreeIdentity,
    pub(in crate::graph) core_citation: Citation,
}

/// One latest persisted Core certification for live access to a repository root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::graph) struct CertifiedLiveRoot {
    pub(in crate::graph) worktree_resource_id: String,
    pub(in crate::graph) repository_resource_id: String,
    pub(in crate::graph) path: PathBuf,
    /// Persisted digest binding the root, Git directory, common directory, and
    /// object format observed by Core when this root was certified.
    pub(in crate::graph) security_geometry_fingerprint: String,
    pub(in crate::graph) observed_at_unix_ms: i64,
}

/// Exact storage facts required to authorize one bounded live Git blame.
///
/// Implementations expose no query surface or provider history. They resolve
/// only one file authority, the latest certified root candidates for its exact
/// repository/worktree identity, and exact-candidate revalidation.
pub(in crate::graph) trait GitBlameAuthority {
    fn authorized_git_executable(&self) -> Option<&GitExecutable>;

    fn exact_file_authority(&self, file_id: &ResourceId) -> Result<ExactGitBlameFile, QueryError>;

    fn certified_live_root_candidates(
        &self,
        repository: &RepositoryWorktreeIdentity,
    ) -> Result<Vec<CertifiedLiveRoot>, QueryError>;

    fn revalidate_certified_live_root(
        &self,
        authorization: &CertifiedLiveRoot,
    ) -> Result<(), QueryError>;
}

use anyhow::Result;
use ctx_core::models::SessionGitStatusSummary;
use ctx_fs::vcs::VcsStructuredStatus;

use super::{GitStatusEntry, GitStatusSnapshot, WorktreeVcsCommitLookup};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeVcsStructuredStatus {
    pub raw: String,
    pub summary_line: String,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub detached: bool,
    pub staged: i64,
    pub unstaged: i64,
    pub untracked: i64,
    pub entries: Vec<GitStatusEntry>,
    pub entries_total_count: i64,
    pub entries_truncated: bool,
}

#[async_trait::async_trait]
pub trait WorktreeVcsStatusSource: Send + Sync {
    async fn has_vcs_repo(&self) -> Result<bool>;

    async fn load_structured_status(
        &self,
        include_untracked_files: bool,
        include_entries: bool,
    ) -> Result<WorktreeVcsStructuredStatus>;
}

#[async_trait::async_trait]
pub trait WorktreeVcsCommitLookupSource: Send + Sync {
    async fn resolve_commit(&self, reference: &str) -> Result<String>;
}

pub async fn worktree_has_vcs_repo_from_source(
    source: &impl WorktreeVcsStatusSource,
) -> Result<bool> {
    source.has_vcs_repo().await
}

pub async fn load_git_status_snapshot_from_source(
    source: &impl WorktreeVcsStatusSource,
    include_untracked_files: bool,
    include_entries: bool,
) -> Result<GitStatusSnapshot> {
    let structured = source
        .load_structured_status(include_untracked_files, include_entries)
        .await?;
    Ok(git_status_snapshot_from_structured(
        structured,
        include_entries,
    ))
}

pub async fn resolve_worktree_vcs_commit_lookup_from_source(
    source: &impl WorktreeVcsCommitLookupSource,
    lookup: &WorktreeVcsCommitLookup,
) -> Result<Option<String>> {
    match lookup {
        WorktreeVcsCommitLookup::Resolved(commit) => Ok(Some(commit.clone())),
        WorktreeVcsCommitLookup::Missing => Ok(None),
        WorktreeVcsCommitLookup::Head => Ok(Some(source.resolve_commit("HEAD").await?)),
        WorktreeVcsCommitLookup::TargetBranch(target_branch) => {
            Ok(Some(source.resolve_commit(target_branch).await?))
        }
    }
}

pub fn git_status_snapshot_from_structured(
    structured: WorktreeVcsStructuredStatus,
    include_entries: bool,
) -> GitStatusSnapshot {
    GitStatusSnapshot {
        raw: structured.raw,
        summary_line: structured.summary_line,
        branch: structured.branch,
        upstream: structured.upstream,
        ahead: structured.ahead,
        behind: structured.behind,
        detached: structured.detached,
        staged: structured.staged,
        unstaged: structured.unstaged,
        untracked: structured.untracked,
        entries: if include_entries {
            structured.entries
        } else {
            Vec::new()
        },
        entries_total_count: structured.entries_total_count,
        entries_truncated: structured.entries_truncated,
    }
}

pub fn worktree_vcs_structured_status_from_vcs(
    structured: VcsStructuredStatus,
) -> WorktreeVcsStructuredStatus {
    WorktreeVcsStructuredStatus {
        raw: structured.raw,
        summary_line: structured.branch.summary_line,
        branch: structured.branch.branch,
        upstream: structured.branch.upstream,
        ahead: structured.branch.ahead,
        behind: structured.branch.behind,
        detached: structured.branch.detached,
        staged: structured.staged,
        unstaged: structured.unstaged,
        untracked: structured.untracked,
        entries: structured
            .entries
            .into_iter()
            .map(|entry| GitStatusEntry {
                path: entry.path,
                orig_path: entry.orig_path,
                index_status: entry.index_status,
                worktree_status: entry.worktree_status,
            })
            .collect(),
        entries_total_count: structured.total_count,
        entries_truncated: structured.truncated,
    }
}

pub fn session_git_status_summary_from_snapshot(
    snapshot: &GitStatusSnapshot,
) -> SessionGitStatusSummary {
    SessionGitStatusSummary {
        summary_line: snapshot.summary_line.clone(),
        branch: snapshot.branch.clone(),
        upstream: snapshot.upstream.clone(),
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        detached: snapshot.detached,
        staged: snapshot.staged,
        unstaged: snapshot.unstaged,
        untracked: snapshot.untracked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn structured_status() -> WorktreeVcsStructuredStatus {
        WorktreeVcsStructuredStatus {
            raw: "## main...origin/main [ahead 1]".to_string(),
            summary_line: "## main...origin/main [ahead 1]".to_string(),
            branch: Some("main".to_string()),
            upstream: Some("origin/main".to_string()),
            ahead: 1,
            behind: 0,
            detached: false,
            staged: 1,
            unstaged: 2,
            untracked: 3,
            entries: vec![GitStatusEntry {
                path: "src/lib.rs".to_string(),
                orig_path: None,
                index_status: "M".to_string(),
                worktree_status: ".".to_string(),
            }],
            entries_total_count: 1,
            entries_truncated: false,
        }
    }

    #[test]
    fn status_snapshot_conversion_preserves_counts_and_entries() {
        let snapshot = git_status_snapshot_from_structured(structured_status(), true);

        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert_eq!(snapshot.upstream.as_deref(), Some("origin/main"));
        assert_eq!(snapshot.ahead, 1);
        assert_eq!(snapshot.staged, 1);
        assert_eq!(snapshot.unstaged, 2);
        assert_eq!(snapshot.untracked, 3);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries_total_count, 1);
    }

    #[test]
    fn status_snapshot_conversion_can_drop_entries() {
        let snapshot = git_status_snapshot_from_structured(structured_status(), false);

        assert!(snapshot.entries.is_empty());
        assert_eq!(snapshot.entries_total_count, 1);
    }

    #[test]
    fn structured_status_from_vcs_preserves_branch_counts_and_entries() {
        let structured =
            worktree_vcs_structured_status_from_vcs(ctx_fs::vcs::VcsStructuredStatus {
                raw: "## feature...origin/feature [ahead 2, behind 1]".to_string(),
                branch: ctx_fs::vcs::VcsStatusBranchInfo {
                    summary_line: "## feature...origin/feature [ahead 2, behind 1]".to_string(),
                    branch: Some("feature".to_string()),
                    upstream: Some("origin/feature".to_string()),
                    ahead: 2,
                    behind: 1,
                    detached: false,
                },
                staged: 3,
                unstaged: 4,
                untracked: 5,
                entries: vec![ctx_fs::vcs::VcsStatusEntry {
                    path: "src/main.rs".to_string(),
                    orig_path: Some("src/old.rs".to_string()),
                    index_status: "R".to_string(),
                    worktree_status: "M".to_string(),
                }],
                total_count: 9,
                truncated: true,
            });

        assert_eq!(
            structured.summary_line,
            "## feature...origin/feature [ahead 2, behind 1]"
        );
        assert_eq!(structured.branch.as_deref(), Some("feature"));
        assert_eq!(structured.upstream.as_deref(), Some("origin/feature"));
        assert_eq!(structured.ahead, 2);
        assert_eq!(structured.behind, 1);
        assert_eq!(structured.staged, 3);
        assert_eq!(structured.unstaged, 4);
        assert_eq!(structured.untracked, 5);
        assert_eq!(structured.entries_total_count, 9);
        assert!(structured.entries_truncated);
        assert_eq!(structured.entries.len(), 1);
        assert_eq!(
            structured.entries[0].orig_path.as_deref(),
            Some("src/old.rs")
        );
    }

    #[test]
    fn session_git_status_summary_projection_preserves_route_persisted_fields() {
        let snapshot = git_status_snapshot_from_structured(structured_status(), true);
        let summary = session_git_status_summary_from_snapshot(&snapshot);

        assert_eq!(summary.summary_line, "## main...origin/main [ahead 1]");
        assert_eq!(summary.branch.as_deref(), Some("main"));
        assert_eq!(summary.upstream.as_deref(), Some("origin/main"));
        assert_eq!(summary.ahead, 1);
        assert_eq!(summary.behind, 0);
        assert!(!summary.detached);
        assert_eq!(summary.staged, 1);
        assert_eq!(summary.unstaged, 2);
        assert_eq!(summary.untracked, 3);
    }

    struct FakeCommitLookupSource;

    #[async_trait::async_trait]
    impl WorktreeVcsCommitLookupSource for FakeCommitLookupSource {
        async fn resolve_commit(&self, reference: &str) -> Result<String> {
            Ok(format!("resolved-{reference}"))
        }
    }

    #[tokio::test]
    async fn commit_lookup_source_resolves_only_live_requests() {
        let source = FakeCommitLookupSource;

        assert_eq!(
            resolve_worktree_vcs_commit_lookup_from_source(
                &source,
                &WorktreeVcsCommitLookup::Resolved("known".to_string())
            )
            .await
            .unwrap()
            .as_deref(),
            Some("known")
        );
        assert_eq!(
            resolve_worktree_vcs_commit_lookup_from_source(&source, &WorktreeVcsCommitLookup::Head)
                .await
                .unwrap()
                .as_deref(),
            Some("resolved-HEAD")
        );
        assert_eq!(
            resolve_worktree_vcs_commit_lookup_from_source(
                &source,
                &WorktreeVcsCommitLookup::TargetBranch("main".to_string())
            )
            .await
            .unwrap()
            .as_deref(),
            Some("resolved-main")
        );
        assert!(resolve_worktree_vcs_commit_lookup_from_source(
            &source,
            &WorktreeVcsCommitLookup::Missing
        )
        .await
        .unwrap()
        .is_none());
    }
}

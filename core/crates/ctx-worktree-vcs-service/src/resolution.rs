use anyhow::Result;
use ctx_core::models::{
    DiffUnavailableReason, Worktree, WorktreeVcsBaseResolutionKind, WorktreeVcsTargetSource,
};

pub fn is_no_vcs_repo_error(err: &anyhow::Error) -> bool {
    let lower = err.to_string().to_lowercase();
    lower.contains("no vcs repo found")
        || lower.contains("not a git repository")
        || lower.contains("is not a git repo")
        || lower.contains("not inside a jj repo")
}

#[derive(Clone)]
pub struct WorktreeDiffBaseResolution {
    pub base_commit_sha: String,
    pub head_commit_sha: Option<String>,
    pub target_branch: Option<String>,
    pub target_branch_commit_sha: Option<String>,
    pub target_source: Option<WorktreeVcsTargetSource>,
    pub kind: WorktreeVcsBaseResolutionKind,
    pub error: Option<String>,
    pub unavailable_reason: Option<DiffUnavailableReason>,
    pub explicit_target: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeVcsDiffBaseQuery {
    pub base_commit_sha: Option<String>,
    pub target_branch: Option<String>,
}

#[async_trait::async_trait]
pub trait WorktreeVcsDiffBaseSource: Send + Sync {
    async fn load_primary_branch(&self) -> Result<Option<String>>;
    async fn rev_parse_head(&self) -> Result<String>;
    async fn rev_parse_refs(&self, references: &[&str]) -> Result<Vec<String>>;
    async fn merge_base(&self, target_branch: &str) -> Result<String>;

    fn redact_error(&self, err: &anyhow::Error) -> String {
        err.to_string()
    }
}

async fn resolve_worktree_ref_commits_from_source(
    source: &impl WorktreeVcsDiffBaseSource,
    worktree: &Worktree,
    target_branch: Option<&str>,
) -> (Option<String>, Option<String>) {
    let Some(target_branch) = target_branch else {
        return match source.rev_parse_refs(&["HEAD"]).await {
            Ok(commits) => (commits.first().cloned(), None),
            Err(err) => {
                tracing::warn!(
                    worktree_id = %worktree.id.0,
                    "failed to resolve worktree head for diff metadata: {err:#}"
                );
                (None, None)
            }
        };
    };
    let refs = vec!["HEAD", target_branch];
    match source.rev_parse_refs(&refs).await {
        Ok(commits) => {
            let head = commits.first().cloned();
            let target = commits.get(1).cloned();
            (head, target)
        }
        Err(err) => {
            tracing::warn!(
                worktree_id = %worktree.id.0,
                "failed to resolve worktree refs for diff metadata: {err:#}"
            );
            let head = match source.rev_parse_head().await {
                Ok(head) => Some(head),
                Err(head_err) => {
                    tracing::warn!(
                        worktree_id = %worktree.id.0,
                        "failed to resolve worktree head after target branch lookup failed: {head_err:#}"
                    );
                    None
                }
            };
            (head, None)
        }
    }
}

pub async fn resolve_worktree_diff_base_from_source(
    source: &impl WorktreeVcsDiffBaseSource,
    worktree: &Worktree,
    query: WorktreeVcsDiffBaseQuery,
) -> WorktreeDiffBaseResolution {
    if let Some(base) = query.base_commit_sha.as_deref() {
        let trimmed = base.trim();
        if !trimmed.is_empty() {
            let (head_commit_sha, _) =
                resolve_worktree_ref_commits_from_source(source, worktree, None).await;
            return WorktreeDiffBaseResolution {
                base_commit_sha: trimmed.to_string(),
                head_commit_sha,
                target_branch: None,
                target_branch_commit_sha: None,
                target_source: None,
                kind: WorktreeVcsBaseResolutionKind::ExplicitBase,
                error: None,
                unavailable_reason: None,
                explicit_target: false,
            };
        }
    }

    let explicit_target = query
        .target_branch
        .as_deref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let mut target_branch = match query.target_branch.as_deref() {
        Some(target) if !target.trim().is_empty() => Some(target.trim().to_string()),
        _ => None,
    };
    let mut target_source = if target_branch.is_some() {
        Some(WorktreeVcsTargetSource::Explicit)
    } else {
        None
    };

    if target_branch.is_none() {
        match source.load_primary_branch().await {
            Ok(Some(branch)) => {
                target_branch = Some(branch);
                target_source = Some(WorktreeVcsTargetSource::PrimaryBranchConfig);
            }
            Ok(None) => {
                let (head_commit_sha, _) =
                    resolve_worktree_ref_commits_from_source(source, worktree, None).await;
                return WorktreeDiffBaseResolution {
                    base_commit_sha: worktree.base_commit_sha.clone(),
                    head_commit_sha,
                    target_branch: None,
                    target_branch_commit_sha: None,
                    target_source: None,
                    kind: WorktreeVcsBaseResolutionKind::WorktreeBase,
                    error: Some("workspace primary branch is not configured".to_string()),
                    unavailable_reason: Some(DiffUnavailableReason::NoTargetBranch),
                    explicit_target,
                };
            }
            Err(err) => tracing::warn!(
                workspace_id = %worktree.workspace_id.0,
                "failed to load workspace primary branch: {err:#}"
            ),
        }
    }

    let mut error: Option<String> = None;
    let mut unavailable_reason: Option<DiffUnavailableReason> = None;
    if let Some(target_branch) = target_branch.clone() {
        match source.merge_base(&target_branch).await {
            Ok(base) => {
                let (head_commit_sha, target_branch_commit_sha) =
                    resolve_worktree_ref_commits_from_source(
                        source,
                        worktree,
                        Some(&target_branch),
                    )
                    .await;
                return WorktreeDiffBaseResolution {
                    base_commit_sha: base,
                    head_commit_sha,
                    target_branch: Some(target_branch),
                    target_branch_commit_sha,
                    target_source,
                    kind: WorktreeVcsBaseResolutionKind::MergeBase,
                    error: None,
                    unavailable_reason: None,
                    explicit_target,
                };
            }
            Err(err) => {
                error = Some(source.redact_error(&err));
                unavailable_reason = Some(if is_no_vcs_repo_error(&err) {
                    DiffUnavailableReason::NoRepo
                } else {
                    DiffUnavailableReason::NoTargetBranch
                });
                tracing::warn!(
                    worktree_id = %worktree.id.0,
                    "merge-base failed for target {target_branch}: {err:#}"
                );
            }
        }
    }

    let (head_commit_sha, target_branch_commit_sha) =
        resolve_worktree_ref_commits_from_source(source, worktree, target_branch.as_deref()).await;
    WorktreeDiffBaseResolution {
        base_commit_sha: worktree.base_commit_sha.clone(),
        head_commit_sha,
        target_branch,
        target_branch_commit_sha,
        target_source,
        kind: WorktreeVcsBaseResolutionKind::WorktreeBase,
        error,
        unavailable_reason,
        explicit_target,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use chrono::Utc;
    use ctx_core::ids::{WorkspaceId, WorktreeId};
    use ctx_core::models::VcsKind;

    use super::*;

    #[test]
    fn no_vcs_repo_classifier_matches_supported_vcs_messages() {
        for message in [
            "no vcs repo found",
            "fatal: not a git repository",
            "path is not a git repo",
            "not inside a jj repo",
        ] {
            assert!(is_no_vcs_repo_error(&anyhow!(message)));
        }
    }

    #[test]
    fn no_vcs_repo_classifier_rejects_unrelated_errors() {
        assert!(!is_no_vcs_repo_error(&anyhow!("permission denied")));
    }

    struct FakeDiffBaseSource {
        primary_branch: Option<String>,
        primary_branch_error: Option<&'static str>,
        head: String,
        head_error: Option<&'static str>,
        refs: Vec<String>,
        refs_error: Option<&'static str>,
        merge_base: String,
        merge_base_error: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl WorktreeVcsDiffBaseSource for FakeDiffBaseSource {
        async fn load_primary_branch(&self) -> Result<Option<String>> {
            if let Some(error) = self.primary_branch_error {
                return Err(anyhow!(error));
            }
            Ok(self.primary_branch.clone())
        }

        async fn rev_parse_head(&self) -> Result<String> {
            if let Some(error) = self.head_error {
                return Err(anyhow!(error));
            }
            Ok(self.head.clone())
        }

        async fn rev_parse_refs(&self, _references: &[&str]) -> Result<Vec<String>> {
            if let Some(error) = self.refs_error {
                return Err(anyhow!(error));
            }
            Ok(self.refs.clone())
        }

        async fn merge_base(&self, _target_branch: &str) -> Result<String> {
            if let Some(error) = self.merge_base_error {
                return Err(anyhow!(error));
            }
            Ok(self.merge_base.clone())
        }
    }

    fn worktree() -> Worktree {
        Worktree {
            id: WorktreeId::new(),
            workspace_id: WorkspaceId::new(),
            root_path: "/repo".to_string(),
            base_commit_sha: "worktree-base".to_string(),
            git_branch: Some("feature".to_string()),
            vcs_kind: Some(VcsKind::Git),
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

    fn source() -> FakeDiffBaseSource {
        FakeDiffBaseSource {
            primary_branch: Some("main".to_string()),
            primary_branch_error: None,
            head: "head".to_string(),
            head_error: None,
            refs: vec!["head".to_string(), "target".to_string()],
            refs_error: None,
            merge_base: "merge-base".to_string(),
            merge_base_error: None,
        }
    }

    #[tokio::test]
    async fn diff_base_resolution_uses_explicit_base_without_target() {
        let resolution = resolve_worktree_diff_base_from_source(
            &source(),
            &worktree(),
            WorktreeVcsDiffBaseQuery {
                base_commit_sha: Some(" explicit-base ".to_string()),
                target_branch: Some("main".to_string()),
            },
        )
        .await;

        assert_eq!(resolution.base_commit_sha, "explicit-base");
        assert_eq!(resolution.head_commit_sha.as_deref(), Some("head"));
        assert!(resolution.target_branch.is_none());
        assert_eq!(resolution.kind, WorktreeVcsBaseResolutionKind::ExplicitBase);
    }

    #[tokio::test]
    async fn diff_base_resolution_uses_primary_branch_merge_base() {
        let resolution =
            resolve_worktree_diff_base_from_source(&source(), &worktree(), Default::default())
                .await;

        assert_eq!(resolution.base_commit_sha, "merge-base");
        assert_eq!(resolution.head_commit_sha.as_deref(), Some("head"));
        assert_eq!(resolution.target_branch.as_deref(), Some("main"));
        assert_eq!(
            resolution.target_branch_commit_sha.as_deref(),
            Some("target")
        );
        assert_eq!(
            resolution.target_source,
            Some(WorktreeVcsTargetSource::PrimaryBranchConfig)
        );
        assert_eq!(resolution.kind, WorktreeVcsBaseResolutionKind::MergeBase);
    }

    #[tokio::test]
    async fn diff_base_resolution_reports_missing_primary_branch() {
        let fake = FakeDiffBaseSource {
            primary_branch: None,
            refs: vec!["head".to_string()],
            ..source()
        };
        let resolution =
            resolve_worktree_diff_base_from_source(&fake, &worktree(), Default::default()).await;

        assert_eq!(resolution.base_commit_sha, "worktree-base");
        assert_eq!(resolution.head_commit_sha.as_deref(), Some("head"));
        assert_eq!(
            resolution.unavailable_reason,
            Some(DiffUnavailableReason::NoTargetBranch)
        );
    }

    #[tokio::test]
    async fn diff_base_resolution_preserves_explicit_target_failure() {
        let fake = FakeDiffBaseSource {
            refs: vec!["head".to_string()],
            merge_base_error: Some("missing target"),
            ..source()
        };
        let resolution = resolve_worktree_diff_base_from_source(
            &fake,
            &worktree(),
            WorktreeVcsDiffBaseQuery {
                base_commit_sha: None,
                target_branch: Some("main".to_string()),
            },
        )
        .await;

        assert!(resolution.explicit_target);
        assert_eq!(resolution.base_commit_sha, "worktree-base");
        assert_eq!(
            resolution.unavailable_reason,
            Some(DiffUnavailableReason::NoTargetBranch)
        );
        assert_eq!(resolution.error.as_deref(), Some("missing target"));
    }
}

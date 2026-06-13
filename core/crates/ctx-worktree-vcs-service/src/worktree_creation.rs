use std::path::Path;

use ctx_core::models::VcsKind;
use ctx_fs::vcs;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorktreeCreationBase {
    pub base_commit_sha: String,
    pub vcs_kind: VcsKind,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum WorktreeCreationBaseError {
    RepositoryUnavailable { message: String },
    MissingHead { message: String },
    HeadLookupFailed { message: String },
}

impl WorktreeCreationBaseError {
    pub fn is_client_error(&self) -> bool {
        matches!(
            self,
            WorktreeCreationBaseError::RepositoryUnavailable { .. }
                | WorktreeCreationBaseError::MissingHead { .. }
        )
    }
}

pub async fn resolve_worktree_creation_base(
    workspace_root: &Path,
) -> Result<WorktreeCreationBase, WorktreeCreationBaseError> {
    let driver = vcs::driver_for_path(workspace_root)
        .await
        .map_err(|error| WorktreeCreationBaseError::RepositoryUnavailable {
            message: error.to_string(),
        })?;
    let vcs_kind = driver.kind();
    let base_commit_sha = driver
        .rev_parse_head(workspace_root)
        .await
        .map_err(|error| {
            let message = error.to_string();
            if is_missing_head_message(&message) {
                WorktreeCreationBaseError::MissingHead { message }
            } else {
                WorktreeCreationBaseError::HeadLookupFailed { message }
            }
        })?;

    Ok(WorktreeCreationBase {
        base_commit_sha,
        vcs_kind,
    })
}

fn is_missing_head_message(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("ambiguous argument 'head'")
        || message.contains("unknown revision or path not in the working tree")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_head_classifier_matches_git_empty_repo_errors() {
        assert!(is_missing_head_message(
            "fatal: ambiguous argument 'HEAD': unknown revision"
        ));
        assert!(is_missing_head_message(
            "unknown revision or path not in the working tree"
        ));
        assert!(!is_missing_head_message("permission denied"));
    }

    #[test]
    fn only_repo_and_missing_head_errors_are_client_errors() {
        assert!(WorktreeCreationBaseError::RepositoryUnavailable {
            message: "not a repo".to_string(),
        }
        .is_client_error());
        assert!(WorktreeCreationBaseError::MissingHead {
            message: "empty".to_string(),
        }
        .is_client_error());
        assert!(!WorktreeCreationBaseError::HeadLookupFailed {
            message: "io".to_string(),
        }
        .is_client_error());
    }
}

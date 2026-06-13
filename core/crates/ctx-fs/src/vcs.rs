use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use ctx_core::models::VcsKind;

use crate::git;
use crate::patch::{self, WorktreePatch};

pub use crate::git::ApplyPatchTarget;

mod jj;
mod jj_status;

use jj::JjVcs;

pub use jj::jj_command_output;

pub type VcsFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

#[derive(Debug, Clone, Default)]
pub struct VcsStatusBranchInfo {
    pub summary_line: String,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub detached: bool,
}

#[derive(Debug, Clone)]
pub struct VcsStatusEntry {
    pub path: String,
    pub orig_path: Option<String>,
    pub index_status: String,
    pub worktree_status: String,
}

#[derive(Debug, Clone, Default)]
pub struct VcsStructuredStatus {
    pub raw: String,
    pub branch: VcsStatusBranchInfo,
    pub entries: Vec<VcsStatusEntry>,
    pub staged: i64,
    pub unstaged: i64,
    pub untracked: i64,
    pub total_count: i64,
    pub truncated: bool,
}

pub trait VcsDriver: Send + Sync {
    fn kind(&self) -> VcsKind;
    fn assert_repo<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, ()>;
    fn is_repo<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, bool>;
    fn is_worktree<'a>(&'a self, worktree_path: &'a Path) -> VcsFuture<'a, bool>;
    fn rev_parse_head<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, String>;
    fn rev_parse_ref<'a>(&'a self, root: &'a Path, reference: &'a str) -> VcsFuture<'a, String>;
    fn merge_base<'a>(&'a self, root: &'a Path, a: &'a str, b: &'a str) -> VcsFuture<'a, String>;
    fn is_ancestor<'a>(
        &'a self,
        root: &'a Path,
        ancestor: &'a str,
        descendant: &'a str,
    ) -> VcsFuture<'a, bool>;
    fn create_worktree<'a>(
        &'a self,
        workspace_root: &'a Path,
        worktree_path: &'a Path,
        base_revision: &'a str,
        branch_name: &'a str,
    ) -> VcsFuture<'a, ()>;
    fn remove_worktree<'a>(
        &'a self,
        workspace_root: &'a Path,
        worktree_path: &'a Path,
    ) -> VcsFuture<'a, ()>;
    fn prune_worktrees<'a>(&'a self, workspace_root: &'a Path) -> VcsFuture<'a, ()>;
    fn diff<'a>(&'a self, worktree_path: &'a Path, base_revision: &'a str)
        -> VcsFuture<'a, String>;
    fn diff_summary<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, (i64, i64, i64)>;
    fn diff_file_count<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, i64>;
    fn diff_name_status<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, Vec<VcsNameStatusEntry>>;
    fn diff_name_status_for_summary<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, Vec<VcsNameStatusEntry>> {
        self.diff_name_status(worktree_path, base_revision)
    }
    fn diff_name_status_paths<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
        paths: &'a [String],
    ) -> VcsFuture<'a, Vec<VcsNameStatusEntry>>;
    fn list_untracked<'a>(&'a self, worktree_path: &'a Path) -> VcsFuture<'a, Vec<String>>;
    fn untracked_file_count<'a>(&'a self, worktree_path: &'a Path) -> VcsFuture<'a, i64>;
    fn diff_untracked_file<'a>(
        &'a self,
        worktree_path: &'a Path,
        rel_path: &'a str,
    ) -> VcsFuture<'a, String>;
    fn status_short<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, String>;
    fn status_porcelain<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, Vec<String>>;
    fn status_structured<'a>(
        &'a self,
        root: &'a Path,
        include_untracked_files: bool,
        include_entries: bool,
    ) -> VcsFuture<'a, VcsStructuredStatus>;
    fn build_worktree_patch<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, WorktreePatch>;
    fn apply_patch<'a>(
        &'a self,
        root: &'a Path,
        patch: &'a str,
        target: ApplyPatchTarget,
        reverse: bool,
    ) -> VcsFuture<'a, ()>;
    fn reset_worktree_to_revision<'a>(
        &'a self,
        root: &'a Path,
        revision: &'a str,
    ) -> VcsFuture<'a, ()>;
    fn delete_branch<'a>(&'a self, root: &'a Path, branch: &'a str) -> VcsFuture<'a, ()>;
}

#[derive(Debug, Clone)]
pub struct VcsNameStatusEntry {
    pub status: String,
    pub path: String,
    pub orig_path: Option<String>,
}

pub struct GitVcs;

impl VcsDriver for GitVcs {
    fn kind(&self) -> VcsKind {
        VcsKind::Git
    }

    fn assert_repo<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, ()> {
        Box::pin(async move { git::assert_git_repo(root).await })
    }

    fn is_repo<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, bool> {
        Box::pin(async move { Ok(git::is_git_worktree(root).await.unwrap_or(false)) })
    }

    fn is_worktree<'a>(&'a self, worktree_path: &'a Path) -> VcsFuture<'a, bool> {
        Box::pin(async move { git::is_git_worktree(worktree_path).await })
    }

    fn rev_parse_head<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, String> {
        Box::pin(async move { git::rev_parse_head(root).await })
    }

    fn rev_parse_ref<'a>(&'a self, root: &'a Path, reference: &'a str) -> VcsFuture<'a, String> {
        Box::pin(async move { git::rev_parse_ref(root, reference).await })
    }

    fn merge_base<'a>(&'a self, root: &'a Path, a: &'a str, b: &'a str) -> VcsFuture<'a, String> {
        Box::pin(async move { git::git_merge_base(root, a, b).await })
    }

    fn is_ancestor<'a>(
        &'a self,
        root: &'a Path,
        ancestor: &'a str,
        descendant: &'a str,
    ) -> VcsFuture<'a, bool> {
        Box::pin(async move { git::git_is_ancestor(root, ancestor, descendant).await })
    }

    fn create_worktree<'a>(
        &'a self,
        workspace_root: &'a Path,
        worktree_path: &'a Path,
        base_revision: &'a str,
        branch_name: &'a str,
    ) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            let mut cmd = Command::new("git");
            cmd.arg("-C")
                .arg(workspace_root)
                .arg("worktree")
                .arg("add")
                .arg(worktree_path);
            if branch_name.trim().is_empty() {
                cmd.arg(base_revision);
            } else if git::branch_exists(workspace_root, branch_name).await? {
                cmd.arg(branch_name);
            } else {
                cmd.arg("-b").arg(branch_name).arg(base_revision);
            }
            let output = cmd
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .context("running git worktree add")?;
            if !output.status.success() {
                bail!(
                    "git worktree add failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(())
        })
    }

    fn remove_worktree<'a>(
        &'a self,
        workspace_root: &'a Path,
        worktree_path: &'a Path,
    ) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            let output = Command::new("git")
                .arg("-C")
                .arg(workspace_root)
                .arg("worktree")
                .arg("remove")
                .arg("--force")
                .arg(worktree_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .context("running git worktree remove")?;
            if !output.status.success() {
                bail!(
                    "git worktree remove failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(())
        })
    }

    fn prune_worktrees<'a>(&'a self, workspace_root: &'a Path) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            let output = Command::new("git")
                .arg("-C")
                .arg(workspace_root)
                .arg("worktree")
                .arg("prune")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .context("running git worktree prune")?;
            if !output.status.success() {
                bail!(
                    "git worktree prune failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(())
        })
    }

    fn diff<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, String> {
        Box::pin(async move { git::git_diff(worktree_path, base_revision).await })
    }

    fn diff_summary<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, (i64, i64, i64)> {
        Box::pin(async move {
            let mut file_count: i64 = 0;
            let mut additions: i64 = 0;
            let mut deletions: i64 = 0;
            let numstats = git::git_diff_numstat(worktree_path, base_revision).await?;
            for (add, del, _path) in numstats {
                file_count += 1;
                additions += add;
                deletions += del;
            }
            Ok((file_count, additions, deletions))
        })
    }

    fn diff_file_count<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, i64> {
        Box::pin(async move {
            crate::git_counts::diff_name_status_count(worktree_path, base_revision).await
        })
    }

    fn diff_name_status<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, Vec<VcsNameStatusEntry>> {
        Box::pin(async move {
            let entries = git::git_diff_name_status(worktree_path, base_revision).await?;
            Ok(entries
                .into_iter()
                .map(|entry| VcsNameStatusEntry {
                    status: entry.status,
                    path: entry.path,
                    orig_path: entry.orig_path,
                })
                .collect())
        })
    }

    fn diff_name_status_for_summary<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, Vec<VcsNameStatusEntry>> {
        Box::pin(async move {
            let entries =
                git::git_diff_name_status_no_renames(worktree_path, base_revision).await?;
            Ok(entries
                .into_iter()
                .map(|entry| VcsNameStatusEntry {
                    status: entry.status,
                    path: entry.path,
                    orig_path: entry.orig_path,
                })
                .collect())
        })
    }

    fn diff_name_status_paths<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
        paths: &'a [String],
    ) -> VcsFuture<'a, Vec<VcsNameStatusEntry>> {
        Box::pin(async move {
            let entries =
                git::git_diff_name_status_paths(worktree_path, base_revision, paths).await?;
            Ok(entries
                .into_iter()
                .map(|(status, path, orig_path)| VcsNameStatusEntry {
                    status,
                    path,
                    orig_path,
                })
                .collect())
        })
    }

    fn list_untracked<'a>(&'a self, worktree_path: &'a Path) -> VcsFuture<'a, Vec<String>> {
        Box::pin(async move { git::list_untracked_files(worktree_path).await })
    }

    fn untracked_file_count<'a>(&'a self, worktree_path: &'a Path) -> VcsFuture<'a, i64> {
        Box::pin(async move { crate::git_counts::untracked_count(worktree_path).await })
    }

    fn diff_untracked_file<'a>(
        &'a self,
        worktree_path: &'a Path,
        rel_path: &'a str,
    ) -> VcsFuture<'a, String> {
        Box::pin(async move { git::git_diff_untracked_file(worktree_path, rel_path).await })
    }

    fn status_short<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, String> {
        Box::pin(async move { git::git_status_short(root).await })
    }

    fn status_porcelain<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, Vec<String>> {
        Box::pin(async move { git::git_status_porcelain(root).await })
    }

    fn status_structured<'a>(
        &'a self,
        root: &'a Path,
        include_untracked_files: bool,
        include_entries: bool,
    ) -> VcsFuture<'a, VcsStructuredStatus> {
        Box::pin(async move {
            git::git_status_structured(root, include_untracked_files, include_entries).await
        })
    }

    fn build_worktree_patch<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, WorktreePatch> {
        Box::pin(async move { patch::build_worktree_patch(worktree_path, base_revision).await })
    }

    fn apply_patch<'a>(
        &'a self,
        root: &'a Path,
        patch: &'a str,
        target: ApplyPatchTarget,
        reverse: bool,
    ) -> VcsFuture<'a, ()> {
        Box::pin(async move { git::git_apply_patch(root, patch, target, reverse).await })
    }

    fn reset_worktree_to_revision<'a>(
        &'a self,
        root: &'a Path,
        revision: &'a str,
    ) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            let output = Command::new("git")
                .arg("-C")
                .arg(root)
                .arg("reset")
                .arg("--hard")
                .arg(revision)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .context("running git reset --hard")?;
            if !output.status.success() {
                bail!(
                    "git reset --hard failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(())
        })
    }

    fn delete_branch<'a>(&'a self, root: &'a Path, branch: &'a str) -> VcsFuture<'a, ()> {
        Box::pin(async move { git::delete_branch(root, branch).await })
    }
}

pub async fn driver_for_path(root: impl AsRef<Path>) -> Result<Arc<dyn VcsDriver>> {
    let root = root.as_ref();
    if root.join(".jj").exists() {
        let jj = JjVcs;
        jj.assert_repo(root).await?;
        return Ok(Arc::new(JjVcs));
    }
    let jj = JjVcs;
    if jj.is_repo(root).await.unwrap_or(false) {
        return Ok(Arc::new(JjVcs));
    }
    let git = GitVcs;
    if git.is_repo(root).await.unwrap_or(false) {
        return Ok(Arc::new(GitVcs));
    }
    bail!("no vcs repo found at {}", root.display());
}

pub fn driver_for_kind(kind: Option<VcsKind>) -> Arc<dyn VcsDriver> {
    match kind {
        Some(VcsKind::Jj) => Arc::new(JjVcs),
        _ => Arc::new(GitVcs),
    }
}

#[cfg(test)]
mod tests {
    use super::jj_status::parse_jj_status_output;

    #[test]
    fn parse_jj_status_output_handles_untracked_dirs() {
        let output = r#"
Working copy changes:
M file.txt
A new.txt
Untracked paths:
? dir/
? stray.txt
Working copy  (@) : abcdef0123
"#;
        let parsed = parse_jj_status_output(output);
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].status, 'M');
        assert_eq!(parsed.entries[0].path, "file.txt");
        assert_eq!(parsed.entries[1].status, 'A');
        assert_eq!(parsed.entries[1].path, "new.txt");
        assert_eq!(parsed.untracked.len(), 2);
        assert_eq!(parsed.untracked[0].path, "dir");
        assert!(parsed.untracked[0].is_dir);
        assert_eq!(parsed.untracked[1].path, "stray.txt");
        assert!(!parsed.untracked[1].is_dir);
    }
}

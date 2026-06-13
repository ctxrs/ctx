mod diff;
mod patch;
mod refs;
mod support;

use super::*;
use crate::patch::should_ignore_path;
use std::collections::HashSet;

use tokio::fs;

use super::jj_status::{
    collect_jj_untracked_files, count_jj_untracked_files, parse_jj_status_output,
};
use diff::{
    count_file_lines, diff_summary_from_git, git_output_allow, jj_diff_name_count,
    jj_diff_name_only, jj_diff_name_only_paths,
};
use patch::{jj_apply_patch, jj_command_unsupported, JjApplyOutcome};
use refs::{jj_is_ancestor, jj_merge_base, jj_rev_parse};
pub use support::jj_command_output;
use support::{ensure_jj_usable, jj_command, run_jj};

pub struct JjVcs;

impl VcsDriver for JjVcs {
    fn kind(&self) -> VcsKind {
        VcsKind::Jj
    }

    fn assert_repo<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            run_jj(root, &["root"]).await?;
            Ok(())
        })
    }

    fn is_repo<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, bool> {
        Box::pin(async move {
            if ensure_jj_usable().await.is_err() {
                return Ok(false);
            }
            let output = jj_command(root)
                .arg("root")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .await;
            match output {
                Ok(output) => Ok(output.status.success()),
                Err(_) => Ok(false),
            }
        })
    }

    fn is_worktree<'a>(&'a self, worktree_path: &'a Path) -> VcsFuture<'a, bool> {
        Box::pin(async move {
            if ensure_jj_usable().await.is_err() {
                return Ok(false);
            }
            let output = jj_command(worktree_path)
                .arg("root")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .await;
            match output {
                Ok(output) => Ok(output.status.success()),
                Err(_) => Ok(false),
            }
        })
    }

    fn rev_parse_head<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, String> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            jj_rev_parse(root, "@").await
        })
    }

    fn rev_parse_ref<'a>(&'a self, root: &'a Path, reference: &'a str) -> VcsFuture<'a, String> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            jj_rev_parse(root, reference).await
        })
    }

    fn merge_base<'a>(&'a self, root: &'a Path, a: &'a str, b: &'a str) -> VcsFuture<'a, String> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            jj_merge_base(root, a, b).await
        })
    }

    fn is_ancestor<'a>(
        &'a self,
        root: &'a Path,
        ancestor: &'a str,
        descendant: &'a str,
    ) -> VcsFuture<'a, bool> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            jj_is_ancestor(root, ancestor, descendant).await
        })
    }

    fn create_worktree<'a>(
        &'a self,
        workspace_root: &'a Path,
        worktree_path: &'a Path,
        base_revision: &'a str,
        branch_name: &'a str,
    ) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            let mut cmd = jj_command(workspace_root);
            cmd.arg("workspace").arg("add");
            if !branch_name.trim().is_empty() {
                cmd.arg("--name").arg(branch_name);
            }
            if !base_revision.trim().is_empty() {
                cmd.arg("--revision").arg(base_revision);
            }
            if let Some(parent) = worktree_path.parent() {
                fs::create_dir_all(parent).await?;
            }
            cmd.arg(worktree_path);
            let output = cmd
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .context("running jj workspace add")?;
            if !output.status.success() {
                bail!(
                    "jj workspace add failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(())
        })
    }

    fn remove_worktree<'a>(
        &'a self,
        _workspace_root: &'a Path,
        worktree_path: &'a Path,
    ) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            let output = jj_command(worktree_path)
                .arg("workspace")
                .arg("forget")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .context("running jj workspace forget")?;
            if !output.status.success() {
                bail!(
                    "jj workspace forget failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(())
        })
    }

    fn prune_worktrees<'a>(&'a self, workspace_root: &'a Path) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            run_jj(workspace_root, &["workspace", "update-stale"]).await?;
            Ok(())
        })
    }

    fn diff<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, String> {
        Box::pin(async move {
            let output = run_jj(
                worktree_path,
                &["diff", "--git", "--from", base_revision, "--to", "@"],
            )
            .await?;
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        })
    }

    fn diff_summary<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, (i64, i64, i64)> {
        Box::pin(async move {
            let output = run_jj(
                worktree_path,
                &["diff", "--git", "--from", base_revision, "--to", "@"],
            )
            .await?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(diff_summary_from_git(&stdout))
        })
    }

    fn diff_file_count<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, i64> {
        Box::pin(async move { jj_diff_name_count(worktree_path, base_revision).await })
    }

    fn diff_name_status<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, Vec<VcsNameStatusEntry>> {
        Box::pin(async move {
            let paths = jj_diff_name_only(worktree_path, base_revision).await?;
            Ok(paths
                .into_iter()
                .map(|path| VcsNameStatusEntry {
                    status: "M".to_string(),
                    path,
                    orig_path: None,
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
            let paths = jj_diff_name_only_paths(worktree_path, base_revision, paths).await?;
            Ok(paths
                .into_iter()
                .map(|path| VcsNameStatusEntry {
                    status: "M".to_string(),
                    path,
                    orig_path: None,
                })
                .collect())
        })
    }

    fn list_untracked<'a>(&'a self, worktree_path: &'a Path) -> VcsFuture<'a, Vec<String>> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            let output = run_jj(worktree_path, &["status"]).await?;
            let parsed = parse_jj_status_output(&String::from_utf8_lossy(&output.stdout));
            collect_jj_untracked_files(worktree_path, &parsed.untracked).await
        })
    }

    fn untracked_file_count<'a>(&'a self, worktree_path: &'a Path) -> VcsFuture<'a, i64> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            let output = run_jj(worktree_path, &["status"]).await?;
            let parsed = parse_jj_status_output(&String::from_utf8_lossy(&output.stdout));
            count_jj_untracked_files(worktree_path, &parsed.untracked).await
        })
    }

    fn diff_untracked_file<'a>(
        &'a self,
        worktree_path: &'a Path,
        rel_path: &'a str,
    ) -> VcsFuture<'a, String> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            git::git_diff_untracked_file(worktree_path, rel_path).await
        })
    }

    fn status_short<'a>(&'a self, _root: &'a Path) -> VcsFuture<'a, String> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            Ok("## @\n".to_string())
        })
    }

    fn status_porcelain<'a>(&'a self, root: &'a Path) -> VcsFuture<'a, Vec<String>> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            let output = run_jj(root, &["status"]).await?;
            let parsed = parse_jj_status_output(&String::from_utf8_lossy(&output.stdout));
            let mut entries = Vec::new();
            for entry in parsed.entries {
                entries.push(format!(" {} {}", entry.status, entry.path));
            }
            for entry in parsed.untracked {
                if should_ignore_path(Path::new(&entry.path)) {
                    continue;
                }
                let mut path = entry.path;
                if entry.is_dir {
                    path.push(std::path::MAIN_SEPARATOR);
                }
                entries.push(format!("?? {path}"));
            }
            Ok(entries)
        })
    }

    fn status_structured<'a>(
        &'a self,
        root: &'a Path,
        include_untracked_files: bool,
        include_entries: bool,
    ) -> VcsFuture<'a, VcsStructuredStatus> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            let output = run_jj(root, &["status"]).await?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let parsed = parse_jj_status_output(&stdout);
            let mut entries = Vec::new();
            let staged = 0;
            let unstaged = parsed.entries.len() as i64;
            if include_entries {
                for entry in parsed.entries {
                    entries.push(VcsStatusEntry {
                        path: entry.path,
                        orig_path: None,
                        index_status: " ".to_string(),
                        worktree_status: entry.status.to_string(),
                    });
                }
            }
            let untracked = if include_untracked_files {
                let untracked_files = collect_jj_untracked_files(root, &parsed.untracked).await?;
                let untracked = untracked_files.len() as i64;
                if include_entries {
                    for path in untracked_files {
                        entries.push(VcsStatusEntry {
                            path,
                            orig_path: None,
                            index_status: "?".to_string(),
                            worktree_status: "?".to_string(),
                        });
                    }
                }
                untracked
            } else {
                parsed.untracked.len() as i64
            };
            let total_count = staged + unstaged + untracked;
            Ok(VcsStructuredStatus {
                raw: if include_entries {
                    stdout
                } else {
                    String::new()
                },
                branch: VcsStatusBranchInfo {
                    summary_line: "jj status".to_string(),
                    branch: None,
                    upstream: None,
                    ahead: 0,
                    behind: 0,
                    detached: false,
                },
                entries,
                staged,
                unstaged,
                untracked,
                total_count,
                truncated: false,
            })
        })
    }

    fn build_worktree_patch<'a>(
        &'a self,
        worktree_path: &'a Path,
        base_revision: &'a str,
    ) -> VcsFuture<'a, WorktreePatch> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            let output = run_jj(worktree_path, &["status"]).await?;
            let parsed = parse_jj_status_output(&String::from_utf8_lossy(&output.stdout));
            let untracked = collect_jj_untracked_files(worktree_path, &parsed.untracked).await?;
            let head_revision = jj_rev_parse(worktree_path, "@")
                .await
                .unwrap_or_else(|_| base_revision.to_string());
            let diff_output = run_jj(
                worktree_path,
                &["diff", "--git", "--from", base_revision, "--to", "@"],
            )
            .await?;
            let tracked_diff = String::from_utf8_lossy(&diff_output.stdout).to_string();
            let mut patch_text = tracked_diff.clone();
            let mut changed_files = jj_diff_name_only(worktree_path, base_revision).await?;
            let mut seen: HashSet<String> = changed_files.iter().cloned().collect();
            let (mut file_count, mut line_additions, line_deletions) =
                diff_summary_from_git(&tracked_diff);
            for file in &untracked {
                if should_ignore_path(Path::new(file)) {
                    continue;
                }
                let diff = git_output_allow(
                    worktree_path,
                    &["diff", "--binary", "--no-index", "--", "/dev/null", file],
                    &[0, 1],
                )
                .await?;
                if !diff.is_empty() {
                    patch_text.push_str(&diff);
                }
                if seen.insert(file.clone()) {
                    changed_files.push(file.clone());
                    file_count += 1;
                    if let Ok(lines) = count_file_lines(&worktree_path.join(file)).await {
                        line_additions += lines;
                    }
                }
            }
            Ok(WorktreePatch {
                base_revision: base_revision.to_string(),
                head_revision: head_revision.trim().to_string(),
                patch: patch_text,
                changed_files,
                file_count,
                line_additions,
                line_deletions,
            })
        })
    }

    fn apply_patch<'a>(
        &'a self,
        root: &'a Path,
        patch: &'a str,
        target: ApplyPatchTarget,
        reverse: bool,
    ) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            if matches!(target, ApplyPatchTarget::Index) || reverse {
                return git::git_apply_patch(root, patch, target, reverse).await;
            }
            match jj_apply_patch(root, patch).await? {
                JjApplyOutcome::Applied => Ok(()),
                JjApplyOutcome::Unsupported => {
                    git::git_apply_patch(root, patch, target, reverse).await
                }
            }
        })
    }

    fn reset_worktree_to_revision<'a>(
        &'a self,
        root: &'a Path,
        revision: &'a str,
    ) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            let output = jj_command(root)
                .arg("edit")
                .arg(revision)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .context("running jj edit")?;
            if output.status.success() {
                return Ok(());
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            if jj_command_unsupported(&stderr) {
                bail!(
                    "jj edit is required to reset worktrees in jj repos, but this jj build does not support it: {}",
                    stderr.trim()
                );
            }
            bail!("jj edit failed: {}", stderr.trim())
        })
    }

    fn delete_branch<'a>(&'a self, root: &'a Path, branch: &'a str) -> VcsFuture<'a, ()> {
        Box::pin(async move {
            ensure_jj_usable().await?;
            let branch = branch.trim();
            if branch.is_empty() {
                return Ok(());
            }
            let output = jj_command(root)
                .arg("bookmark")
                .arg("delete")
                .arg(branch)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .context("running jj bookmark delete")?;
            if output.status.success() {
                return Ok(());
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            if jj_command_unsupported(&stderr) {
                bail!(
                    "jj bookmark delete is required to remove bookmarks in jj repos, but this jj build does not support it: {}",
                    stderr.trim()
                );
            }
            let lower = stderr.to_lowercase();
            if lower.contains("no such bookmark")
                || lower.contains("no matching bookmarks")
                || lower.contains("no bookmarks")
            {
                return Ok(());
            }
            bail!("jj bookmark delete failed: {}", stderr.trim())
        })
    }
}

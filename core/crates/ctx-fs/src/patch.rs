use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::git;

#[derive(Debug, Clone)]
pub struct WorktreePatch {
    pub base_revision: String,
    pub head_revision: String,
    pub patch: String,
    pub changed_files: Vec<String>,
    pub file_count: i64,
    pub line_additions: i64,
    pub line_deletions: i64,
}

pub fn should_ignore_path(path: &Path) -> bool {
    for component in path.components() {
        let name = component.as_os_str().to_string_lossy();
        if name == ".git"
            || name == ".jj"
            || name == ".ctx"
            || name == "node_modules"
            || name == "target"
            || name == "dist"
            || name == "build"
        {
            return true;
        }
    }
    false
}

pub async fn build_worktree_patch(
    workdir: impl AsRef<Path>,
    base_commit_sha: &str,
) -> Result<WorktreePatch> {
    let workdir = workdir.as_ref();
    let untracked = git::list_untracked_files(workdir).await?;
    build_worktree_patch_with_untracked(workdir, base_commit_sha, untracked).await
}

pub async fn build_worktree_patch_with_untracked(
    workdir: impl AsRef<Path>,
    base_commit_sha: &str,
    untracked: Vec<String>,
) -> Result<WorktreePatch> {
    let workdir = workdir.as_ref();
    let head_revision = git::rev_parse_head(workdir)
        .await
        .unwrap_or_else(|_| base_commit_sha.to_string());

    let mut patch =
        git_output_allow(workdir, &["diff", "--binary", base_commit_sha], &[0, 1]).await?;

    let changed_files_raw = git_output_allow(
        workdir,
        &["diff", "--name-only", "-z", base_commit_sha],
        &[0, 1],
    )
    .await?;

    let mut seen = std::collections::HashSet::new();
    let mut changed_files = Vec::new();
    for entry in changed_files_raw.split_terminator('\0') {
        if entry.is_empty() {
            continue;
        }
        if should_ignore_path(Path::new(entry)) {
            continue;
        }
        if seen.insert(entry.to_string()) {
            changed_files.push(entry.to_string());
        }
    }

    for file in &untracked {
        if should_ignore_path(Path::new(file)) {
            continue;
        }
        let diff = git_output_allow(
            workdir,
            &["diff", "--binary", "--no-index", "--", "/dev/null", file],
            &[0, 1],
        )
        .await?;
        if !diff.is_empty() {
            patch.push_str(&diff);
        }
        if seen.insert(file.clone()) {
            changed_files.push(file.clone());
        }
    }

    let (file_count, line_additions, line_deletions) =
        diff_stats(workdir, base_commit_sha, &untracked).await?;

    Ok(WorktreePatch {
        base_revision: base_commit_sha.to_string(),
        head_revision: head_revision.trim().to_string(),
        patch,
        changed_files,
        file_count,
        line_additions,
        line_deletions,
    })
}

async fn diff_stats(workdir: &Path, base: &str, untracked: &[String]) -> Result<(i64, i64, i64)> {
    let output = git_output_allow(workdir, &["diff", "--numstat", "-z", base], &[0, 1]).await?;
    let mut files = 0i64;
    let mut additions = 0i64;
    let mut deletions = 0i64;
    let mut parts = output.split_terminator('\0');
    while let (Some(add), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) {
        let path = path.trim();
        if path.is_empty() || should_ignore_path(Path::new(path)) {
            continue;
        }
        let add_count = add.parse::<i64>().unwrap_or(0);
        let del_count = del.parse::<i64>().unwrap_or(0);
        files += 1;
        additions += add_count;
        deletions += del_count;
    }

    for file in untracked {
        if should_ignore_path(Path::new(file)) {
            continue;
        }
        let output = git_output_allow(
            workdir,
            &[
                "diff",
                "--numstat",
                "-z",
                "--no-index",
                "--",
                "/dev/null",
                file,
            ],
            &[0, 1],
        )
        .await?;
        let mut parts = output.split_terminator('\0');
        while let (Some(add), Some(del), Some(_path)) = (parts.next(), parts.next(), parts.next()) {
            let add_count = add.parse::<i64>().unwrap_or(0);
            let del_count = del.parse::<i64>().unwrap_or(0);
            files += 1;
            additions += add_count;
            deletions += del_count;
        }
    }

    Ok((files, additions, deletions))
}

async fn git_output_allow(root: &Path, args: &[&str], ok_codes: &[i32]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("running git {args:?}"))?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        if !ok_codes.contains(&code) {
            return Err(anyhow::anyhow!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

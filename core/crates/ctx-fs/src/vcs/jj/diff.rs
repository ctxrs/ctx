use super::*;
use crate::patch::should_ignore_path;
use std::collections::HashSet;
use std::io::ErrorKind;

pub(super) async fn git_output_allow(
    root: &Path,
    args: &[&str],
    ok_codes: &[i32],
) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| {
            if err.kind() == ErrorKind::NotFound {
                anyhow::anyhow!("git is required to generate diffs for untracked files in jj repos")
            } else {
                anyhow::anyhow!("running git {args:?} failed: {err}")
            }
        })?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        if !ok_codes.contains(&code) {
            bail!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(super) async fn jj_diff_name_only(root: &Path, base_revision: &str) -> Result<Vec<String>> {
    let output = run_jj(
        root,
        &["diff", "--name-only", "--from", base_revision, "--to", "@"],
    )
    .await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for line in stdout.lines() {
        let path = line.trim();
        if path.is_empty() {
            continue;
        }
        if should_ignore_path(Path::new(path)) {
            continue;
        }
        if seen.insert(path.to_string()) {
            out.push(path.to_string());
        }
    }
    Ok(out)
}

pub(super) async fn jj_diff_name_count(root: &Path, base_revision: &str) -> Result<i64> {
    let output = run_jj(
        root,
        &["diff", "--name-only", "--from", base_revision, "--to", "@"],
    )
    .await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut total = 0;
    let mut seen = HashSet::new();
    for line in stdout.lines() {
        let path = line.trim();
        if path.is_empty() {
            continue;
        }
        if should_ignore_path(Path::new(path)) {
            continue;
        }
        if seen.insert(path.to_string()) {
            total += 1;
        }
    }
    Ok(total)
}

pub(super) async fn jj_diff_name_only_paths(
    root: &Path,
    base_revision: &str,
    paths: &[String],
) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut cmd = jj_command(root);
    cmd.arg("diff")
        .arg("--name-only")
        .arg("--from")
        .arg(base_revision)
        .arg("--to")
        .arg("@")
        .arg("--");
    for path in paths {
        cmd.arg(path);
    }
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running jj diff --name-only -- <paths>")?;
    if !output.status.success() {
        bail!(
            "jj diff --name-only failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for line in stdout.lines() {
        let path = line.trim();
        if path.is_empty() {
            continue;
        }
        if should_ignore_path(Path::new(path)) {
            continue;
        }
        if seen.insert(path.to_string()) {
            out.push(path.to_string());
        }
    }
    Ok(out)
}

pub(super) async fn count_file_lines(path: &Path) -> Result<i64> {
    let bytes = fs::read(path).await?;
    if bytes.contains(&0) {
        return Ok(0);
    }
    let mut lines = bytes.iter().filter(|b| **b == b'\n').count() as i64;
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        lines += 1;
    }
    Ok(lines)
}

pub(super) fn diff_summary_from_git(diff: &str) -> (i64, i64, i64) {
    let mut files = 0i64;
    let mut additions = 0i64;
    let mut deletions = 0i64;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            files += 1;
            continue;
        }
        if line.starts_with("+++ ") || line.starts_with("--- ") {
            continue;
        }
        if let Some(first) = line.as_bytes().first() {
            match first {
                b'+' => additions += 1,
                b'-' => deletions += 1,
                _ => {}
            }
        }
    }
    (files, additions, deletions)
}

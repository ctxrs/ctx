use std::path::Path;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use super::{VcsStatusBranchInfo, VcsStatusEntry, VcsStructuredStatus};

pub async fn git_status_structured(
    root_path: impl AsRef<Path>,
    include_untracked_files: bool,
    include_entries: bool,
) -> Result<VcsStructuredStatus> {
    let untracked_mode = if include_untracked_files {
        "--untracked-files=all"
    } else {
        "--untracked-files=normal"
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("status")
        .arg("--porcelain")
        .arg("-z")
        .arg("--branch")
        .arg(untracked_mode)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git status --porcelain -z --branch")?;
    if !output.status.success() {
        bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(git_status_structured_from_bytes_with_entries(
        &output.stdout,
        include_entries,
    ))
}

pub fn git_status_structured_from_bytes(bytes: &[u8]) -> VcsStructuredStatus {
    git_status_structured_from_bytes_with_entries(bytes, true)
}

pub fn git_status_structured_from_bytes_with_entries(
    bytes: &[u8],
    include_entries: bool,
) -> VcsStructuredStatus {
    parse_git_status_structured_bytes(bytes, include_entries)
}

pub async fn git_diff_name_status_paths(
    root_path: impl AsRef<Path>,
    base_revision: &str,
    paths: &[String],
) -> Result<Vec<(String, String, Option<String>)>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root_path.as_ref())
        .arg("diff")
        .arg("--name-status")
        .arg("-z")
        .arg(base_revision)
        .arg("--");
    for path in paths {
        cmd.arg(path);
    }
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git diff --name-status -z -- <paths>")?;
    if !output.status.success() {
        bail!(
            "git diff --name-status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(parse_git_diff_name_status_bytes(&output.stdout))
}

fn parse_git_status_structured_bytes(bytes: &[u8], include_entries: bool) -> VcsStructuredStatus {
    let mut entries = bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .peekable();
    let mut branch = VcsStatusBranchInfo::default();
    let mut parsed_entries = Vec::new();
    let mut staged = 0;
    let mut unstaged = 0;
    let mut untracked = 0;
    let mut total_count = 0;
    if let Some(first) = entries.peek() {
        let first = String::from_utf8_lossy(first);
        if first.starts_with("## ") {
            branch = parse_git_status_branch_info(&first);
            entries.next();
        }
    }
    while let Some(raw_bytes) = entries.next() {
        let raw = String::from_utf8_lossy(raw_bytes);
        let raw = raw.trim_end();
        if raw.len() < 3 {
            continue;
        }
        let bytes = raw.as_bytes();
        if bytes[2] != b' ' {
            continue;
        }
        let index_status = raw.chars().next().unwrap_or(' ');
        let worktree_status = raw.chars().nth(1).unwrap_or(' ');
        let path = raw[3..].trim();
        if path.is_empty() {
            continue;
        }
        let mut orig_path = None;
        if let Some(next_bytes) = entries
            .peek()
            .copied()
            .filter(|_| index_status == 'R' || index_status == 'C')
        {
            let next = String::from_utf8_lossy(next_bytes);
            let next = next.trim_end();
            if !looks_like_porcelain_status(next) && !next.is_empty() {
                if include_entries {
                    orig_path = Some(next.to_string());
                }
                entries.next();
            }
        }
        total_count += 1;
        if index_status == '?' && worktree_status == '?' {
            untracked += 1;
        } else {
            if index_status != ' ' {
                staged += 1;
            }
            if worktree_status != ' ' {
                unstaged += 1;
            }
        }
        if include_entries {
            parsed_entries.push(VcsStatusEntry {
                path: path.to_string(),
                orig_path,
                index_status: index_status.to_string(),
                worktree_status: worktree_status.to_string(),
            });
        }
    }
    VcsStructuredStatus {
        raw: if include_entries {
            String::from_utf8_lossy(bytes).to_string()
        } else {
            String::new()
        },
        branch,
        entries: parsed_entries,
        staged,
        unstaged,
        untracked,
        total_count,
        truncated: false,
    }
}

fn parse_git_status_branch_info(line: &str) -> VcsStatusBranchInfo {
    let mut info = VcsStatusBranchInfo {
        summary_line: line.trim().to_string(),
        branch: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        detached: false,
    };
    let Some(mut line) = line.trim().strip_prefix("## ") else {
        return info;
    };
    let mut counts_part = None;
    if let Some(idx) = line.find(" [") {
        counts_part = Some(line[idx + 2..].trim());
        line = line[..idx].trim();
    }
    if line.starts_with("HEAD") {
        info.detached = true;
    }
    if let Some((local, upstream)) = line.split_once("...") {
        if !local.trim().is_empty() {
            info.branch = Some(local.trim().to_string());
        }
        if !upstream.trim().is_empty() {
            info.upstream = Some(upstream.trim().to_string());
        }
    } else if !line.trim().is_empty() && !info.detached {
        info.branch = Some(line.trim().to_string());
    }
    if let Some(mut counts) = counts_part {
        if counts.ends_with(']') {
            counts = &counts[..counts.len() - 1];
        }
        for part in counts.split(',') {
            let mut iter = part.split_whitespace();
            let Some(kind) = iter.next() else {
                continue;
            };
            let Some(value) = iter.next() else {
                continue;
            };
            let count = value.parse::<i64>().unwrap_or(0);
            match kind {
                "ahead" => info.ahead = count,
                "behind" => info.behind = count,
                _ => {}
            }
        }
    }
    info
}

fn looks_like_porcelain_status(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3 && bytes[2] == b' '
}

pub(super) fn parse_git_diff_name_status_bytes(
    bytes: &[u8],
) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    let mut parts = bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .peekable();
    while let Some(part) = parts.next() {
        let status = part.trim().to_string();
        if status.is_empty() {
            continue;
        }
        let Some(path) = parts.next() else {
            continue;
        };
        if status.is_empty() || path.trim().is_empty() {
            continue;
        }
        let status_char = status.chars().next().unwrap_or('M');
        if status_char == 'R' || status_char == 'C' {
            let Some(next_path) = parts.next() else {
                continue;
            };
            let new_path = next_path;
            if new_path.trim().is_empty() {
                continue;
            }
            out.push((status, new_path, Some(path)));
        } else {
            out.push((status, path, None));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        git_status_structured_from_bytes, git_status_structured_from_bytes_with_entries,
        parse_git_diff_name_status_bytes,
    };

    #[test]
    fn parse_git_diff_name_status_handles_regular_entries() {
        let parsed = parse_git_diff_name_status_bytes(b"M\0file.txt\0A\0new.txt\0");
        assert_eq!(
            parsed,
            vec![
                ("M".to_string(), "file.txt".to_string(), None),
                ("A".to_string(), "new.txt".to_string(), None),
            ]
        );
    }

    #[test]
    fn parse_git_diff_name_status_handles_rename_entries() {
        let parsed = parse_git_diff_name_status_bytes(b"R100\0old.txt\0new.txt\0");
        assert_eq!(
            parsed,
            vec![(
                "R100".to_string(),
                "new.txt".to_string(),
                Some("old.txt".to_string()),
            )]
        );
    }

    #[test]
    fn parse_git_status_structured_handles_rename_entries() {
        let parsed = git_status_structured_from_bytes(b"## main\0R  new.txt\0old.txt\0");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].path, "new.txt");
        assert_eq!(parsed.entries[0].orig_path.as_deref(), Some("old.txt"));
        assert_eq!(parsed.staged, 1);
        assert_eq!(parsed.unstaged, 0);
    }

    #[test]
    fn parse_git_status_structured_can_skip_entries() {
        let parsed = git_status_structured_from_bytes_with_entries(
            b"## main\0M  file.txt\0?? new.txt\0",
            false,
        );
        assert!(parsed.entries.is_empty());
        assert_eq!(parsed.total_count, 2);
        assert_eq!(parsed.staged, 1);
        assert_eq!(parsed.untracked, 1);
        assert!(parsed.raw.is_empty());
    }
}

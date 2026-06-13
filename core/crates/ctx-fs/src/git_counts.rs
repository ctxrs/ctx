use std::path::Path;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

pub async fn diff_name_status_count(root_path: impl AsRef<Path>, base: &str) -> Result<i64> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("diff")
        .arg("--name-status")
        .arg("-z")
        .arg(base)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git diff --name-status")?;
    if !output.status.success() {
        bail!(
            "git diff --name-status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(count_name_status_entries(&output.stdout))
}

pub async fn untracked_count(root_path: impl AsRef<Path>) -> Result<i64> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("ls-files")
        .arg("--others")
        .arg("--exclude-standard")
        .arg("-z")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git ls-files --others")?;
    if !output.status.success() {
        bail!(
            "git ls-files --others failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .count() as i64)
}

fn count_name_status_entries(bytes: &[u8]) -> i64 {
    let mut total = 0;
    let mut parts = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty());
    while let Some(status_bytes) = parts.next() {
        let status = String::from_utf8_lossy(status_bytes);
        let status = status.trim();
        if status.is_empty() {
            continue;
        }
        let Some(path) = parts.next() else {
            continue;
        };
        if String::from_utf8_lossy(path).trim().is_empty() {
            continue;
        }
        let status_char = status.chars().next().unwrap_or('M');
        if status_char == 'R' || status_char == 'C' {
            let Some(next_path) = parts.next() else {
                continue;
            };
            if String::from_utf8_lossy(next_path).trim().is_empty() {
                continue;
            }
        }
        total += 1;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::count_name_status_entries;

    #[test]
    fn counts_regular_and_rename_entries() {
        let bytes = b"M\0file.txt\0A\0new.txt\0R100\0old.txt\0renamed.txt\0";
        assert_eq!(count_name_status_entries(bytes), 3);
    }

    #[test]
    fn skips_incomplete_rename_entries() {
        let bytes = b"M\0file.txt\0R100\0old.txt\0";
        assert_eq!(count_name_status_entries(bytes), 1);
    }
}

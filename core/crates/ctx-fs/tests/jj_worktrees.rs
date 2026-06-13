use std::path::Path;

use tokio::process::Command;

use ctx_fs::vcs;
use ctx_fs::worktrees::{create_worktree, diff_worktree_summary, remove_worktree};

const JJ_MIN_VERSION: (u64, u64, u64) = (0, 25, 0);

fn parse_jj_version(output: &str) -> Option<(u64, u64, u64)> {
    for token in output.split_whitespace() {
        let token = token.trim_start_matches('v');
        let mut version = String::new();
        let mut saw_digit = false;
        for ch in token.chars() {
            if ch.is_ascii_digit() {
                saw_digit = true;
                version.push(ch);
                continue;
            }
            if ch == '.' && saw_digit {
                version.push(ch);
                continue;
            }
            break;
        }
        if version.is_empty() {
            continue;
        }
        let parts = version.split('.').collect::<Vec<_>>();
        if parts.len() < 2 {
            continue;
        }
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let patch = parts.get(2).and_then(|part| part.parse().ok()).unwrap_or(0);
        return Some((major, minor, patch));
    }
    None
}

async fn jj_available() -> bool {
    Command::new("jj")
        .arg("--version")
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| parse_jj_version(&String::from_utf8_lossy(&output.stdout)))
        .map(|version| version >= JJ_MIN_VERSION)
        .unwrap_or(false)
}

async fn init_jj_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_jj_repo_root(root).await;

    for (path, contents) in files {
        let p = root.join(path);
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(p, *contents).await.unwrap();
    }
    let tracked_files: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
    track_jj_files(root, &tracked_files).await;

    dir
}

async fn init_jj_repo_root(root: &Path) {
    let output = Command::new("jj")
        .current_dir(root)
        .args(["git", "init"])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "jj git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join(".jj").exists());
}

async fn run_jj(root: &Path, args: &[&str]) -> String {
    let output = Command::new("jj")
        .arg("-R")
        .arg(root)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "jj {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

async fn track_jj_files(root: &Path, files: &[&str]) {
    if files.is_empty() {
        return;
    }
    let filesets: Vec<String> = files.iter().map(|path| format!("root:{path}")).collect();
    let output = Command::new("jj")
        .arg("-R")
        .arg(root)
        .args(["file", "track"])
        .args(filesets.iter().map(String::as_str))
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "jj file track failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn jj_worktree_create_remove() {
    if !jj_available().await {
        eprintln!("skipping jj_worktree_create_remove: jj not installed or too old");
        return;
    }

    let repo = init_jj_repo(&[("file.txt", "hello\n")]).await;
    let base_commit = vcs::driver_for_path(repo.path())
        .await
        .unwrap()
        .rev_parse_head(repo.path())
        .await
        .unwrap();
    let worktree_parent = tempfile::tempdir().unwrap();
    let worktree_path = worktree_parent.path().join("jj-worktree");

    create_worktree(repo.path(), &worktree_path, &base_commit, "jj-worktree")
        .await
        .unwrap();
    assert!(tokio::fs::metadata(&worktree_path).await.is_ok());
    let root = run_jj(&worktree_path, &["root"]).await;
    let expected_root = tokio::fs::canonicalize(&worktree_path).await.unwrap();
    let actual_root = tokio::fs::canonicalize(Path::new(root.trim()))
        .await
        .unwrap();
    if actual_root != expected_root {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let expected_meta = tokio::fs::metadata(&expected_root).await.unwrap();
            let actual_meta = tokio::fs::metadata(&actual_root).await.unwrap();
            assert_eq!(expected_meta.dev(), actual_meta.dev());
            assert_eq!(expected_meta.ino(), actual_meta.ino());
        }
        #[cfg(not(unix))]
        assert_eq!(actual_root, expected_root);
    }

    remove_worktree(repo.path(), &worktree_path).await.unwrap();
    assert!(tokio::fs::metadata(&worktree_path).await.is_err());
}

#[tokio::test]
async fn jj_diff_worktree_summary_counts_untracked() {
    if !jj_available().await {
        eprintln!(
            "skipping jj_diff_worktree_summary_counts_untracked: jj not installed or too old"
        );
        return;
    }

    let repo = init_jj_repo(&[("tracked.txt", "line1\n")]).await;
    let base_commit = vcs::driver_for_path(repo.path())
        .await
        .unwrap()
        .rev_parse_head(repo.path())
        .await
        .unwrap();

    tokio::fs::write(repo.path().join("tracked.txt"), "line1\nline2\nline3\n")
        .await
        .unwrap();
    tokio::fs::write(repo.path().join("new.txt"), "alpha\nbeta\n")
        .await
        .unwrap();

    let (files, additions, deletions) = diff_worktree_summary(repo.path(), &base_commit)
        .await
        .unwrap();
    assert_eq!(files, 2);
    assert_eq!(additions, 4);
    assert_eq!(deletions, 0);
}

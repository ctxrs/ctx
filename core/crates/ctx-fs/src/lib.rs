pub mod git;
mod git_counts;
pub mod patch;
pub mod paths;
pub mod permissions;
pub mod vcs;
pub mod worktrees;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tokio::process::Command;

    use crate::git::{assert_git_repo, rev_parse_head};
    use crate::worktrees::{create_worktree, diff_worktree, standaloneize_worktree_git_dir};

    async fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    async fn init_repo(root: &Path) {
        run_git(root, &["init"]).await;
        run_git(root, &["symbolic-ref", "HEAD", "refs/heads/main"]).await;
    }

    #[tokio::test]
    async fn worktree_diff_shows_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        init_repo(root).await;
        run_git(root, &["config", "user.email", "test@example.com"]).await;
        run_git(root, &["config", "user.name", "Test"]).await;

        fs::write(root.join("file.txt"), "hello\n").unwrap();
        run_git(root, &["add", "."]).await;
        run_git(root, &["commit", "-m", "init"]).await;

        assert_git_repo(root).await.unwrap();
        let base = rev_parse_head(root).await.unwrap();

        let wt_path = root.join("wt1");
        create_worktree(root, &wt_path, &base, "ctx/test")
            .await
            .unwrap();

        fs::write(wt_path.join("file.txt"), "hello\nworld\n").unwrap();

        let diff = diff_worktree(&wt_path, &base).await.unwrap();
        assert!(diff.contains("+world"));
    }

    #[tokio::test]
    async fn standaloneized_worktree_survives_source_repo_removal() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path().join("repo");
        tokio::fs::create_dir_all(&repo_root).await.unwrap();

        init_repo(&repo_root).await;
        run_git(&repo_root, &["config", "user.email", "test@example.com"]).await;
        run_git(&repo_root, &["config", "user.name", "Test"]).await;

        fs::write(repo_root.join("file.txt"), "hello\n").unwrap();
        run_git(&repo_root, &["add", "."]).await;
        run_git(&repo_root, &["commit", "-m", "init"]).await;

        let base = rev_parse_head(&repo_root).await.unwrap();
        let managed_root = dir.path().join("managed");
        create_worktree(&repo_root, &managed_root, &base, "ctx/test")
            .await
            .unwrap();
        standaloneize_worktree_git_dir(&managed_root).await.unwrap();

        assert!(managed_root.join(".git").is_dir());
        tokio::fs::remove_dir_all(&repo_root).await.unwrap();

        let head = rev_parse_head(&managed_root).await.unwrap();
        assert_eq!(head, base);

        let shadow_root = dir.path().join("shadow");
        run_git(
            &managed_root,
            &[
                "worktree",
                "add",
                "-b",
                "ctx/shadow",
                shadow_root.to_str().unwrap(),
                &base,
            ],
        )
        .await;
        assert_git_repo(&shadow_root).await.unwrap();
    }
}

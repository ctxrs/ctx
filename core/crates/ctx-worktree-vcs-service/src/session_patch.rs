use std::path::Path;

use anyhow::Result;

pub async fn apply_worktree_vcs_session_patch(
    worktree_root: &Path,
    patch: &str,
    reverse: bool,
) -> Result<()> {
    ctx_fs::git::git_apply_patch(
        worktree_root,
        patch,
        ctx_fs::git::ApplyPatchTarget::Worktree,
        reverse,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn git(args: &[&str], cwd: &Path) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[tokio::test]
    async fn applies_and_reverses_worktree_patch() {
        let temp = tempfile::tempdir().expect("tempdir");
        git(&["init"], temp.path());
        git(&["symbolic-ref", "HEAD", "refs/heads/main"], temp.path());
        std::fs::write(temp.path().join("file.txt"), "one\n").expect("write file");
        let patch = "\
diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -1 +1 @@
-one
+two
";

        apply_worktree_vcs_session_patch(temp.path(), patch, false)
            .await
            .expect("apply patch");

        assert_eq!(
            std::fs::read_to_string(temp.path().join("file.txt")).expect("read file"),
            "two\n"
        );

        apply_worktree_vcs_session_patch(temp.path(), patch, true)
            .await
            .expect("reverse patch");

        assert_eq!(
            std::fs::read_to_string(temp.path().join("file.txt")).expect("read file"),
            "one\n"
        );
    }
}

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::validate_workspace_root_repo;

const CTX_PACK_TMP_GITIGNORE_LINE: &str = ".ctx/ctx-pack/tmp/";

pub async fn init_workspace(root: Option<String>) -> Result<()> {
    let root_path = root
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().context("getting current dir")?);

    init_workspace_at(&root_path).await
}

pub async fn init_workspace_at(root_path: &Path) -> Result<()> {
    validate_workspace_root_repo(root_path).await?;

    let context_dir = root_path.join(".ctx");
    let pack_dir = context_dir.join("ctx-pack");
    let tmp_dir = pack_dir.join("tmp");

    tokio::fs::create_dir_all(pack_dir.join("specs")).await?;
    tokio::fs::create_dir_all(pack_dir.join("prompts")).await?;
    tokio::fs::create_dir_all(pack_dir.join("docs")).await?;
    tokio::fs::create_dir_all(pack_dir.join("skills")).await?;
    tokio::fs::create_dir_all(&tmp_dir).await?;

    tokio::fs::create_dir_all(context_dir.join("exec-plans")).await?;
    ensure_ctx_pack_tmp_ignored(root_path).await
}

async fn ensure_ctx_pack_tmp_ignored(root_path: &Path) -> Result<()> {
    let gitignore_path = root_path.join(".gitignore");
    let mut gitignore = if gitignore_path.exists() {
        tokio::fs::read_to_string(&gitignore_path).await?
    } else {
        String::new()
    };

    if !gitignore
        .lines()
        .any(|line| line.trim() == CTX_PACK_TMP_GITIGNORE_LINE)
    {
        if !gitignore.ends_with('\n') && !gitignore.is_empty() {
            gitignore.push('\n');
        }
        gitignore.push_str(CTX_PACK_TMP_GITIGNORE_LINE);
        gitignore.push('\n');
        tokio::fs::write(&gitignore_path, gitignore).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::*;

    fn init_git_repo(root: &Path) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("init")
            .output()
            .expect("git init should run");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn init_workspace_creates_ctx_pack_layout_and_gitignore() {
        let temp = tempfile::tempdir().expect("tempdir");
        init_git_repo(temp.path());

        init_workspace_at(temp.path())
            .await
            .expect("init workspace");

        for relative in [
            ".ctx/ctx-pack/specs",
            ".ctx/ctx-pack/prompts",
            ".ctx/ctx-pack/docs",
            ".ctx/ctx-pack/skills",
            ".ctx/ctx-pack/tmp",
            ".ctx/exec-plans",
        ] {
            assert!(
                temp.path().join(relative).is_dir(),
                "expected {relative} to exist"
            );
        }

        let gitignore = tokio::fs::read_to_string(temp.path().join(".gitignore"))
            .await
            .expect("gitignore");
        assert!(gitignore
            .lines()
            .any(|line| line == CTX_PACK_TMP_GITIGNORE_LINE));
    }

    #[tokio::test]
    async fn init_workspace_gitignore_update_is_idempotent_and_preserves_existing_content() {
        let temp = tempfile::tempdir().expect("tempdir");
        init_git_repo(temp.path());
        tokio::fs::write(temp.path().join(".gitignore"), ".env")
            .await
            .expect("seed gitignore");

        init_workspace_at(temp.path()).await.expect("first init");
        init_workspace_at(temp.path()).await.expect("second init");

        let gitignore = tokio::fs::read_to_string(temp.path().join(".gitignore"))
            .await
            .expect("gitignore");
        assert_eq!(
            gitignore
                .lines()
                .filter(|line| line.trim() == CTX_PACK_TMP_GITIGNORE_LINE)
                .count(),
            1
        );
        assert_eq!(gitignore, ".env\n.ctx/ctx-pack/tmp/\n");
    }
}

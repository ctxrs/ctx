use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use ctx_sandbox_container_runtime::{sandbox_container_command, SandboxCommandMode};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

fn collect_tree_size_bytes(root: &Path) -> Result<u64> {
    fn recurse(path: &Path) -> Result<u64> {
        let meta = std::fs::symlink_metadata(path)
            .with_context(|| format!("reading {}", path.display()))?;
        if meta.is_file() {
            return Ok(meta.len());
        }
        if !meta.is_dir() {
            return Ok(0);
        }
        let mut total = 0u64;
        for entry in
            std::fs::read_dir(path).with_context(|| format!("reading {}", path.display()))?
        {
            let entry = entry.with_context(|| format!("reading {}", path.display()))?;
            total = total.saturating_add(recurse(&entry.path())?);
        }
        Ok(total)
    }

    recurse(root)
}

async fn estimate_tree_size_bytes(root: &Path) -> Result<u64> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || collect_tree_size_bytes(&root))
        .await
        .context("joining tree-size estimate task")?
}

pub(super) async fn estimate_self_contained_copy_size_bytes(source_root: &Path) -> Result<u64> {
    let source_bytes = estimate_tree_size_bytes(source_root).await?;
    let dotgit = source_root.join(".git");
    let dotgit_meta = match tokio::fs::symlink_metadata(&dotgit).await {
        Ok(meta) => meta,
        Err(_) => return Ok(source_bytes),
    };
    if dotgit_meta.is_dir() {
        return Ok(source_bytes);
    }

    // Conservative upper bound: staging replaces the lightweight `.git` pointer with a full
    // standalone `.git` directory assembled from the common git dir plus worktree-specific git
    // metadata. Summing both trees can slightly overestimate when files overlap, but it avoids
    // admitting copies that still fail during host-side staging.
    let git_dir = resolve_git_dir(source_root).await?;
    let common_git_dir = resolve_common_git_dir(&git_dir).await?;
    let mut expanded_bytes = source_bytes.saturating_sub(dotgit_meta.len());
    expanded_bytes =
        expanded_bytes.saturating_add(estimate_tree_size_bytes(&common_git_dir).await?);
    if git_dir != common_git_dir {
        expanded_bytes = expanded_bytes.saturating_add(estimate_tree_size_bytes(&git_dir).await?);
    }
    Ok(expanded_bytes)
}

fn host_tar_stream_command(src_root: &Path) -> Result<Option<Command>> {
    let entries = std::fs::read_dir(src_root)
        .with_context(|| format!("reading {}", src_root.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("reading {}", src_root.display()))?;
    if entries.is_empty() {
        return Ok(None);
    }

    #[cfg(target_os = "macos")]
    let mut tar_cmd = {
        let mut cmd = Command::new("bsdtar");
        cmd.arg("--format=pax").arg("--no-mac-metadata");
        cmd
    };
    #[cfg(not(target_os = "macos"))]
    let mut tar_cmd = Command::new("tar");

    tar_cmd
        .arg("-C")
        .arg(src_root)
        .arg("-cf")
        .arg("-")
        // Shared-VM guest exec still truncates explicit multi-entry archives like
        // `-- .git README.md`; archiving `.` is the stable shape now that `/ctx/ws`
        // is normalized to the execution user before import.
        .arg(".");
    tar_cmd.stdout(Stdio::piped());
    Ok(Some(tar_cmd))
}

pub(super) async fn stream_dir_to_container(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    src_root: &Path,
    dest_root: &Path,
) -> Result<()> {
    let mut sandbox_cmd = sandbox_container_command(data_root, mode)?;
    sandbox_cmd
        .arg("exec")
        .arg("--interactive")
        .arg("--workdir")
        .arg(dest_root)
        .arg(container_id)
        .arg("tar")
        .arg("-xf")
        .arg("-");
    sandbox_cmd.stdin(Stdio::piped());
    let mut sandbox_child = sandbox_cmd
        .spawn()
        .context("spawning sandbox exec tar for disk-isolated copy")?;
    let mut sandbox_in = sandbox_child
        .stdin
        .take()
        .context("taking sandbox exec stdin for disk-isolated copy")?;

    let Some(mut tar_cmd) = host_tar_stream_command(src_root)? else {
        return Ok(());
    };
    let mut tar_child = tar_cmd
        .spawn()
        .context("spawning tar for disk-isolated copy")?;
    let mut tar_out = tar_child
        .stdout
        .take()
        .context("taking tar stdout for disk-isolated copy")?;

    tokio::io::copy(&mut tar_out, &mut sandbox_in)
        .await
        .context("streaming disk-isolated tar archive into container")?;
    sandbox_in
        .shutdown()
        .await
        .context("closing sandbox exec stdin for disk-isolated copy")?;
    drop(sandbox_in);

    let tar_status = tar_child.wait().await.context("waiting on tar")?;
    if !tar_status.success() {
        anyhow::bail!("tar failed with status {tar_status}");
    }
    let out = sandbox_child
        .wait_with_output()
        .await
        .context("waiting on sandbox exec tar for disk-isolated copy")?;
    if !out.status.success() {
        anyhow::bail!(
            "sandbox exec tar failed (status {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

async fn resolve_git_dir(worktree_root: &Path) -> Result<PathBuf> {
    let dotgit = worktree_root.join(".git");
    let meta = tokio::fs::symlink_metadata(&dotgit)
        .await
        .with_context(|| format!("reading {}", dotgit.display()))?;
    if meta.is_dir() {
        return Ok(dotgit);
    }
    let txt = tokio::fs::read_to_string(&dotgit)
        .await
        .with_context(|| format!("reading {}", dotgit.display()))?;
    let line = txt
        .lines()
        .find(|value| value.trim_start().starts_with("gitdir:"))
        .ok_or_else(|| anyhow::anyhow!("invalid .git file: missing gitdir"))?;
    let raw = line.trim_start().trim_start_matches("gitdir:").trim();
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(worktree_root.join(path))
    }
}

async fn resolve_common_git_dir(git_dir: &Path) -> Result<PathBuf> {
    let commondir = git_dir.join("commondir");
    let meta = match tokio::fs::symlink_metadata(&commondir).await {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(git_dir.to_path_buf());
        }
        Err(err) => return Err(err).with_context(|| format!("reading {}", commondir.display())),
    };
    if !meta.is_file() {
        return Ok(git_dir.to_path_buf());
    }
    let raw = tokio::fs::read_to_string(&commondir)
        .await
        .with_context(|| format!("reading {}", commondir.display()))?;
    let path = PathBuf::from(raw.trim());
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(git_dir.join(path))
    }
}

#[cfg(unix)]
fn symlink_path(target: &Path, dest: &Path, _is_dir: bool) -> Result<()> {
    std::os::unix::fs::symlink(target, dest)?;
    Ok(())
}

#[cfg(windows)]
fn symlink_path(target: &Path, dest: &Path, is_dir: bool) -> Result<()> {
    if is_dir {
        std::os::windows::fs::symlink_dir(target, dest)?;
    } else {
        std::os::windows::fs::symlink_file(target, dest)?;
    }
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let entry_path = entry.path();
        let dest = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry_path, &dest)?;
        } else if file_type.is_symlink() {
            if dest.exists() {
                let _ = std::fs::remove_file(&dest);
                let _ = std::fs::remove_dir_all(&dest);
            }
            let link_target = std::fs::read_link(&entry_path)?;
            let is_dir = std::fs::metadata(&entry_path)
                .map(|meta| meta.is_dir())
                .unwrap_or(false);
            symlink_path(&link_target, &dest, is_dir)?;
        } else if file_type.is_file() {
            std::fs::copy(&entry_path, &dest)?;
        }
    }
    Ok(())
}

pub(super) async fn prepare_self_contained_copy_root(
    data_root: &Path,
    source_root: &Path,
) -> Result<(PathBuf, Option<TempDir>)> {
    let dotgit = source_root.join(".git");
    let dotgit_meta = match tokio::fs::symlink_metadata(&dotgit).await {
        Ok(meta) => meta,
        Err(_) => return Ok((source_root.to_path_buf(), None)),
    };
    if dotgit_meta.is_dir() {
        return Ok((source_root.to_path_buf(), None));
    }

    let git_dir = resolve_git_dir(source_root).await?;
    let common_git_dir = resolve_common_git_dir(&git_dir).await?;
    let staging_parent = data_root.join("disk-isolated").join("staging");
    tokio::fs::create_dir_all(&staging_parent)
        .await
        .with_context(|| format!("creating {}", staging_parent.display()))?;
    let staging = TempDir::new_in(&staging_parent)
        .with_context(|| format!("creating temp dir in {}", staging_parent.display()))?;
    let staging_root = staging.path().join("worktree");
    let source = source_root.to_path_buf();
    let git_dir_copy = git_dir.clone();
    let common_git_dir_copy = common_git_dir.clone();
    let staging_copy = staging_root.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        copy_dir_recursive(&source, &staging_copy)?;
        let staged_dotgit = staging_copy.join(".git");
        if staged_dotgit.exists() {
            if staged_dotgit.is_dir() {
                std::fs::remove_dir_all(&staged_dotgit)?;
            } else {
                std::fs::remove_file(&staged_dotgit)?;
            }
        }
        copy_dir_recursive(&common_git_dir_copy, &staged_dotgit)?;
        if git_dir_copy != common_git_dir_copy {
            copy_dir_recursive(&git_dir_copy, &staged_dotgit)?;
        }
        let commondir = staged_dotgit.join("commondir");
        if commondir.exists() {
            std::fs::remove_file(&commondir)?;
        }
        let gitdir = staged_dotgit.join("gitdir");
        if gitdir.exists() {
            std::fs::remove_file(&gitdir)?;
        }
        Ok(())
    })
    .await??;

    Ok((staging_root, Some(staging)))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent dir");
        }
        std::fs::write(path, contents).expect("write fixture file");
    }

    pub(crate) fn seed_linked_git_worktree_fixture(
        root: &Path,
        worktree_name: &str,
        branch_name: &str,
    ) -> (PathBuf, PathBuf) {
        let repo_root = root.join("repo");
        let worktree_root = root.join(worktree_name);
        let common_git_dir = repo_root.join(".git");
        let linked_git_dir = common_git_dir.join("worktrees").join(worktree_name);
        let worktree_dotgit = worktree_root.join(".git");
        let main_commit = "0123456789abcdef0123456789abcdef01234567\n";
        let branch_commit = "89abcdef0123456789abcdef0123456789abcdef\n";

        std::fs::create_dir_all(&worktree_root).expect("create worktree root");
        std::fs::write(worktree_root.join("README.md"), "hello\n").expect("write readme");

        write_file(&common_git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write_file(
            &common_git_dir.join("refs").join("heads").join("main"),
            main_commit,
        );
        write_file(
            &common_git_dir.join("config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
        );
        write_file(&common_git_dir.join("info").join("exclude"), "");
        write_file(
            &common_git_dir.join("objects").join("info").join("keep"),
            "",
        );

        write_file(
            &linked_git_dir.join("HEAD"),
            &format!("ref: refs/heads/{branch_name}\n"),
        );
        write_file(
            &linked_git_dir.join("refs").join("heads").join(branch_name),
            branch_commit,
        );
        write_file(&linked_git_dir.join("index"), "fixture-index");
        write_file(&linked_git_dir.join("commondir"), "../..\n");
        write_file(
            &linked_git_dir.join("gitdir"),
            &format!("{}\n", worktree_dotgit.display()),
        );

        write_file(
            &worktree_dotgit,
            &format!("gitdir: {}\n", linked_git_dir.display()),
        );

        (repo_root, worktree_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_sandbox_container_runtime::sandbox_cli_env_test_lock;
    use std::fs;

    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = self.prev.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test]
    async fn prepare_self_contained_copy_root_makes_git_worktree_clone_standalone() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (_repo_root, worktree_root) = test_support::seed_linked_git_worktree_fixture(
            temp.path(),
            "worktree",
            "ctx/test-worktree",
        );

        let (copy_root, _guard) = prepare_self_contained_copy_root(temp.path(), &worktree_root)
            .await
            .expect("prepare self-contained root");
        assert_ne!(copy_root, worktree_root);
        assert!(
            !worktree_root.join(".git").is_dir(),
            "source worktree must remain linked and unmodified"
        );
        assert!(copy_root.join(".git").is_dir());
        assert!(!copy_root.join(".git").join("commondir").exists());
        assert!(!copy_root.join(".git").join("gitdir").exists());
        assert_eq!(
            std::fs::read_to_string(copy_root.join(".git").join("HEAD")).expect("read staged HEAD"),
            "ref: refs/heads/ctx/test-worktree\n"
        );
        assert_eq!(
            std::fs::read_to_string(
                copy_root
                    .join(".git")
                    .join("refs")
                    .join("heads")
                    .join("main")
            )
            .expect("read staged main ref"),
            "0123456789abcdef0123456789abcdef01234567\n"
        );
        assert!(
            copy_root.join(".git").join("index").exists(),
            "worktree-specific git metadata should be copied into the staged standalone .git dir"
        );
        assert_eq!(
            resolve_git_dir(&copy_root)
                .await
                .expect("resolve staged git dir"),
            copy_root.join(".git")
        );
        assert_eq!(
            resolve_common_git_dir(&copy_root.join(".git"))
                .await
                .expect("resolve staged common git dir"),
            copy_root.join(".git")
        );
    }

    #[tokio::test]
    async fn estimate_self_contained_copy_size_accounts_for_expanded_git_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (_repo_root, worktree_root) = test_support::seed_linked_git_worktree_fixture(
            temp.path(),
            "worktree",
            "ctx/test-size-estimate",
        );

        let source_bytes = collect_tree_size_bytes(&worktree_root).expect("measure worktree");
        let estimated_bytes = estimate_self_contained_copy_size_bytes(&worktree_root)
            .await
            .expect("estimate self-contained worktree");
        let (copy_root, _guard) = prepare_self_contained_copy_root(temp.path(), &worktree_root)
            .await
            .expect("prepare self-contained root");
        let staged_bytes = collect_tree_size_bytes(&copy_root).expect("measure staged copy");

        assert!(estimated_bytes > source_bytes);
        assert!(estimated_bytes >= staged_bytes);
    }

    #[tokio::test]
    async fn stream_dir_to_container_uses_tar_exec_instead_of_container_cp() {
        let _env_lock = sandbox_cli_env_test_lock().lock().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let log_path = temp.path().join("sandbox-cli.log");
        let cli_path = temp.path().join("fake-sandbox-cli.sh");
        fs::write(
            &cli_path,
            format!(
                "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" >> '{log_path}'\ncmd=\"$1\"\nshift\nif [ \"$cmd\" != \"exec\" ]; then\n  echo \"unexpected sandbox cli command: $cmd\" >&2\n  exit 1\nfi\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --interactive)\n      shift\n      ;;\n    --workdir)\n      workdir=\"$2\"\n      shift 2\n      ;;\n    *)\n      break\n      ;;\n  esac\ndone\ncontainer_id=\"$1\"\nshift\ncommand=\"$1\"\nshift\nif [ \"$command\" != \"tar\" ]; then\n  echo \"unexpected exec command: $command\" >&2\n  exit 1\nfi\ncat >/dev/null\nexit 0\n",
                log_path = log_path.display(),
            ),
        )
        .expect("write fake sandbox cli");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = fs::metadata(&cli_path).expect("metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&cli_path, perms).expect("chmod fake sandbox cli");
        }

        let src = temp.path().join("src");
        fs::create_dir_all(&src).expect("create src dir");
        fs::write(src.join("file.txt"), "hello\n").expect("write src file");
        fs::create_dir_all(src.join(".git")).expect("create git dir");
        fs::write(src.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("write git head");

        let _cli = EnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", &cli_path);

        stream_dir_to_container(
            temp.path(),
            &ctx_sandbox_container_runtime::SandboxCommandMode::NativeContainer,
            "ctx-harness-test",
            &src,
            Path::new("/ctx/ws"),
        )
        .await
        .expect("stream dir to container");

        let log = fs::read_to_string(&log_path).expect("read sandbox cli log");
        assert!(log.contains("exec --interactive --workdir /ctx/ws ctx-harness-test tar -xf -"));
        assert!(!log.contains(" cp "));

        let tar_cmd = host_tar_stream_command(&src)
            .expect("build host tar stream command")
            .expect("non-empty archive command");
        let program = tar_cmd.as_std().get_program().to_string_lossy().to_string();
        let args = tar_cmd
            .as_std()
            .get_args()
            .map(|value| value.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        #[cfg(target_os = "macos")]
        {
            assert_eq!(program, "bsdtar");
            assert!(
                args.starts_with(&["--format=pax".to_string(), "--no-mac-metadata".to_string(),])
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(program, "tar");
        }
        assert!(args.contains(&".".to_string()));
        assert!(!args.contains(&".git".to_string()));
        assert!(!args.contains(&"file.txt".to_string()));
    }
}

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use ctx_sandbox_container_runtime::{
    command_output_with_timeout, sandbox_container_command, SandboxCommandMode,
};

const SANDBOX_EXEC_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) async fn remove_live_worktree_root(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    live_worktree_root: &Path,
) -> Result<()> {
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--interactive")
        .arg(container_id)
        .arg("rm")
        .arg("-rf")
        .arg("--")
        .arg(live_worktree_root);
    let out = command_output_with_timeout(cmd, SANDBOX_EXEC_TIMEOUT)
        .await
        .context("sandbox exec rm -rf disk-isolated worktree")?;
    if !out.status.success() {
        anyhow::bail!(
            "failed to remove disk-isolated worktree root {} (status {}): {}",
            live_worktree_root.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub(super) async fn verify_container_git_repo(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    worktree_root: &Path,
) -> Result<()> {
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--interactive")
        .arg("--workdir")
        .arg(worktree_root)
        .arg(container_id)
        .arg("sh")
        .arg("-lc")
        .arg("git rev-parse --is-inside-work-tree && git rev-parse HEAD >/dev/null");
    let out = command_output_with_timeout(cmd, SANDBOX_EXEC_TIMEOUT)
        .await
        .context("sandbox exec git repo verification")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            "unknown error".to_string()
        };
        anyhow::bail!(
            "disk-isolated worktree verification failed (status {}): {}",
            out.status,
            detail
        );
    }
    Ok(())
}

pub(super) async fn ensure_directory(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    dest_root: &Path,
) -> Result<()> {
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--user")
        .arg("0")
        .arg(container_id)
        .arg("mkdir")
        .arg("-p")
        .arg("--")
        .arg(dest_root);
    let out = command_output_with_timeout(cmd, SANDBOX_EXEC_TIMEOUT)
        .await
        .context("sandbox exec mkdir")?;
    if !out.status.success() {
        anyhow::bail!(
            "failed to create disk-isolated worktree dir (status {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    let uid = resolve_container_exec_id(data_root, mode, container_id, "-u")
        .await
        .context("resolving sandbox exec uid for disk-isolated worktree root")?;
    let gid = resolve_container_exec_id(data_root, mode, container_id, "-g")
        .await
        .context("resolving sandbox exec gid for disk-isolated worktree root")?;
    let mut chown = sandbox_container_command(data_root, mode)?;
    chown
        .arg("exec")
        .arg("--user")
        .arg("0")
        .arg(container_id)
        .arg("chown")
        .arg(format!("{uid}:{gid}"))
        .arg(dest_root);
    let out = command_output_with_timeout(chown, SANDBOX_EXEC_TIMEOUT)
        .await
        .context("sandbox exec chown disk-isolated worktree root")?;
    if !out.status.success() {
        anyhow::bail!(
            "failed to set disk-isolated worktree root owner {} to {}:{} (status {}): {}",
            dest_root.display(),
            uid,
            gid,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub(super) async fn ensure_empty_container_root(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    dest_root: &Path,
) -> Result<()> {
    let mut normalize = sandbox_container_command(data_root, mode)?;
    normalize
        .arg("exec")
        .arg("--user")
        .arg("0")
        .arg(container_id)
        .arg("sh")
        .arg("-lc")
        .arg(r#"mkdir -p -- "$1" && chmod 0777 "$1""#)
        .arg("sh")
        .arg(dest_root);
    let out = command_output_with_timeout(normalize, SANDBOX_EXEC_TIMEOUT)
        .await
        .context("sandbox exec normalize disk-isolated root")?;
    if !out.status.success() {
        anyhow::bail!(
            "failed to normalize disk-isolated root {} (status {}): {}",
            dest_root.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    let mut clear = sandbox_container_command(data_root, mode)?;
    clear
        .arg("exec")
        .arg("--user")
        .arg("0")
        .arg(container_id)
        .arg("sh")
        .arg("-lc")
        .arg(r#"find "$1" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +"#)
        .arg("sh")
        .arg(dest_root);
    let out = command_output_with_timeout(clear, SANDBOX_EXEC_TIMEOUT)
        .await
        .context("sandbox exec clear disk-isolated root")?;
    if !out.status.success() {
        anyhow::bail!(
            "failed to clear disk-isolated root {} (status {}): {}",
            dest_root.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    let uid = resolve_container_exec_id(data_root, mode, container_id, "-u")
        .await
        .context("resolving sandbox exec uid for disk-isolated root")?;
    let gid = resolve_container_exec_id(data_root, mode, container_id, "-g")
        .await
        .context("resolving sandbox exec gid for disk-isolated root")?;
    let mut chown = sandbox_container_command(data_root, mode)?;
    chown
        .arg("exec")
        .arg("--user")
        .arg("0")
        .arg(container_id)
        .arg("chown")
        .arg(format!("{uid}:{gid}"))
        .arg(dest_root);
    let out = command_output_with_timeout(chown, SANDBOX_EXEC_TIMEOUT)
        .await
        .context("sandbox exec chown disk-isolated root")?;
    if !out.status.success() {
        anyhow::bail!(
            "failed to set disk-isolated root owner {} to {}:{} (status {}): {}",
            dest_root.display(),
            uid,
            gid,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    Ok(())
}

pub(super) async fn best_effort_make_user_writable(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    dest_root: &Path,
) -> Result<()> {
    let mut chmod = sandbox_container_command(data_root, mode)?;
    chmod
        .arg("exec")
        .arg("--interactive")
        .arg("--workdir")
        .arg(dest_root)
        .arg(container_id)
        .arg("sh")
        .arg("-lc")
        .arg("chmod -R u+rwX . >/dev/null 2>&1 || true");
    let _ = command_output_with_timeout(chmod, SANDBOX_EXEC_TIMEOUT).await;
    Ok(())
}

pub(super) async fn checkout_branch_at_base(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    dest_root: &Path,
    branch_name: &str,
    base_commit_sha: &str,
) -> Result<()> {
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--interactive")
        .arg("--workdir")
        .arg(dest_root)
        .arg(container_id)
        .arg("git")
        .arg("checkout")
        .arg("-B")
        .arg(branch_name)
        .arg(base_commit_sha);
    let out = command_output_with_timeout(cmd, SANDBOX_EXEC_TIMEOUT)
        .await
        .context("sandbox exec git checkout")?;
    if !out.status.success() {
        anyhow::bail!(
            "git checkout failed (status {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

async fn resolve_container_exec_id(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    id_flag: &str,
) -> Result<u32> {
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--interactive")
        .arg(container_id)
        .arg("id")
        .arg(id_flag);
    let out = command_output_with_timeout(cmd, SANDBOX_EXEC_TIMEOUT)
        .await
        .with_context(|| format!("sandbox exec id {id_flag}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "failed to resolve sandbox exec id {} (status {}): {}",
            id_flag,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .with_context(|| format!("parsing sandbox exec id {id_flag} output"))
}

use super::*;
use std::process::Stdio;

use tokio::process::Command;

pub(super) async fn container_path_exists(
    data_root: &Path,
    container_id: &str,
    path: &Path,
) -> Result<bool> {
    let mut cmd = sandbox_container_command(data_root)?;
    cmd.arg("exec")
        .arg("--interactive")
        .arg(container_id)
        .arg("test")
        .arg("-e")
        .arg("--")
        .arg(path);
    let out = cmd.output().await.context("sandbox exec test -e")?;
    Ok(out.status.success())
}

pub(super) async fn container_rm_rf(
    data_root: &Path,
    container_id: &str,
    path: &Path,
) -> Result<()> {
    let mut cmd = sandbox_container_command(data_root)?;
    cmd.arg("exec")
        .arg("--interactive")
        .arg(container_id)
        .arg("rm")
        .arg("-rf")
        .arg("--")
        .arg(path);
    let out = cmd.output().await.context("sandbox exec rm -rf")?;
    if out.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "container rm -rf failed (status {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
}

pub(super) fn container_mount_script() -> String {
    format!(
        r#"set -eu
{}
ensure_mount_parent_chain "$1" "$2"
root="$1"
target="$2"
source="$3"
mode="$4"
target_parent="${{target%/*}}"
target_name="${{target##*/}}"
stage=""
temp=""
backup=""
cleanup_stage() {{
  if [ -n "${{stage:-}}" ] && {{ [ -L "$stage" ] || [ -e "$stage" ]; }}; then
    if [ ! -L "$stage" ]; then
      chmod -R u+w -- "$stage" 2>/dev/null || true
    fi
    rm -rf -- "$stage"
  fi
}}
make_stage() {{
  stage="$(mktemp -d "$target_parent/.${{target_name}}.tmp.XXXXXX")"
  temp="$stage/payload"
}}
finish_stage() {{
  backup=""
  if [ -L "$target" ] || [ -e "$target" ]; then
    backup="$(mktemp -d "$target_parent/.${{target_name}}.old.XXXXXX")"
    rmdir -- "$backup"
    mv -- "$target" "$backup"
  fi
  if mv -- "$temp" "$target"; then
    :
  else
    status="$?"
    if [ -n "$backup" ]; then
      mv -- "$backup" "$target" || printf 'failed to restore previous attachment mount: %s\n' "$target" >&2
    fi
    return "$status"
  fi
  if [ -n "$backup" ]; then
    if [ ! -L "$backup" ]; then
      chmod -R u+w -- "$backup" 2>/dev/null || true
    fi
    rm -rf -- "$backup"
  fi
  rmdir -- "$stage"
  stage=""
}}
trap cleanup_stage EXIT
if [ "$mode" = "ro" ]; then
  if [ -L "$source" ]; then
    printf 'read-only attachment copy refuses symlink: %s\n' "$source" >&2
    exit 2
  fi
  if [ -d "$source" ]; then
    link="$(find "$source" -type l -print -quit)"
    if [ -n "$link" ]; then
      printf 'read-only attachment copy refuses symlink: %s\n' "$link" >&2
      exit 2
    fi
  fi
  make_stage
  cp -a -- "$source" "$temp"
  chmod -R a-w -- "$temp"
  finish_stage
else
  make_stage
  ln -s -- "$source" "$temp" || cp -a -- "$source" "$temp"
  finish_stage
fi
trap - EXIT
"#,
        sandbox_mount_parent_chain_functions_script()
    )
}

pub(super) fn container_import_dir_script() -> String {
    format!(
        r#"set -eu
{}
root="$1"
dest="$2"
ensure_mount_parent_chain "$root" "$dest"
dest_parent="${{dest%/*}}"
dest_name="${{dest##*/}}"
stage=""
temp=""
backup=""
cleanup_stage() {{
  if [ -n "${{stage:-}}" ] && {{ [ -L "$stage" ] || [ -e "$stage" ]; }}; then
    if [ ! -L "$stage" ]; then
      chmod -R u+w -- "$stage" 2>/dev/null || true
    fi
    rm -rf -- "$stage"
  fi
}}
make_stage() {{
  stage="$(mktemp -d "$dest_parent/.${{dest_name}}.import-tmp.XXXXXX")"
  temp="$stage/payload"
}}
finish_stage() {{
  backup=""
  if [ -L "$dest" ] || [ -e "$dest" ]; then
    backup="$(mktemp -d "$dest_parent/.${{dest_name}}.old.XXXXXX")"
    rmdir -- "$backup"
    mv -- "$dest" "$backup"
  fi
  if mv -- "$temp" "$dest"; then
    :
  else
    status="$?"
    if [ -n "$backup" ]; then
      mv -- "$backup" "$dest" || printf 'failed to restore previous container attachment materialization: %s\n' "$dest" >&2
    fi
    return "$status"
  fi
  if [ -n "$backup" ]; then
    if [ ! -L "$backup" ]; then
      chmod -R u+w -- "$backup" 2>/dev/null || true
    fi
    rm -rf -- "$backup"
  fi
  rmdir -- "$stage"
  stage=""
}}
trap cleanup_stage EXIT
make_stage
mkdir "$temp"
tar -C "$temp" -xf -
finish_stage
trap - EXIT
"#,
        sandbox_mount_parent_chain_functions_script()
    )
}

pub(super) fn container_remove_mount_path_script() -> String {
    format!(
        "set -eu\n{}\nremove_mount_path_if_parent_safe \"$1\" \"$2\"\n",
        sandbox_mount_parent_chain_functions_script()
    )
}

pub(super) async fn container_remove_mount_path_in_worktree(
    data_root: &Path,
    container_id: &str,
    worktree_root: &Path,
    target: &Path,
) -> Result<()> {
    let mut cmd = sandbox_container_command(data_root)?;
    cmd.arg("exec")
        .arg("--interactive")
        .arg(container_id)
        .arg("sh")
        .arg("-lc")
        .arg(container_remove_mount_path_script())
        .arg("--")
        .arg(worktree_root)
        .arg(target);
    let out = cmd
        .output()
        .await
        .context("sandbox exec remove attachment mount path")?;
    if out.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "container attachment mount removal failed (status {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
}

async fn import_dir_to_container(
    data_root: &Path,
    container_id: &str,
    src: &Path,
    dest: &Path,
) -> Result<()> {
    // Stream a tar archive into the container so extracted files are writable by the execution
    // user (avoids `container cp` ownership quirks). The guest script stages the extract before
    // replacing the materialized root so failed streams are not mistaken for ready imports.
    let mut tar_cmd = Command::new("tar");
    tar_cmd.arg("-C").arg(src).arg("-cf").arg("-").arg(".");
    tar_cmd.stdout(Stdio::piped());
    let mut tar_child = tar_cmd.spawn().context("spawning tar")?;
    let mut tar_out = tar_child.stdout.take().context("taking tar stdout")?;

    let mut pod_cmd = sandbox_container_command(data_root)?;
    pod_cmd
        .arg("exec")
        .arg("--interactive")
        .arg(container_id)
        .arg("sh")
        .arg("-lc")
        .arg(container_import_dir_script())
        .arg("--")
        .arg(CTX_CONTAINER_WORKSPACE_ROOT)
        .arg(dest);
    pod_cmd.stdin(Stdio::piped());
    let mut pod_child = pod_cmd.spawn().context("spawning sandbox exec tar")?;
    let mut pod_in = pod_child
        .stdin
        .take()
        .context("taking sandbox exec stdin")?;

    let copy_result = tokio::io::copy(&mut tar_out, &mut pod_in)
        .await
        .context("streaming tar to sandbox exec");
    drop(tar_out);
    drop(pod_in);

    let tar_status = tar_child.wait().await.context("waiting on tar")?;
    let out = pod_child
        .wait_with_output()
        .await
        .context("waiting on sandbox exec tar")?;
    if !out.status.success() {
        anyhow::bail!(
            "sandbox exec tar failed (status {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    copy_result?;
    if !tar_status.success() {
        anyhow::bail!("tar failed with status {tar_status}");
    }
    Ok(())
}

pub(super) async fn ensure_attachment_imported_to_container(
    data_root: &Path,
    container_id: &str,
    attachment: &WorkspaceAttachment,
    src_dir: &Path,
    refresh: bool,
) -> Result<PathBuf> {
    let dest = container_attachment_materialized_root(attachment);
    let exists = if refresh {
        false
    } else {
        container_path_exists(data_root, container_id, &dest)
            .await
            .unwrap_or(false)
    };
    if exists {
        return Ok(dest);
    }
    // Reset and re-import.
    import_dir_to_container(data_root, container_id, src_dir, &dest).await?;
    Ok(dest)
}

pub(super) async fn container_ensure_mount(
    data_root: &Path,
    container_id: &str,
    worktree_root: &Path,
    target: &Path,
    source: &Path,
    mode: AttachmentMode,
) -> Result<()> {
    let mode_arg = match mode {
        AttachmentMode::Ro => "ro",
        AttachmentMode::Rw => "rw",
    };
    let mut cmd = sandbox_container_command(data_root)?;
    cmd.arg("exec")
        .arg("--interactive")
        .arg(container_id)
        .arg("sh")
        .arg("-lc")
        .arg(container_mount_script())
        .arg("--")
        .arg(worktree_root)
        .arg(target)
        .arg(source)
        .arg(mode_arg);
    let out = cmd
        .output()
        .await
        .context("sandbox exec attachment mount")?;
    if out.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "container mount failed (status {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
}

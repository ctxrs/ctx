use super::*;
use std::process::Stdio;

use tokio::process::Command;

fn avf_guest_rel_path(worktree_root: &Path, target: &Path) -> Result<String> {
    let relative = target.strip_prefix(worktree_root).with_context(|| {
        format!(
            "path {} must stay under AVF worktree root {}",
            target.display(),
            worktree_root.display()
        )
    })?;
    if relative.as_os_str().is_empty() {
        Ok(".".to_string())
    } else {
        Ok(relative.to_string_lossy().to_string())
    }
}

async fn avf_run_capture(
    data_root: &Path,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    worktree_root: &Path,
    command: &str,
    args: &[String],
) -> Result<std::process::Output> {
    ctx_avf_linux_runtime::run_guest_exec_capture(
        data_root,
        workspace_id,
        worktree_id,
        worktree_root,
        command,
        args,
        &std::collections::HashMap::new(),
        None,
        false,
    )
    .await
    .with_context(|| format!("AVF guest exec `{command}`"))
}

pub(super) async fn avf_run_success(
    data_root: &Path,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    worktree_root: &Path,
    command: &str,
    args: &[String],
) -> Result<()> {
    let out = avf_run_capture(
        data_root,
        workspace_id,
        worktree_id,
        worktree_root,
        command,
        args,
    )
    .await?;
    if out.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "AVF guest exec `{command}` failed (status {}): {}",
            out.status,
            command_failure_detail(&out)
        );
    }
}

fn avf_guest_target_arg(worktree_root: &Path, target: &Path) -> Result<String> {
    let rel = avf_guest_rel_path(worktree_root, target)?;
    if rel == "." {
        Ok(".".to_string())
    } else {
        Ok(format!("./{rel}"))
    }
}

pub(super) fn avf_remove_mount_path_script() -> String {
    format!(
        "set -eu\n{}\nremove_mount_path_if_parent_safe \"$1\" \"$2\"\n",
        sandbox_mount_parent_chain_functions_script()
    )
}

pub(super) fn avf_import_dir_script() -> String {
    format!(
        r#"set -eu
{}
target="$1"
mode="$2"
if [ "$mode" != "ro" ] && [ "$mode" != "rw" ]; then
  printf 'unsupported attachment mount mode: %s\n' "$mode" >&2
  exit 2
fi
ensure_mount_parent_chain "." "$target"
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
      mv -- "$backup" "$target" || printf 'failed to restore previous AVF attachment mount: %s\n' "$target" >&2
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
if [ -L "$temp" ] || [ ! -d "$temp" ]; then
  printf 'attachment mount temp target must be a directory: %s\n' "$temp" >&2
  exit 2
fi
tar -C "$temp" -xf -
if [ "$mode" = "ro" ]; then
  chmod -R a-w -- "$temp"
fi
finish_stage
trap - EXIT
"#,
        sandbox_mount_parent_chain_functions_script()
    )
}

pub(super) fn avf_import_file_script() -> String {
    format!(
        r#"set -eu
{}
target="$1"
mode="$2"
if [ "$mode" != "ro" ] && [ "$mode" != "rw" ]; then
  printf 'unsupported attachment mount mode: %s\n' "$mode" >&2
  exit 2
fi
ensure_mount_parent_chain "." "$target"
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
      mv -- "$backup" "$target" || printf 'failed to restore previous AVF attachment mount: %s\n' "$target" >&2
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
cat > "$temp"
if [ "$mode" = "ro" ]; then
  chmod -R a-w -- "$temp"
fi
finish_stage
trap - EXIT
"#,
        sandbox_mount_parent_chain_functions_script()
    )
}

pub(super) async fn avf_remove_mount_path_in_worktree(
    data_root: &Path,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    worktree_root: &Path,
    target: &Path,
) -> Result<()> {
    let guest_target = avf_guest_target_arg(worktree_root, target)?;
    avf_run_success(
        data_root,
        workspace_id,
        worktree_id,
        worktree_root,
        "sh",
        &[
            "-lc".to_string(),
            avf_remove_mount_path_script(),
            "--".to_string(),
            ".".to_string(),
            guest_target,
        ],
    )
    .await
}

async fn import_dir_to_avf_worktree(
    data_root: &Path,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    worktree_root: &Path,
    src: &Path,
    dest: &Path,
    mode: AttachmentMode,
) -> Result<()> {
    let dest_rel = avf_guest_target_arg(worktree_root, dest)?;
    let mode_arg = match mode {
        AttachmentMode::Ro => "ro",
        AttachmentMode::Rw => "rw",
    };
    let mut tar_cmd = Command::new("tar");
    tar_cmd.arg("-C").arg(src).arg("-cf").arg("-").arg(".");
    tar_cmd.stdout(Stdio::piped());
    let mut tar_child = tar_cmd
        .spawn()
        .context("spawning tar for AVF attachment import")?;
    let mut tar_out = tar_child.stdout.take().context("taking tar stdout")?;

    let mut guest_cmd = ctx_avf_linux_runtime::build_guest_exec_command(
        data_root,
        workspace_id,
        worktree_id,
        worktree_root,
        "sh",
        &[
            "-lc".to_string(),
            avf_import_dir_script(),
            "--".to_string(),
            dest_rel,
            mode_arg.to_string(),
        ],
        &std::collections::HashMap::new(),
        None,
        false,
    )?;
    guest_cmd.stdin(Stdio::piped());
    let mut guest_child = guest_cmd
        .spawn()
        .context("spawning AVF guest tar extract")?;
    let mut guest_in = guest_child
        .stdin
        .take()
        .context("taking AVF guest tar stdin")?;

    tokio::io::copy(&mut tar_out, &mut guest_in)
        .await
        .context("streaming tar to AVF guest exec")?;
    drop(guest_in);

    let tar_status = tar_child.wait().await.context("waiting on host tar")?;
    if !tar_status.success() {
        anyhow::bail!("tar failed with status {tar_status}");
    }
    let out = guest_child
        .wait_with_output()
        .await
        .context("waiting on AVF guest tar extract")?;
    if !out.status.success() {
        anyhow::bail!(
            "AVF guest tar extract failed (status {}): {}",
            out.status,
            command_failure_detail(&out)
        );
    }
    Ok(())
}

async fn import_file_to_avf_worktree(
    data_root: &Path,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    worktree_root: &Path,
    src: &Path,
    dest: &Path,
    mode: AttachmentMode,
) -> Result<()> {
    let dest_rel = avf_guest_target_arg(worktree_root, dest)?;
    let mode_arg = match mode {
        AttachmentMode::Ro => "ro",
        AttachmentMode::Rw => "rw",
    };
    let mut guest_cmd = ctx_avf_linux_runtime::build_guest_exec_command(
        data_root,
        workspace_id,
        worktree_id,
        worktree_root,
        "sh",
        &[
            "-lc".to_string(),
            avf_import_file_script(),
            "--".to_string(),
            dest_rel,
            mode_arg.to_string(),
        ],
        &std::collections::HashMap::new(),
        None,
        false,
    )?;
    guest_cmd.stdin(Stdio::piped());
    let mut guest_child = guest_cmd.spawn().context("spawning AVF guest file copy")?;
    let mut guest_in = guest_child
        .stdin
        .take()
        .context("taking AVF guest file stdin")?;
    let mut source = tokio::fs::File::open(src)
        .await
        .with_context(|| format!("opening source file {}", src.display()))?;
    tokio::io::copy(&mut source, &mut guest_in)
        .await
        .with_context(|| format!("streaming file {} into AVF guest", src.display()))?;
    drop(guest_in);
    let out = guest_child
        .wait_with_output()
        .await
        .context("waiting on AVF guest file copy")?;
    if !out.status.success() {
        anyhow::bail!(
            "AVF guest file copy failed (status {}): {}",
            out.status,
            command_failure_detail(&out)
        );
    }
    Ok(())
}

pub(super) async fn avf_copy_source_to_mount(
    data_root: &Path,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    worktree_root: &Path,
    source: &Path,
    target: &Path,
    mode: AttachmentMode,
) -> Result<()> {
    if mode == AttachmentMode::Ro {
        validate_attachment_tree_within_root(source, source, AttachmentSourceSymlinkPolicy::Reject)
            .await?;
    }
    let metadata = tokio::fs::metadata(source)
        .await
        .with_context(|| format!("stat attachment source {}", source.display()))?;
    if metadata.is_dir() {
        import_dir_to_avf_worktree(
            data_root,
            workspace_id,
            worktree_id,
            worktree_root,
            source,
            target,
            mode,
        )
        .await?;
    } else {
        import_file_to_avf_worktree(
            data_root,
            workspace_id,
            worktree_id,
            worktree_root,
            source,
            target,
            mode,
        )
        .await?;
    }
    Ok(())
}

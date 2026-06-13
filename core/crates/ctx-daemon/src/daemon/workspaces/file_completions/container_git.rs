use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ctx_core::models::Worktree;
use ctx_settings_model::ContainerRuntimeKind;

use super::super::FileCompletionsError;

pub(super) async fn container_git_ls_files(
    data_root: &Path,
    worktree: &Worktree,
    runtime: ContainerRuntimeKind,
    workdir: &str,
    git_args: &[&str],
) -> Result<Vec<String>, FileCompletionsError> {
    const SANDBOX_GIT_LS_FILES_TIMEOUT: Duration = Duration::from_secs(30);
    let out = match runtime {
        ContainerRuntimeKind::NativeContainer => {
            let mut cmd =
                ctx_harness_runtime::sandbox_container_command(data_root).map_err(|err| {
                    FileCompletionsError::internal(format!("building sandbox command: {err}"))
                })?;
            cmd.arg("exec")
                .arg("--workdir")
                .arg(workdir)
                .arg(ctx_workspace_container::workspace_container_name(
                    worktree.workspace_id,
                ))
                .arg("git");
            for arg in git_args {
                cmd.arg(arg);
            }
            ctx_sandbox_container_runtime::command_output_with_timeout(
                cmd,
                SANDBOX_GIT_LS_FILES_TIMEOUT,
            )
            .await
            .map_err(|err| FileCompletionsError::internal(format!("running sandbox git: {err}")))?
        }
        ContainerRuntimeKind::SharedVmContainer => {
            let args = git_args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>();
            let guest_cwd = PathBuf::from(workdir);
            tokio::time::timeout(
                SANDBOX_GIT_LS_FILES_TIMEOUT,
                ctx_avf_linux_runtime::run_guest_exec_capture(
                    data_root,
                    worktree.workspace_id,
                    worktree.id,
                    &guest_cwd,
                    "git",
                    &args,
                    &HashMap::new(),
                    None,
                    false,
                ),
            )
            .await
            .map_err(|_| FileCompletionsError::internal("shared VM git timed out"))?
            .map_err(|err| {
                FileCompletionsError::internal(format!("running shared VM git: {err}"))
            })?
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(FileCompletionsError::internal(format!(
            "container git {git_args:?} failed: {stderr}"
        )));
    }
    Ok(parse_nul_delimited_git_paths(&out.stdout))
}

fn parse_nul_delimited_git_paths(stdout: &[u8]) -> Vec<String> {
    let mut files = Vec::new();
    for part in stdout.split(|b| *b == 0u8) {
        if part.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(part).to_string();
        if !s.trim().is_empty() {
            files.push(s);
        }
    }
    files
}

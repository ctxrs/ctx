use super::*;

pub(super) async fn command_for_shell<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    command: &str,
    workdir: &Path,
    envs: &[(String, String)],
) -> Command {
    if cfg!(windows) {
        let mut cmd = merge_queue_command(state, entry, command, "cmd", Some(workdir), envs).await;
        cmd.args(["/C", command]);
        cmd
    } else {
        let mut cmd = merge_queue_command(state, entry, command, "bash", Some(workdir), envs).await;
        cmd.args(["-lc", command]);
        cmd
    }
}

#[derive(Debug)]
pub(super) enum QueueError {
    Conflict {
        message: String,
    },
    Failed {
        message: String,
        exit_code: Option<i64>,
        result_commit_sha: Option<String>,
    },
}

impl QueueError {
    pub(super) fn fail(
        message: String,
        exit_code: Option<i64>,
        result_commit_sha: Option<String>,
    ) -> Self {
        QueueError::Failed {
            message,
            exit_code,
            result_commit_sha,
        }
    }
}

fn emit_merge_queue_tool_event<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    command: &str,
    workdir: Option<&Path>,
    used_tool_slice: bool,
) {
    H::emit_tool_exec(
        state,
        MergeQueueToolExecEvent {
            entry_id: entry.id,
            session_id: entry.session_id,
            worktree_id: entry.worktree_id,
            command: command.to_string(),
            workdir: workdir.map(|dir| dir.to_string_lossy().to_string()),
            used_tool_slice,
            tool_slice_unit: TOOL_SLICE_UNIT,
        },
    );
}

pub(super) async fn merge_queue_command<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    command_label: &str,
    program: &str,
    workdir: Option<&Path>,
    envs: &[(String, String)],
) -> Command {
    let merged: HashMap<String, String> = envs.iter().cloned().collect();
    let merged: Vec<(String, String)> = merged.into_iter().collect();
    let (cmd, used_tool_slice) = tool_slice_command(program, workdir, &merged).await;
    emit_merge_queue_tool_event(state, entry, command_label, workdir, used_tool_slice);
    cmd
}

#[cfg(target_os = "linux")]
async fn tool_slice_command(
    program: &str,
    workdir: Option<&Path>,
    envs: &[(String, String)],
) -> (Command, bool) {
    if systemd_run_available().await {
        let mut cmd = Command::new("systemd-run");
        cmd.arg("--user")
            .arg("--slice")
            .arg(TOOL_SLICE_UNIT)
            .arg("--quiet")
            .arg("--pipe")
            .arg("--wait");
        if let Some(dir) = workdir {
            cmd.arg("--working-directory").arg(dir);
        }
        for (key, value) in envs {
            cmd.arg("--setenv").arg(format!("{key}={value}"));
        }
        cmd.arg("--").arg(program);
        return (cmd, true);
    }

    let mut cmd = Command::new(program);
    if let Some(dir) = workdir {
        cmd.current_dir(dir);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    (cmd, false)
}

#[cfg(not(target_os = "linux"))]
async fn tool_slice_command(
    program: &str,
    workdir: Option<&Path>,
    envs: &[(String, String)],
) -> (Command, bool) {
    let mut cmd = Command::new(program);
    if let Some(dir) = workdir {
        cmd.current_dir(dir);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    (cmd, false)
}

#[cfg(target_os = "linux")]
async fn systemd_run_available() -> bool {
    static SYSTEMD_RUN_AVAILABLE: OnceLock<bool> = OnceLock::new();
    if let Some(value) = SYSTEMD_RUN_AVAILABLE.get() {
        return *value;
    }

    let available = {
        let output = match Command::new("systemd-run").arg("--version").output().await {
            Ok(output) => output,
            Err(_) => return false,
        };
        if !output.status.success() {
            false
        } else {
            let output = Command::new("systemctl")
                .arg("--user")
                .arg("show-environment")
                .output()
                .await;
            output.map(|o| o.status.success()).unwrap_or(false)
        }
    };

    let _ = SYSTEMD_RUN_AVAILABLE.set(available);
    if !available {
        tracing::warn!(
            "systemd-run unavailable; merge queue commands will run without tool slice isolation"
        );
    }
    available
}

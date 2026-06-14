use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use ctx_core::ids::{TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{Workspace, Worktree, WorktreeBootstrapStatus};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const DEFAULT_TIMEOUT_SEC: u64 = 60;
const MAX_LOG_BYTES: usize = 200 * 1024;

#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub timeout: Duration,
    pub command: String,
    pub wait_for_completion: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BootstrapConfigInput {
    pub setup_command: Option<String>,
    pub timeout_sec: Option<u64>,
    pub wait_for_completion: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct CleanupConfig {
    pub timeout: Duration,
    pub command: String,
}

#[derive(Debug, Clone, Default)]
pub struct CleanupConfigInput {
    pub cleanup_command: Option<String>,
    pub cleanup_timeout_sec: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct BootstrapStep {
    pub label: String,
    pub command: String,
}

#[derive(Debug)]
pub struct BootstrapCommandResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapCommandRuntime {
    Host,
    Container,
}

impl BootstrapCommandRuntime {
    fn spawn_context(self) -> &'static str {
        match self {
            BootstrapCommandRuntime::Host => "spawning bootstrap command",
            BootstrapCommandRuntime::Container => "spawning bootstrap command (container)",
        }
    }

    fn wait_context(self) -> &'static str {
        match self {
            BootstrapCommandRuntime::Host => "waiting on bootstrap command",
            BootstrapCommandRuntime::Container => "waiting on bootstrap command (container)",
        }
    }

    fn killed_wait_context(self) -> &'static str {
        match self {
            BootstrapCommandRuntime::Host => "waiting on killed bootstrap command",
            BootstrapCommandRuntime::Container => "waiting on killed bootstrap command (container)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BootstrapReport {
    pub status: WorktreeBootstrapStatus,
    pub started_at: chrono::DateTime<Utc>,
    pub finished_at: chrono::DateTime<Utc>,
    pub exit_code: Option<i64>,
    pub timeout_sec: i64,
    pub error: Option<String>,
    pub command: Option<String>,
    pub raw_log: String,
}

#[async_trait]
pub trait WorktreeBootstrapHost: Send + Sync + 'static {
    async fn load_bootstrap_config(&self, workspace: &Workspace)
        -> Result<Option<BootstrapConfig>>;

    async fn execute_bootstrap_step(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        step: &BootstrapStep,
        timeout: Duration,
    ) -> Result<BootstrapCommandResult>;

    async fn persist_bootstrap_report(
        &self,
        workspace_id: WorkspaceId,
        worktree: &Worktree,
        report: BootstrapReport,
    );

    async fn register_bootstrap(&self, worktree_id: WorktreeId, wait_for_completion: bool);

    async fn finish_bootstrap(&self, worktree_id: WorktreeId);
}

pub async fn run_worktree_bootstrap<H>(
    host: &H,
    workspace: &Workspace,
    worktree: &Worktree,
) -> Result<()>
where
    H: WorktreeBootstrapHost,
{
    let Some(plan) = prepare_worktree_bootstrap(host, workspace, worktree).await? else {
        return Ok(());
    };
    run_worktree_bootstrap_plan(host, workspace, worktree, plan).await
}

pub async fn spawn_worktree_bootstrap<H>(
    host: std::sync::Arc<H>,
    workspace: Workspace,
    worktree: Worktree,
) -> Result<()>
where
    H: WorktreeBootstrapHost,
{
    let Some(plan) = prepare_worktree_bootstrap(host.as_ref(), &workspace, &worktree).await? else {
        return Ok(());
    };

    let worktree_id = worktree.id;
    host.register_bootstrap(worktree_id, plan.wait_for_completion)
        .await;

    tokio::spawn(async move {
        if let Err(e) =
            run_worktree_bootstrap_plan(host.as_ref(), &workspace, &worktree, plan).await
        {
            tracing::warn!(worktree_id = %worktree_id.0, "worktree bootstrap failed: {e:?}");
        }
        host.finish_bootstrap(worktree_id).await;
    });

    Ok(())
}

#[derive(Debug, Clone)]
struct WorktreeBootstrapPlan {
    timeout: Duration,
    steps: Vec<BootstrapStep>,
    wait_for_completion: bool,
}

async fn prepare_worktree_bootstrap<H>(
    host: &H,
    workspace: &Workspace,
    worktree: &Worktree,
) -> Result<Option<WorktreeBootstrapPlan>>
where
    H: WorktreeBootstrapHost,
{
    let has_vcs_ref = worktree.vcs_ref.is_some() || worktree.git_branch.is_some();
    if !has_vcs_ref {
        return Ok(None);
    }

    let bootstrap = match host.load_bootstrap_config(workspace).await {
        Ok(Some(cfg)) => cfg,
        Ok(None) => return Ok(None),
        Err(err) => {
            let started_at = Utc::now();
            let finished_at = Utc::now();
            let error = err.to_string();
            let error_details = format!("{err:#}");
            let mut log = String::new();
            log.push_str("# ctx worktree bootstrap\n");
            log.push_str(&format!("# Worktree: {}\n", worktree.root_path.trim()));
            log.push_str(&format!("# Started: {}\n\n", started_at.to_rfc3339()));
            log.push_str("[error]\n");
            log.push_str(&error_details);
            log.push('\n');
            log.push_str(&format!("\n# Finished: {}\n", finished_at.to_rfc3339()));
            log.push_str("# Status: Failed\n");
            host.persist_bootstrap_report(
                workspace.id,
                worktree,
                BootstrapReport {
                    status: WorktreeBootstrapStatus::Failed,
                    started_at,
                    finished_at,
                    exit_code: None,
                    timeout_sec: DEFAULT_TIMEOUT_SEC as i64,
                    error: Some(error),
                    command: None,
                    raw_log: log,
                },
            )
            .await;
            return Ok(None);
        }
    };

    let steps = build_bootstrap_steps(&bootstrap.command)?;
    if steps.is_empty() {
        return Ok(None);
    }

    Ok(Some(WorktreeBootstrapPlan {
        timeout: bootstrap.timeout,
        steps,
        wait_for_completion: bootstrap.wait_for_completion,
    }))
}

async fn run_worktree_bootstrap_plan<H>(
    host: &H,
    workspace: &Workspace,
    worktree: &Worktree,
    plan: WorktreeBootstrapPlan,
) -> Result<()>
where
    H: WorktreeBootstrapHost,
{
    let WorktreeBootstrapPlan { timeout, steps, .. } = plan;
    let started_at = Utc::now();
    let mut log = String::new();
    log.push_str("# ctx worktree bootstrap\n");
    log.push_str(&format!("# Worktree: {}\n", worktree.root_path.trim()));
    log.push_str(&format!("# Started: {}\n\n", started_at.to_rfc3339()));

    let mut last_exit = None;
    let mut last_step: Option<BootstrapStep> = None;
    let mut failure_status = None;
    let mut failure_error = None;

    for step in &steps {
        last_step = Some(step.clone());
        log.push_str(&format!("$ {}\n", step.label));

        let result = match host
            .execute_bootstrap_step(workspace, worktree, step, timeout)
            .await
        {
            Ok(result) => result,
            Err(err) => {
                failure_status = Some(WorktreeBootstrapStatus::Failed);
                failure_error = Some(err.to_string());
                break;
            }
        };
        last_exit = result.exit_code.map(|v| v as i64);

        append_output(&mut log, &result.stdout, "stdout");
        append_output(&mut log, &result.stderr, "stderr");

        if result.timed_out {
            failure_status = Some(WorktreeBootstrapStatus::Timeout);
            failure_error = Some(format!("bootstrap timed out after {}s", timeout.as_secs()));
            break;
        }

        if let Some(code) = result.exit_code {
            if code != 0 {
                failure_status = Some(WorktreeBootstrapStatus::Failed);
                failure_error = Some(format!("command exited with code {code}"));
                break;
            }
        }
    }

    let finished_at = Utc::now();
    log.push_str(&format!("\n# Finished: {}\n", finished_at.to_rfc3339()));
    if let Some(status) = &failure_status {
        log.push_str(&format!("# Status: {status:?}\n"));
    } else {
        log.push_str("# Status: success\n");
    }

    let (status, error) = match failure_status {
        Some(status) => (status, failure_error),
        None => (WorktreeBootstrapStatus::Success, None),
    };

    host.persist_bootstrap_report(
        workspace.id,
        worktree,
        BootstrapReport {
            status,
            started_at,
            finished_at,
            exit_code: last_exit,
            timeout_sec: timeout.as_secs() as i64,
            error,
            command: last_step.as_ref().map(|step| step.command.clone()),
            raw_log: log,
        },
    )
    .await;

    Ok(())
}

fn build_bootstrap_steps(command: &str) -> Result<Vec<BootstrapStep>> {
    let mut steps = Vec::new();
    let trimmed = command.trim();
    if !trimmed.is_empty() {
        steps.push(BootstrapStep {
            label: trimmed.to_string(),
            command: trimmed.to_string(),
        });
    }
    Ok(steps)
}

pub fn shell_bootstrap_command(command: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.arg("-lc").arg(command);
        cmd
    }
}

pub fn normalize_bootstrap_config(input: BootstrapConfigInput) -> Option<BootstrapConfig> {
    let command = input
        .setup_command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)?;
    let timeout_sec = input.timeout_sec.unwrap_or(DEFAULT_TIMEOUT_SEC);
    let timeout_sec = if timeout_sec == 0 {
        DEFAULT_TIMEOUT_SEC
    } else {
        timeout_sec
    };

    Some(BootstrapConfig {
        timeout: Duration::from_secs(timeout_sec),
        command,
        wait_for_completion: input.wait_for_completion.unwrap_or(false),
    })
}

pub fn normalize_cleanup_config(input: CleanupConfigInput) -> Option<CleanupConfig> {
    let command = input
        .cleanup_command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)?;
    let timeout_sec = input.cleanup_timeout_sec.unwrap_or(DEFAULT_TIMEOUT_SEC);
    let timeout_sec = if timeout_sec == 0 {
        DEFAULT_TIMEOUT_SEC
    } else {
        timeout_sec
    };

    Some(CleanupConfig {
        timeout: Duration::from_secs(timeout_sec),
        command,
    })
}

pub async fn run_bootstrap_command(
    mut cmd: Command,
    timeout: Duration,
    runtime: BootstrapCommandRuntime,
) -> Result<BootstrapCommandResult> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context(runtime.spawn_context())?;

    let mut stdout = child.stdout.take().context("reading stdout")?;
    let mut stderr = child.stderr.take().context("reading stderr")?;

    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).await?;
        Ok::<Vec<u8>, std::io::Error>(buf)
    });

    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stderr.read_to_end(&mut buf).await?;
        Ok::<Vec<u8>, std::io::Error>(buf)
    });

    let mut timed_out = false;
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status.context(runtime.wait_context())?,
        Err(_) => {
            timed_out = true;
            let _ = child.kill().await;
            child.wait().await.context(runtime.killed_wait_context())?
        }
    };

    let stdout = stdout_task.await.unwrap_or_else(|_| Ok(Vec::new()))?;
    let stderr = stderr_task.await.unwrap_or_else(|_| Ok(Vec::new()))?;

    Ok(BootstrapCommandResult {
        exit_code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        timed_out,
    })
}

pub fn bootstrap_command_env(
    worktree: &Worktree,
    live_workspace_root: &Path,
    live_worktree_root: &Path,
) -> HashMap<String, String> {
    HashMap::from([
        (
            "CTX_WORKSPACE_ROOT".to_string(),
            live_workspace_root.to_string_lossy().to_string(),
        ),
        (
            "CTX_WORKTREE_ROOT".to_string(),
            live_worktree_root.to_string_lossy().to_string(),
        ),
        ("CTX_WORKTREE_ID".to_string(), worktree.id.0.to_string()),
        (
            "CTX_BRANCH_NAME".to_string(),
            worktree
                .vcs_ref
                .clone()
                .or_else(|| worktree.git_branch.clone())
                .unwrap_or_default(),
        ),
        (
            "CTX_BASE_REVISION".to_string(),
            worktree
                .base_revision
                .as_deref()
                .unwrap_or(&worktree.base_commit_sha)
                .to_string(),
        ),
        (
            "CTX_BASE_COMMIT_SHA".to_string(),
            worktree
                .base_revision
                .as_deref()
                .unwrap_or(&worktree.base_commit_sha)
                .to_string(),
        ),
    ])
}

pub fn cleanup_command_env(
    worktree: &Worktree,
    task_id: TaskId,
    live_workspace_root: &Path,
    live_worktree_root: &Path,
) -> HashMap<String, String> {
    let mut env = bootstrap_command_env(worktree, live_workspace_root, live_worktree_root);
    env.insert("CTX_TASK_ID".to_string(), task_id.0.to_string());
    env
}

fn append_output(log: &mut String, output: &str, label: &str) {
    let trimmed = output.trim_end_matches('\n');
    if trimmed.is_empty() {
        return;
    }
    log.push_str(&format!("[{label}]\n"));
    log.push_str(trimmed);
    log.push('\n');
}

pub fn truncate_log(input: &str) -> (String, bool) {
    if input.len() <= MAX_LOG_BYTES {
        return (input.to_string(), false);
    }
    let mut out = input.chars().take(MAX_LOG_BYTES).collect::<String>();
    out.push_str("\n...(truncated)\n");
    (out, true)
}

pub fn prepare_bootstrap_log_for_storage(raw_log: &str) -> (String, bool) {
    truncate_log(&ctx_core::redaction::redact_sensitive(raw_log))
}

pub fn bootstrap_logs_dir(data_root: &Path) -> PathBuf {
    data_root.join("logs").join("worktree-bootstrap")
}

pub fn bootstrap_log_path(data_root: &Path, worktree_id: WorktreeId) -> PathBuf {
    bootstrap_logs_dir(data_root).join(format!("worktree-bootstrap-{}.log", worktree_id.0))
}

pub async fn write_bootstrap_log(
    data_root: &Path,
    worktree_id: WorktreeId,
    contents: &str,
) -> Result<PathBuf> {
    let dir = bootstrap_logs_dir(data_root);
    tokio::fs::create_dir_all(&dir)
        .await
        .context("creating bootstrap log dir")?;
    let path = bootstrap_log_path(data_root, worktree_id);
    tokio::fs::write(&path, contents)
        .await
        .with_context(|| format!("writing bootstrap log to {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ctx_core::ids::{WorkspaceId, WorktreeId};
    use ctx_core::models::{VcsKind, Worktree};
    use std::path::Path;
    use std::time::Duration;

    use super::{
        bootstrap_command_env, bootstrap_log_path, build_bootstrap_steps, cleanup_command_env,
        normalize_bootstrap_config, normalize_cleanup_config, prepare_bootstrap_log_for_storage,
        run_bootstrap_command, shell_bootstrap_command, truncate_log, BootstrapCommandRuntime,
        BootstrapConfigInput, CleanupConfigInput,
    };

    #[test]
    fn build_bootstrap_steps_ignores_blank_commands() {
        assert!(build_bootstrap_steps("   ").expect("steps").is_empty());
        assert_eq!(
            build_bootstrap_steps("echo hi").expect("steps")[0].command,
            "echo hi"
        );
    }

    #[test]
    fn normalize_bootstrap_config_ignores_blank_commands() {
        assert!(normalize_bootstrap_config(BootstrapConfigInput {
            setup_command: Some("   ".to_string()),
            timeout_sec: Some(5),
            wait_for_completion: Some(true),
        })
        .is_none());
    }

    #[test]
    fn normalize_bootstrap_config_defaults_zero_timeout_and_wait_flag() {
        let config = normalize_bootstrap_config(BootstrapConfigInput {
            setup_command: Some("  pnpm install  ".to_string()),
            timeout_sec: Some(0),
            wait_for_completion: None,
        })
        .expect("config");

        assert_eq!(config.command, "pnpm install");
        assert_eq!(config.timeout.as_secs(), super::DEFAULT_TIMEOUT_SEC);
        assert!(!config.wait_for_completion);
    }

    #[test]
    fn normalize_bootstrap_config_preserves_explicit_timeout_and_wait_flag() {
        let config = normalize_bootstrap_config(BootstrapConfigInput {
            setup_command: Some("make bootstrap".to_string()),
            timeout_sec: Some(120),
            wait_for_completion: Some(true),
        })
        .expect("config");

        assert_eq!(config.timeout.as_secs(), 120);
        assert!(config.wait_for_completion);
    }

    #[test]
    fn normalize_cleanup_config_ignores_blank_commands() {
        assert!(normalize_cleanup_config(CleanupConfigInput {
            cleanup_command: Some("   ".to_string()),
            cleanup_timeout_sec: Some(5),
        })
        .is_none());
    }

    #[test]
    fn normalize_cleanup_config_defaults_zero_timeout() {
        let config = normalize_cleanup_config(CleanupConfigInput {
            cleanup_command: Some("  ./cleanup.sh  ".to_string()),
            cleanup_timeout_sec: Some(0),
        })
        .expect("config");

        assert_eq!(config.command, "./cleanup.sh");
        assert_eq!(config.timeout.as_secs(), super::DEFAULT_TIMEOUT_SEC);
    }

    #[test]
    fn truncate_log_marks_truncated_output() {
        let input = "x".repeat(250_000);
        let (out, truncated) = truncate_log(&input);
        assert!(truncated);
        assert!(out.contains("...(truncated)"));
    }

    #[test]
    fn prepare_bootstrap_log_for_storage_redacts_before_persisting() {
        let (log, truncated) =
            prepare_bootstrap_log_for_storage("Authorization: Bearer secret-token\nok");

        assert!(!truncated);
        assert!(!log.contains("secret-token"));
        assert!(log.contains("ok"));
    }

    #[test]
    fn bootstrap_log_path_uses_worktree_specific_log_file() {
        let worktree_id = WorktreeId::new();
        let path = bootstrap_log_path(Path::new("/data"), worktree_id);
        assert_eq!(
            path,
            Path::new("/data")
                .join("logs")
                .join("worktree-bootstrap")
                .join(format!("worktree-bootstrap-{}.log", worktree_id.0))
        );
    }

    #[tokio::test]
    async fn run_bootstrap_command_captures_stdout_and_stderr() {
        let cmd = shell_bootstrap_command("printf out; printf err >&2");
        let result =
            run_bootstrap_command(cmd, Duration::from_secs(5), BootstrapCommandRuntime::Host)
                .await
                .expect("run command");

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout, "out");
        assert!(result.stderr.contains("err"));
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn run_bootstrap_command_marks_timeout() {
        let cmd = shell_bootstrap_command("sleep 5");
        let result = run_bootstrap_command(
            cmd,
            Duration::from_millis(10),
            BootstrapCommandRuntime::Host,
        )
        .await
        .expect("run command");

        assert!(result.timed_out);
    }

    #[test]
    fn bootstrap_command_env_uses_live_roots_and_worktree_identity() {
        let worktree = Worktree {
            id: WorktreeId::new(),
            workspace_id: WorkspaceId::new(),
            root_path: "/host/worktree".to_string(),
            base_commit_sha: "base-sha".to_string(),
            git_branch: Some("ctx/test".to_string()),
            vcs_kind: Some(VcsKind::Git),
            base_revision: Some("base-rev".to_string()),
            vcs_ref: Some("vcs-ref".to_string()),
            created_at: Utc::now(),
            bootstrap_status: None,
            bootstrap_started_at: None,
            bootstrap_finished_at: None,
            bootstrap_exit_code: None,
            bootstrap_timeout_sec: None,
            bootstrap_error: None,
            bootstrap_log_path: None,
            bootstrap_log_truncated: None,
            bootstrap_command: None,
            bootstrap_script_path: None,
        };

        let env = bootstrap_command_env(
            &worktree,
            Path::new("/live/workspace"),
            Path::new("/live/worktree"),
        );

        assert_eq!(
            env.get("CTX_WORKSPACE_ROOT").map(String::as_str),
            Some("/live/workspace")
        );
        assert_eq!(
            env.get("CTX_WORKTREE_ROOT").map(String::as_str),
            Some("/live/worktree")
        );
        let worktree_id = worktree.id.0.to_string();
        assert_eq!(
            env.get("CTX_WORKTREE_ID").map(String::as_str),
            Some(worktree_id.as_str())
        );
        assert_eq!(
            env.get("CTX_BRANCH_NAME").map(String::as_str),
            Some("vcs-ref")
        );
        assert_eq!(
            env.get("CTX_BASE_REVISION").map(String::as_str),
            Some("base-rev")
        );
        assert_eq!(
            env.get("CTX_BASE_COMMIT_SHA").map(String::as_str),
            Some("base-rev")
        );

        let cleanup_env = cleanup_command_env(
            &worktree,
            ctx_core::ids::TaskId::new(),
            Path::new("/live/workspace"),
            Path::new("/live/worktree"),
        );

        assert_eq!(
            cleanup_env.get("CTX_WORKTREE_ROOT").map(String::as_str),
            Some("/live/worktree")
        );
        assert!(cleanup_env.get("CTX_TASK_ID").is_some());
    }
}

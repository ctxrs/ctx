use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

use crate::container_exec::{build_container_exec_command, container_exec_spec};

use super::config::{
    build_crp_model_probe_config, probe_timeout_for_env, synthetic_models_probe_for_provider,
};
use super::protocol::{
    CrpCommand, CrpCommandEnvelope, CrpEvent, CrpEventEnvelope, CrpModelsProbe, KnownCrpEvent,
};
use super::runtime::{apply_outer_process_env, rewrite_container_command_for_linux};

pub(super) struct CrpModelsProbeRequest {
    pub provider_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub workdir: PathBuf,
    pub env: HashMap<String, String>,
    pub host_timeout: Duration,
    pub container_timeout: Duration,
    pub crp_version: u32,
}

pub async fn probe_crp_models(request: CrpModelsProbeRequest) -> Result<CrpModelsProbe> {
    let CrpModelsProbeRequest {
        provider_id,
        command,
        args,
        workdir,
        env,
        host_timeout,
        container_timeout,
        crp_version,
    } = request;
    if provider_id == "cline" {
        return synthetic_models_probe_for_provider(&provider_id, &env).ok_or_else(|| {
            anyhow!(
                "Cline model discovery requires OPENAI_MODEL; configure a model override for the endpoint"
            )
        });
    }
    if let Some(probe) = synthetic_models_probe_for_provider(&provider_id, &env) {
        return Ok(probe);
    }
    let command_label = command.clone();
    let container_spec = container_exec_spec(&env);
    let probe_timeout = probe_timeout_for_env(&env, host_timeout, container_timeout);
    let mut cmd = if let Some(spec) = container_spec {
        let (container_command, container_args) =
            rewrite_container_command_for_linux(&command, &args, &env)?;
        build_container_exec_command(&spec, &workdir, &env, &container_command, &container_args)?
    } else {
        let mut cmd = Command::new(&command);
        cmd.args(&args);
        cmd.current_dir(&workdir);
        cmd
    };
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    apply_outer_process_env(&mut cmd, &env);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning CRP runtime {provider_id} ({command_label})"))?;
    let stdin = child.stdin.take().context("capturing CRP stdin")?;
    let stdout = child.stdout.take().context("capturing CRP stdout")?;
    let stderr = child.stderr.take().context("capturing CRP stderr")?;

    let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut stderr_tail_task = Some(spawn_probe_output_tail_reader(
        stderr,
        provider_id.clone(),
        "stderr",
        Arc::clone(&stderr_tail),
    ));

    let mut stdin = BufWriter::new(stdin);
    let mut stdout_reader = BufReader::new(stdout).lines();
    let config = build_crp_model_probe_config(&env, &workdir)?;
    let envelope = CrpCommandEnvelope {
        v: Some(crp_version),
        command: CrpCommand::ModelsList {
            config: Some(config),
        },
    };
    let line = serde_json::to_string(&envelope)?;
    if let Err(err) = async {
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await
    }
    .await
    {
        wait_for_probe_output_tail_reader(&mut stderr_tail_task).await;
        let stderr_tail = format_probe_output_tail("stderr", &stderr_tail).await;
        anyhow::bail!(
            "crp runtime closed before models.list response while writing request: {err}{stderr_tail}"
        );
    }

    let result = match timeout(probe_timeout, async {
        loop {
            let Some(line) = stdout_reader.next_line().await? else {
                wait_for_probe_output_tail_reader(&mut stderr_tail_task).await;
                let stderr_tail = format_probe_output_tail("stderr", &stderr_tail).await;
                anyhow::bail!("crp runtime closed before models.list response{stderr_tail}");
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let env = match serde_json::from_str::<CrpEventEnvelope>(trimmed) {
                Ok(env) => env,
                Err(_) => continue,
            };
            if let CrpEvent::Known(event) = env.event {
                if let KnownCrpEvent::ModelsList {
                    models,
                    current_model_id,
                    catalog_source,
                } = *event
                {
                    return Ok(CrpModelsProbe {
                        models,
                        current_model_id,
                        catalog_source,
                    });
                }
            }
        }
    })
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            let stderr_tail = {
                let lines = stderr_tail.lock().await;
                if lines.is_empty() {
                    String::new()
                } else {
                    format!("; stderr_tail={}", lines.join(" | "))
                }
            };
            anyhow::bail!(
                "CRP models.list probe timed out after {}s{}",
                probe_timeout.as_secs(),
                stderr_tail
            );
        }
    };

    let _ = child.kill().await;
    let _ = child.wait().await;

    Ok(result)
}

pub async fn probe_crp_runtime_launch(
    provider_id: &str,
    command: String,
    args: Vec<String>,
    workdir: PathBuf,
    env: HashMap<String, String>,
    host_timeout: Duration,
    container_timeout: Duration,
) -> Result<()> {
    let command_label = command.clone();
    let container_spec = container_exec_spec(&env);
    let probe_timeout = probe_timeout_for_env(&env, host_timeout, container_timeout);
    let mut cmd = if let Some(spec) = container_spec {
        let (container_command, container_args) =
            rewrite_container_command_for_linux(&command, &args, &env)?;
        build_container_exec_command(&spec, &workdir, &env, &container_command, &container_args)?
    } else {
        let mut cmd = Command::new(&command);
        cmd.args(&args);
        cmd.current_dir(&workdir);
        cmd
    };
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    apply_outer_process_env(&mut cmd, &env);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning CRP runtime {provider_id} ({command_label})"))?;
    let stdout = child.stdout.take().context("capturing CRP stdout")?;
    let stderr = child.stderr.take().context("capturing CRP stderr")?;

    let stdout_tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stdout_tail_task = spawn_probe_output_tail_reader(
        stdout,
        provider_id.to_string(),
        "stdout",
        Arc::clone(&stdout_tail),
    );
    let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_tail_task = spawn_probe_output_tail_reader(
        stderr,
        provider_id.to_string(),
        "stderr",
        Arc::clone(&stderr_tail),
    );

    let started_at = tokio::time::Instant::now();
    loop {
        if let Some(exit_status) = child
            .try_wait()
            .with_context(|| format!("waiting for CRP runtime {provider_id} during launch probe"))?
        {
            let _ = tokio::time::timeout(Duration::from_millis(200), stdout_tail_task).await;
            let _ = tokio::time::timeout(Duration::from_millis(200), stderr_tail_task).await;
            let stdout_tail = format_probe_output_tail("stdout", &stdout_tail).await;
            let stderr_tail = format_probe_output_tail("stderr", &stderr_tail).await;
            anyhow::bail!(
                "CRP runtime exited during launch probe with status {exit_status}{stdout_tail}{stderr_tail}"
            );
        }
        if started_at.elapsed() >= probe_timeout {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn spawn_probe_output_tail_reader<R>(
    reader: R,
    provider_id: String,
    stream_name: &'static str,
    tail_ref: Arc<Mutex<Vec<String>>>,
) -> tokio::task::JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            tracing::debug!(
                provider_id = %provider_id,
                stream = stream_name,
                "crp {}: {}",
                stream_name,
                trimmed
            );
            let mut tail = tail_ref.lock().await;
            if tail.len() >= 20 {
                let _ = tail.remove(0);
            }
            tail.push(trimmed.to_string());
        }
    })
}

async fn wait_for_probe_output_tail_reader(task: &mut Option<tokio::task::JoinHandle<()>>) {
    if let Some(task) = task.take() {
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }
}

async fn format_probe_output_tail(label: &str, tail: &Arc<Mutex<Vec<String>>>) -> String {
    let lines = tail.lock().await;
    if lines.is_empty() {
        String::new()
    } else {
        format!("; {label}_tail={}", lines.join(" | "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    fn write_probe_script(dir: &tempfile::TempDir, body: &str) -> PathBuf {
        let script = dir.path().join("probe.sh");
        fs::write(&script, format!("#!/bin/sh\n{body}\n")).expect("write script");
        let mut perms = fs::metadata(&script).expect("stat script").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).expect("chmod script");
        script
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_crp_models_reads_models_list_response() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_probe_script(
            &tmp,
            "read _\necho '{\"seq\":1,\"channel\":\"control\",\"type\":\"models.list\",\"models\":[{\"id\":\"gpt-5\",\"name\":\"GPT-5\"}],\"current_model_id\":\"gpt-5\",\"catalog_source\":\"live_remote\"}'",
        );

        let probe = probe_crp_models(CrpModelsProbeRequest {
            provider_id: "codex".to_string(),
            command: script.to_string_lossy().to_string(),
            args: Vec::new(),
            workdir: tmp.path().to_path_buf(),
            env: HashMap::new(),
            host_timeout: Duration::from_secs(30),
            container_timeout: Duration::from_secs(45),
            crp_version: 1,
        })
        .await
        .expect("models probe should succeed");

        assert_eq!(probe.current_model_id.as_deref(), Some("gpt-5"));
        assert_eq!(probe.catalog_source.as_deref(), Some("live_remote"));
        assert_eq!(probe.models.len(), 1);
        assert_eq!(probe.models[0].id, "gpt-5");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_crp_runtime_launch_accepts_runtime_that_stays_alive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        probe_crp_runtime_launch(
            "codex",
            "/bin/sh".to_string(),
            vec!["-c".to_string(), "cat >/dev/null".to_string()],
            tmp.path().to_path_buf(),
            HashMap::new(),
            Duration::from_secs(2),
            Duration::from_secs(5),
        )
        .await
        .expect("launch probe should succeed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_crp_runtime_launch_reports_early_exit_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = probe_crp_runtime_launch(
            "opencode",
            "/bin/sh".to_string(),
            vec![
                "-c".to_string(),
                "echo bridge missing >&2\nexit 17".to_string(),
            ],
            tmp.path().to_path_buf(),
            HashMap::new(),
            Duration::from_secs(2),
            Duration::from_secs(5),
        )
        .await
        .expect_err("launch probe should fail");
        let msg = err.to_string();
        assert!(msg.contains("launch probe"));
        assert!(msg.contains("bridge missing"));
        assert!(msg.contains("exit status: 17"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_crp_runtime_launch_routes_shared_vm_container_through_helper() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let helper = write_probe_script(&tmp, "sleep 10");

        let mut env = HashMap::new();
        env.insert(
            "CTX_HARNESS_RUNTIME_KIND".to_string(),
            "shared_vm_container".to_string(),
        );
        env.insert(
            "CTX_AVF_LINUX_HELPER_PATH".to_string(),
            helper.to_string_lossy().to_string(),
        );
        env.insert(
            "CTX_AVF_HOST_DATA_ROOT".to_string(),
            tmp.path()
                .join("ctx-data-root")
                .to_string_lossy()
                .to_string(),
        );
        env.insert("CTX_AVF_WORKSPACE_ID".to_string(), "ws-123".to_string());
        env.insert("CTX_AVF_WORKTREE_ID".to_string(), "wt-456".to_string());
        env.insert(
            "CTX_AVF_HOST_WORKTREE_ROOT".to_string(),
            tmp.path().join("repo").to_string_lossy().to_string(),
        );
        env.insert(
            "CTX_AVF_GUEST_WORKTREE_ROOT".to_string(),
            "/ctx/ws/worktrees/wt-456".to_string(),
        );
        env.insert(
            "CTX_HARNESS_GUEST_WORKSPACE_ROOT".to_string(),
            "/ctx/ws".to_string(),
        );

        let host_workdir = tmp.path().join("repo").join("src");
        fs::create_dir_all(&host_workdir).expect("create host workdir");

        probe_crp_runtime_launch(
            "codex",
            "/bin/sh".to_string(),
            vec!["-c".to_string(), "cat >/dev/null".to_string()],
            host_workdir,
            env.clone(),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .expect("AVF shared-vm launch probe should succeed");

        let spec = container_exec_spec(&env).expect("shared-vm container spec");
        let cmd = build_container_exec_command(
            &spec,
            &tmp.path().join("repo").join("src"),
            &env,
            "/bin/sh",
            &["-c".to_string(), "cat >/dev/null".to_string()],
        )
        .expect("build shared-vm container probe command");
        let args = cmd
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(args.first().map(String::as_str), Some("shared-vm-exec"));
        assert!(
            args.windows(2)
                .any(|window| window[0] == "--command" && window[1] == "nerdctl"),
            "missing helper command routing in args: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|window| window[0] == "--workdir"
                    && window[1] == "/ctx/ws/worktrees/wt-456/src"),
            "missing guest workdir in args: {args:?}"
        );
        assert!(args.iter().any(|arg| arg == "ctx-harness-ws-123"));
        assert!(args.iter().any(|arg| arg == "/bin/sh"));
        assert!(args.iter().any(|arg| arg == "--env"));
        assert!(args.iter().any(|arg| arg.starts_with("XDG_RUNTIME_DIR=")));
        assert!(args.iter().any(|arg| arg.starts_with("HOME=")));
        assert!(args.iter().any(|arg| arg.starts_with("TMPDIR=")));
        assert!(args.iter().any(|arg| {
            arg == &format!(
                "XDG_RUNTIME_DIR={}",
                tmp.path()
                    .join("ctx-data-root")
                    .join("sandbox")
                    .join("run")
                    .display()
            )
        }));
        assert!(args.iter().any(|arg| {
            arg == &format!(
                "HOME={}",
                tmp.path()
                    .join("ctx-data-root")
                    .join("sandbox")
                    .join("home")
                    .display()
            )
        }));
        assert!(args.iter().any(|arg| {
            arg == &format!(
                "TMPDIR={}",
                tmp.path()
                    .join("ctx-data-root")
                    .join("sandbox")
                    .join("tmp")
                    .display()
            )
        }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_crp_models_reports_stderr_tail_when_runtime_closes_early() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let err = probe_crp_models(CrpModelsProbeRequest {
            provider_id: "codex".to_string(),
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "read _\necho 'codex auth import unreadable' >&2\nexit 23".to_string(),
            ],
            workdir: tmp.path().to_path_buf(),
            env: HashMap::new(),
            host_timeout: Duration::from_secs(10),
            container_timeout: Duration::from_secs(45),
            crp_version: 1,
        })
        .await
        .expect_err("models probe should surface early runtime close");

        let msg = err.to_string();
        assert!(msg.contains("closed before models.list response"), "{msg}");
        assert!(
            msg.contains("stderr_tail=codex auth import unreadable"),
            "{msg}"
        );
    }
}

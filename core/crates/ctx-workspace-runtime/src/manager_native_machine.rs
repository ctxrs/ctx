use super::*;
use anyhow::Context;
use ctx_harness_runtime::sandbox_machine_name;
use ctx_harness_setup::{
    observe_log, observe_phase, HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase,
};
use ctx_linux_sandbox_runtime::linux_sandbox_runtime_status;
use ctx_sandbox_container_runtime::{sandbox_container_command, SandboxCommandMode};
use ctx_workspace_container::sandbox_machine_required;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::sync::Mutex;

const SANDBOX_INFO_TIMEOUT: Duration = Duration::from_secs(5);
const SANDBOX_MACHINE_START_TIMEOUT: Duration = Duration::from_secs(180);
const SANDBOX_MACHINE_INIT_TIMEOUT: Duration = Duration::from_secs(8 * 60);

static SANDBOX_MACHINE_SINGLEFLIGHT_LOCKS: OnceLock<StdMutex<HashMap<String, Arc<Mutex<()>>>>> =
    OnceLock::new();

fn sandbox_machine_singleflight_lock(machine_name: &str) -> Arc<Mutex<()>> {
    let registry = SANDBOX_MACHINE_SINGLEFLIGHT_LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut guard = match registry.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .entry(machine_name.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn sandbox_machine_ready_timeout() -> Duration {
    if cfg!(test) {
        Duration::from_millis(300)
    } else {
        Duration::from_secs(2 * 60)
    }
}

fn sandbox_machine_ready_poll_interval() -> Duration {
    if cfg!(test) {
        Duration::from_millis(25)
    } else {
        Duration::from_secs(1)
    }
}

fn sandbox_machine_heartbeat_interval() -> Duration {
    if cfg!(test) {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(5)
    }
}

fn looks_like_missing_machine_error(message_lc: &str) -> bool {
    message_lc.contains("no such")
        || message_lc.contains("not found")
        || message_lc.contains("does not exist")
        || message_lc.contains("no machine")
}

fn looks_like_recoverable_machine_start_error(message_lc: &str) -> bool {
    message_lc.contains("already running")
        || message_lc.contains("already starting")
        || message_lc.contains("already started")
        || message_lc.contains("in progress")
}

fn format_heartbeat_elapsed(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    if minutes == 0 {
        format!("{seconds}s")
    } else {
        format!("{minutes}m {seconds}s")
    }
}

async fn wait_for_sandbox_machine_ready(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
    success_message: &str,
    last_err: &mut String,
) -> Result<bool> {
    let deadline = tokio::time::Instant::now() + sandbox_machine_ready_timeout();
    let started = tokio::time::Instant::now();
    let mut last_heartbeat = started;
    while tokio::time::Instant::now() < deadline {
        let mut cmd = sandbox_container_command(data_root, &SandboxCommandMode::NativeContainer)?;
        cmd.arg("info");
        match command_output_with_timeout(cmd, SANDBOX_INFO_TIMEOUT).await {
            Ok(out) if out.status.success() => {
                observe_log(
                    observer,
                    HarnessSetupPhase::MachineStartOrInit,
                    HarnessSetupLogLevel::Info,
                    success_message,
                );
                return Ok(true);
            }
            Ok(out) => {
                let combined = command_output_message(&out);
                if !combined.is_empty() {
                    *last_err = combined;
                }
            }
            Err(err) => *last_err = err.to_string(),
        }

        let now = tokio::time::Instant::now();
        if now.duration_since(last_heartbeat) >= sandbox_machine_heartbeat_interval() {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Info,
                &format!(
                    "still waiting for local sandbox runtime readiness ({} elapsed)",
                    format_heartbeat_elapsed(started.elapsed())
                ),
            );
            last_heartbeat = now;
        }
        tokio::time::sleep(sandbox_machine_ready_poll_interval()).await;
    }
    Ok(false)
}

async fn best_effort_start_machine_after_init(
    data_root: &Path,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
    last_err: &mut String,
) -> Result<()> {
    let mut start = sandbox_container_command(data_root, &SandboxCommandMode::NativeContainer)?;
    start.arg("machine").arg("start").arg(machine_name);
    match command_output_with_timeout(start, SANDBOX_MACHINE_START_TIMEOUT).await {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let combined = command_output_message(&out);
            if !combined.is_empty() {
                *last_err = combined.clone();
            }
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &format!("sandbox machine start after init returned non-zero: {combined}"),
            );
            Ok(())
        }
        Err(err) => {
            *last_err = err.to_string();
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &format!("sandbox machine start after init failed: {err}"),
            );
            Ok(())
        }
    }
}

async fn initialize_sandbox_machine(
    data_root: &Path,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
    last_err: &mut String,
) -> Result<()> {
    observe_phase(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        "materializing local sandbox runtime",
    );
    let mut init = sandbox_container_command(data_root, &SandboxCommandMode::NativeContainer)?;
    init.arg("machine").arg("init").arg(machine_name);
    let out = command_output_with_timeout(init, SANDBOX_MACHINE_INIT_TIMEOUT)
        .await
        .context("sandbox machine init")?;
    let combined = command_output_message(&out);
    if !out.status.success() {
        let combined_lc = combined.to_ascii_lowercase();
        if combined_lc.contains("already exists") {
            let message = if combined.is_empty() {
                "sandbox machine init reported existing machine; starting it explicitly".to_string()
            } else {
                format!(
                    "sandbox machine init reported existing machine; starting it explicitly: {combined}"
                )
            };
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &message,
            );
            if !combined.is_empty() {
                *last_err = combined;
            }
        } else if combined.is_empty() {
            anyhow::bail!("sandbox machine init failed (status: {})", out.status);
        } else {
            anyhow::bail!("sandbox machine init failed: {combined}");
        }
    } else if !combined.is_empty() {
        *last_err = combined;
    }

    best_effort_start_machine_after_init(data_root, machine_name, observer, last_err).await
}

impl HarnessRuntimeManager {
    pub(super) async fn ensure_native_container_machine_ready(
        &self,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        observe_phase(
            observer,
            HarnessSetupPhase::MachineCheck,
            "checking container runtime",
        );
        if sandbox_engine_ready(&self.data_root).await.unwrap_or(false) {
            observe_log(
                observer,
                HarnessSetupPhase::MachineCheck,
                HarnessSetupLogLevel::Info,
                "local sandbox runtime is already reachable",
            );
            return Ok(());
        }

        if !sandbox_machine_required() {
            if sandbox_cli_invocation(&self.data_root).is_err() {
                let bootstrap = linux_sandbox_runtime_status(&self.data_root).await?;
                anyhow::bail!("{}", bootstrap.message);
            }
            let bootstrap = linux_sandbox_runtime_status(&self.data_root).await?;
            anyhow::bail!("{}", bootstrap.message);
        }

        let machine_name = sandbox_machine_name(&self.data_root);
        let machine_lock = sandbox_machine_singleflight_lock(&machine_name);
        let _machine_guard = match machine_lock.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                observe_log(
                    observer,
                    HarnessSetupPhase::MachineStartOrInit,
                    HarnessSetupLogLevel::Info,
                    "waiting for concurrent local sandbox runtime init/start operation",
                );
                machine_lock.lock().await
            }
        };

        if sandbox_engine_ready(&self.data_root).await.unwrap_or(false) {
            observe_log(
                observer,
                HarnessSetupPhase::MachineCheck,
                HarnessSetupLogLevel::Info,
                "local sandbox runtime is already reachable",
            );
            return Ok(());
        }

        let mut last_err = String::new();
        observe_phase(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            "starting or initializing local sandbox runtime",
        );
        let mut start =
            sandbox_container_command(&self.data_root, &SandboxCommandMode::NativeContainer)?;
        start.arg("machine").arg("start").arg(&machine_name);
        let start_out = command_output_with_timeout(start, SANDBOX_MACHINE_START_TIMEOUT).await?;
        if start_out.status.success() {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Info,
                "local sandbox runtime start command completed; waiting for readiness",
            );
        } else {
            let combined = command_output_message(&start_out);
            let combined_lc = combined.to_ascii_lowercase();
            if looks_like_missing_machine_error(&combined_lc) {
                initialize_sandbox_machine(&self.data_root, &machine_name, observer, &mut last_err)
                    .await?;
            } else if looks_like_recoverable_machine_start_error(&combined_lc) {
                let message = if combined.is_empty() {
                    "sandbox machine start returned a recoverable error; waiting for readiness"
                        .to_string()
                } else {
                    format!(
                        "sandbox machine start returned recoverable error; waiting for readiness: {combined}"
                    )
                };
                observe_log(
                    observer,
                    HarnessSetupPhase::MachineStartOrInit,
                    HarnessSetupLogLevel::Warn,
                    &message,
                );
                if !combined.is_empty() {
                    last_err = combined;
                }
            } else if combined.is_empty() {
                anyhow::bail!(
                    "sandbox machine start failed with non-zero exit {}",
                    start_out.status
                );
            } else {
                anyhow::bail!("sandbox machine start failed: {combined}");
            }
        }

        observe_phase(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            "waiting for local sandbox runtime readiness",
        );
        if wait_for_sandbox_machine_ready(
            &self.data_root,
            observer,
            "local sandbox runtime is ready",
            &mut last_err,
        )
        .await?
        {
            return Ok(());
        }

        if last_err.trim().is_empty() {
            anyhow::bail!("sandbox machine did not become reachable after start");
        }
        anyhow::bail!(
            "sandbox machine did not become reachable after start: {}",
            last_err.trim()
        );
    }
}

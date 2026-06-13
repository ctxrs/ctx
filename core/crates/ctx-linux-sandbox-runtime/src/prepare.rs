use super::*;

pub async fn prepare_linux_sandbox_runtime(
    data_root: &Path,
    activation_mode: LinuxSandboxActivationMode,
    sudo_password: Option<&str>,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<LinuxSandboxRuntimePrepareResult> {
    let staged_status = stage_linux_sandbox_runtime_downloads(data_root, observer).await?;
    if staged_status.state == LinuxSandboxRuntimeState::Ready {
        return Ok(LinuxSandboxRuntimePrepareResult {
            ready: true,
            needs_password: false,
            message: "Linux sandbox runtime is ready.".to_string(),
            status: staged_status,
        });
    }
    if !staged_status.supported {
        return Ok(LinuxSandboxRuntimePrepareResult {
            ready: false,
            needs_password: false,
            message: staged_status.message.clone(),
            status: staged_status,
        });
    }

    observe_phase(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        "preparing Linux sandbox runtime",
    );
    observe_log(
        observer,
        HarnessSetupPhase::MachineStartOrInit,
        HarnessSetupLogLevel::Info,
        "activating Linux sandbox runtime for sandbox use",
    );

    let paths = linux_sandbox_bootstrap_paths(data_root);
    ensure_linux_sandbox_bootstrap_script(&paths).await?;
    let user_name = current_username()?;
    let args = activation_args(data_root, &user_name);

    match activation_mode {
        LinuxSandboxActivationMode::Local => {
            let output = try_sudo_non_interactive(&args).await?;
            if !output.status.success() {
                if sudo_needs_password(&output) && sudo_password.is_none() {
                    return Ok(LinuxSandboxRuntimePrepareResult {
                        ready: false,
                        needs_password: true,
                        message: "Preparing Linux sandbox runtime needs the local admin password."
                            .to_string(),
                        status: LinuxSandboxRuntimeStatus {
                            state: LinuxSandboxRuntimeState::Activating,
                            message:
                                "Preparing Linux sandbox runtime needs the local admin password."
                                    .to_string(),
                            ..staged_status
                        },
                    });
                }
                if let Some(password) = sudo_password {
                    let output = run_sudo_with_password(&args, password).await?;
                    if !output.status.success() {
                        if sudo_needs_password(&output) {
                            return Ok(LinuxSandboxRuntimePrepareResult {
                                ready: false,
                                needs_password: true,
                                message:
                                    "Preparing Linux sandbox runtime needs the local admin password."
                                        .to_string(),
                                status: LinuxSandboxRuntimeStatus {
                                    state: LinuxSandboxRuntimeState::Activating,
                                    message:
                                        "Preparing Linux sandbox runtime needs the local admin password."
                                            .to_string(),
                                    ..staged_status
                                },
                            });
                        }
                        let detail = command_output_message(&output);
                        anyhow::bail!(
                            "Preparing Linux sandbox runtime failed. {}",
                            if detail.is_empty() {
                                "ctx couldn't prepare the sandbox runtime on this machine."
                                    .to_string()
                            } else {
                                detail
                            }
                        );
                    }
                } else {
                    let detail = command_output_message(&output);
                    tracing::warn!(target: "linux_sandbox", detail = %redact_sensitive(&detail), "Preparing Linux sandbox runtime failed during activation");
                    anyhow::bail!("Preparing Linux sandbox runtime failed. ctx couldn't prepare the sandbox runtime on this machine.");
                }
            }
        }
        LinuxSandboxActivationMode::Remote => {
            let output = try_sudo_non_interactive(&args).await?;
            if !output.status.success() {
                if sudo_needs_password(&output) && sudo_password.is_none() {
                    return Ok(LinuxSandboxRuntimePrepareResult {
                        ready: false,
                        needs_password: true,
                        message:
                            "Preparing sandbox on remote host needs the remote admin password."
                                .to_string(),
                        status: LinuxSandboxRuntimeStatus {
                            state: LinuxSandboxRuntimeState::Activating,
                            message:
                                "Preparing sandbox on remote host needs the remote admin password."
                                    .to_string(),
                            ..staged_status
                        },
                    });
                }
                if let Some(password) = sudo_password {
                    let output = run_sudo_with_password(&args, password).await?;
                    if !output.status.success() {
                        if sudo_needs_password(&output) {
                            return Ok(LinuxSandboxRuntimePrepareResult {
                                ready: false,
                                needs_password: true,
                                message:
                                    "Preparing sandbox on remote host needs the remote admin password."
                                        .to_string(),
                                status: LinuxSandboxRuntimeStatus {
                                    state: LinuxSandboxRuntimeState::Activating,
                                    message:
                                        "Preparing sandbox on remote host needs the remote admin password."
                                            .to_string(),
                                    ..staged_status
                                },
                            });
                        }
                        let detail = command_output_message(&output);
                        tracing::warn!(target: "linux_sandbox", detail = %redact_sensitive(&detail), "Preparing sandbox on remote host failed during activation");
                        anyhow::bail!("Preparing sandbox on remote host failed. ctx couldn't prepare the sandbox runtime on this host.");
                    }
                } else {
                    let detail = command_output_message(&output);
                    tracing::warn!(target: "linux_sandbox", detail = %redact_sensitive(&detail), "Preparing sandbox on remote host failed during activation");
                    anyhow::bail!("Preparing sandbox on remote host failed. ctx couldn't prepare the sandbox runtime on this host.");
                }
            }
        }
    }

    let status = linux_sandbox_runtime_status(data_root).await?;
    if status.state == LinuxSandboxRuntimeState::Ready {
        return Ok(LinuxSandboxRuntimePrepareResult {
            ready: true,
            needs_password: false,
            message: "Linux sandbox runtime is ready.".to_string(),
            status,
        });
    }

    anyhow::bail!(
        "{}",
        match activation_mode {
            LinuxSandboxActivationMode::Local => {
                "Preparing Linux sandbox runtime failed. ctx couldn't verify the sandbox runtime after activation."
            }
            LinuxSandboxActivationMode::Remote => {
                "Preparing sandbox on remote host failed. ctx couldn't verify the sandbox runtime after activation."
            }
        }
    )
}

//! CLI-only setup sequencing over neutral daemon lifecycle operations.

use super::*;

fn application_config(
    config: &DaemonRuntimeConfig,
) -> ctx_daemon_application::DaemonConfigSnapshot {
    ctx_daemon_application::DaemonConfigSnapshot {
        enabled: config.daemon.enabled,
        mode: super::super::daemon_supervisor::daemon_mode(config.daemon.mode),
        semantic_enabled: config.semantic_search_enabled(),
        semantic_executor: config
            .semantic_embedding_executor()
            .http_endpoint()
            .unwrap_or("builtin")
            .to_owned(),
        semantic_contract_fingerprint: config.semantic_model_contract().fingerprint().to_owned(),
        semantic_builtin_throttling_configured: config.semantic_builtin_throttling_configured(),
        semantic_builtin_throttling_effective: config.semantic_builtin_throttling_effective(),
    }
}

fn application_trigger(trigger: DaemonTriggerCommandArg) -> ctx_daemon_application::DaemonTrigger {
    super::super::daemon_supervisor::daemon_trigger(trigger)
}

#[cfg(test)]
pub(super) fn daemon_autostart_allowed(data_root: &Path, config: &DaemonRuntimeConfig) -> bool {
    ctx_daemon_application::daemon_autostart_allowed(data_root, &application_config(config))
}

pub fn daemon_autostart_suppression_reason() -> Option<&'static str> {
    ctx_daemon_application::daemon_autostart_suppression_reason()
}

pub fn maybe_autostart_daemon(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    trigger: DaemonTriggerCommandArg,
) {
    if !config.daemon.enabled {
        return;
    }
    super::super::daemon_supervisor::with_daemon_application(|application| {
        if application.daemon_start_is_fenced() {
            return;
        }
        if application
            .active_daemon_matches_current_executable(data_root)
            .unwrap_or(false)
        {
            return;
        }
        if daemon_autostart_suppression_reason().is_none() {
            match super::super::daemon_supervisor::ensure_daemon_supervisor_with_policy(
                application,
                data_root,
                ctx_daemon_application::SupervisorMutationPolicy::PreserveLiveOwner,
            ) {
                Ok(_) => {}
                Err(_) => return,
            }
        }
        let _ = application.request_daemon_start(
            data_root,
            &application_config(config),
            application_trigger(trigger),
        );
    });
}

pub fn start_finite_core_worker_and_wait(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    trigger: DaemonTriggerCommandArg,
) -> Result<ctx_daemon_application::FiniteCoreWorkerLease> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        let mut effective = application_config(config);
        effective.enabled = false;
        effective.mode = ctx_daemon_application::DaemonMode::SourceRefreshOnly;
        effective.semantic_enabled = false;
        let handoff = application
            .start_finite_core_worker_and_wait_with_cancellation(
                data_root,
                &effective,
                application_trigger(trigger),
                &mut super::super::finite_worker_owner::checkpoint,
            )
            .map_err(|error| match error {
                ctx_daemon_application::DaemonStartError::Suppressed(reason) => anyhow!(
                    "ctx finite Core worker start was suppressed ({reason}); retry after it clears"
                ),
                ctx_daemon_application::DaemonStartError::BinaryIdentity(error) => error,
                ctx_daemon_application::DaemonStartError::Start(error)
                    if super::super::finite_worker_owner::finite_worker_interrupted(&error) =>
                {
                    error
                }
                ctx_daemon_application::DaemonStartError::Start(error) => {
                    anyhow!("ctx finite Core worker did not start: {error:#}")
                }
                ctx_daemon_application::DaemonStartError::Ready(error) => {
                    error.context("ctx finite Core worker did not become ready")
                }
            })?;
        Ok(handoff)
    })
}

pub fn autostart_daemon_and_wait(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonHandoff> {
    Ok(autostart_daemon_for_setup_and_wait(data_root, config, trigger)?.handoff)
}

pub fn autostart_core_daemon_and_wait(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonHandoff> {
    Ok(autostart_persistent_daemon_and_wait(
        data_root,
        config,
        trigger,
        PersistentDaemonReadiness::Core,
        &mut super::super::finite_worker_owner::checkpoint,
    )?
    .handoff)
}

pub fn restart_daemon_with_current_environment_and_wait(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonHandoff> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        let handoff = application
            .restart_daemon_with_current_environment(
                data_root,
                &application_config(config),
                application_trigger(trigger),
            )
            .map_err(|error| match error {
                ctx_daemon_application::DaemonStartError::Suppressed(reason) => anyhow!(
                    "ctx daemon credential-bound restart was suppressed ({reason}); retry after it clears"
                ),
                ctx_daemon_application::DaemonStartError::BinaryIdentity(error) => error,
                ctx_daemon_application::DaemonStartError::Start(error) => {
                    anyhow!(
                        "{}",
                        credential_restart_start_failure_message(
                            native_supervisor_is_active(data_root),
                            &error,
                        )
                    )
                }
                ctx_daemon_application::DaemonStartError::Ready(error) => {
                    anyhow!(
                        "{}",
                        credential_restart_ready_failure_message(
                            native_supervisor_is_active(data_root),
                            &error,
                        )
                    )
                }
            })?;
        Ok(DaemonHandoff {
            pid: handoff.pid,
            heartbeat_at_ms: handoff.heartbeat_at_ms,
        })
    })
}

pub fn autostart_daemon_for_setup_and_wait(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonSetupHandoff> {
    autostart_persistent_daemon_and_wait(
        data_root,
        config,
        trigger,
        PersistentDaemonReadiness::Full,
        &mut || Ok(()),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistentDaemonReadiness {
    Full,
    Core,
}

fn supervisor_mutation_policy_for_trigger(
    trigger: DaemonTriggerCommandArg,
) -> ctx_daemon_application::SupervisorMutationPolicy {
    match trigger {
        DaemonTriggerCommandArg::Setup => {
            ctx_daemon_application::SupervisorMutationPolicy::AllowReinstall
        }
        DaemonTriggerCommandArg::Import
        | DaemonTriggerCommandArg::Search
        | DaemonTriggerCommandArg::Semantic => {
            ctx_daemon_application::SupervisorMutationPolicy::PreserveLiveOwner
        }
    }
}

fn autostart_persistent_daemon_and_wait(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    trigger: DaemonTriggerCommandArg,
    readiness: PersistentDaemonReadiness,
    checkpoint: &mut dyn FnMut() -> Result<()>,
) -> Result<DaemonSetupHandoff> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        autostart_persistent_daemon_and_wait_with(
            application,
            data_root,
            config,
            trigger,
            readiness,
            checkpoint,
            |application, data_root, policy| {
                super::super::daemon_supervisor::ensure_daemon_supervisor_with_policy(
                    application,
                    data_root,
                    policy,
                )
            },
        )
    })
}

fn autostart_persistent_daemon_and_wait_with(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    trigger: DaemonTriggerCommandArg,
    readiness: PersistentDaemonReadiness,
    checkpoint: &mut dyn FnMut() -> Result<()>,
    ensure_supervisor: impl FnOnce(
        &ctx_daemon_application::DaemonApplication<'_>,
        &Path,
        ctx_daemon_application::SupervisorMutationPolicy,
    )
        -> Result<super::super::daemon_supervisor::DaemonSupervisorStart>,
) -> Result<DaemonSetupHandoff> {
    checkpoint()?;
    if application.daemon_start_is_fenced() {
        return Err(anyhow!(
            "ctx daemon start was suppressed (hosted_uninstall_active); retry after it clears or run `ctx setup --no-daemon`"
        ));
    }
    let app_config = application_config(config);
    let policy = supervisor_mutation_policy_for_trigger(trigger);
    let reuse_live_owner = readiness == PersistentDaemonReadiness::Core
        && (application
            .observe_ready_core_daemon(data_root, &app_config)?
            .is_some()
            || core_daemon_has_live_same_executable_owner(application, data_root, &app_config)?);
    let reconcile_supervisor = !reuse_live_owner
        || policy == ctx_daemon_application::SupervisorMutationPolicy::AllowReinstall
        || !application
            .supervisor_matches_live_owner_without_drift(data_root, &app_config)
            .unwrap_or(false);
    // Keep the zero-subprocess fast path on steady-state retrieval when a live
    // Core daemon already matches a healthy local supervisor receipt without
    // drift. Reconcile under the trigger's policy only on drift or unhealthy /
    // missing receipt state (`PreserveLiveOwner` for retrieval paths).
    if reconcile_supervisor && daemon_autostart_suppression_reason().is_none() {
        let supervisor_result = ensure_supervisor(application, data_root, policy);
        if !reuse_live_owner {
            supervisor_result.context("establish ctx daemon supervision")?;
        }
    }
    checkpoint()?;
    let handoff = match readiness {
        PersistentDaemonReadiness::Full => application.start_daemon_and_wait_with_cancellation(
            data_root,
            &app_config,
            application_trigger(trigger),
            checkpoint,
        ),
        PersistentDaemonReadiness::Core => application
            .start_core_daemon_and_wait_with_cancellation(
                data_root,
                &app_config,
                application_trigger(trigger),
                checkpoint,
            ),
    }
    .map_err(|error| match error {
        ctx_daemon_application::DaemonStartError::Suppressed(reason) => {
            ctx_daemon_service::SourceBackedRefreshDaemonUnavailable::new(Some(format!(
                "ctx daemon start was suppressed ({reason}); retry after it clears or run `ctx setup --no-daemon`"
            )))
            .into()
        }
        ctx_daemon_application::DaemonStartError::BinaryIdentity(error) => error,
        ctx_daemon_application::DaemonStartError::Start(error)
        | ctx_daemon_application::DaemonStartError::Ready(error)
            if super::super::finite_worker_owner::finite_worker_interrupted(&error) =>
        {
            error
        }
        ctx_daemon_application::DaemonStartError::Start(error) => {
            error.context(daemon_start_failure_message(native_supervisor_is_active(data_root)))
        }
        ctx_daemon_application::DaemonStartError::Ready(error) => {
            error.context(daemon_ready_failure_message(native_supervisor_is_active(data_root)))
        }
    })?;
    Ok(DaemonSetupHandoff {
        handoff: DaemonHandoff {
            pid: handoff.pid,
            heartbeat_at_ms: handoff.heartbeat_at_ms,
        },
    })
}

pub fn observe_daemon_for_setup_and_wait(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
) -> Result<DaemonSetupHandoff> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        let handoff = application
            .observe_daemon_and_wait(data_root, &application_config(config))
            .context("observe ready ctx daemon at setup output boundary")?;
        Ok(DaemonSetupHandoff {
            handoff: DaemonHandoff {
                pid: handoff.pid,
                heartbeat_at_ms: handoff.heartbeat_at_ms,
            },
        })
    })
}

#[cfg(test)]
pub(super) fn handoff_mismatched_daemon_owner(data_root: &Path, executable: &Path) -> Result<()> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        application.handoff_mismatched_daemon_owner(data_root, executable)
    })
}

pub(super) fn daemon_autostart_command(
    exe: &Path,
    root: &Path,
    trigger: DaemonTriggerCommandArg,
    interval: Option<u64>,
    token: Option<&str>,
) -> io::Result<NormalizedLaunch> {
    ctx_daemon_application::daemon_autostart_command(
        exe,
        root,
        application_trigger(trigger),
        interval,
        token,
    )
}
#[cfg(test)]
pub(super) fn configured_daemon_autostart_command(
    exe: &Path,
    root: &Path,
    trigger: DaemonTriggerCommandArg,
    token: Option<&str>,
) -> io::Result<NormalizedLaunch> {
    ctx_daemon_application::configured_daemon_autostart_command(
        exe,
        root,
        application_trigger(trigger),
        token,
    )
}
pub(super) fn spawn_daemon_child(launch: NormalizedLaunch) -> io::Result<Child> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        application.spawn_daemon_child(launch)
    })
}
pub(super) fn spawn_daemon_child_for_upgrade_handoff(
    launch: NormalizedLaunch,
    executable: &Path,
) -> io::Result<Child> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        application.spawn_daemon_child_for_upgrade_handoff(launch, executable)
    })
}
pub(super) fn daemon_restart_allowed(root: &Path) -> Result<bool> {
    super::super::daemon_supervisor::with_daemon_application(|application| {
        application.daemon_restart_allowed(root)
    })
}
pub(super) fn daemon_restart_trigger(root: &Path) -> Option<DaemonTriggerCommandArg> {
    ctx_daemon_application::daemon_restart_trigger(root)
        .map(super::super::daemon_supervisor::daemon_trigger_arg)
}
pub(super) fn parse_daemon_trigger(value: Option<&str>) -> Option<DaemonTriggerCommandArg> {
    ctx_daemon_application::parse_persisted_trigger(value)
        .map(super::super::daemon_supervisor::daemon_trigger_arg)
}

fn daemon_autostart_exe_path() -> Result<PathBuf> {
    env::var("CTX_DAEMON_AUTOSTART_EXE")
        .ok()
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| env::current_exe().context("resolve ctx daemon autostart executable"))
}

fn core_daemon_has_live_same_executable_owner(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    data_root: &Path,
    config: &ctx_daemon_application::DaemonConfigSnapshot,
) -> Result<bool> {
    if !config.enabled || !application.active_daemon_matches_current_executable(data_root)? {
        return Ok(false);
    }
    let Some(lock) = ctx_daemon_runtime::read_pid_lock_json(&daemon_lock_path(data_root)) else {
        return Ok(false);
    };
    let executable = daemon_autostart_exe_path()?;
    if !matches!(
        ctx_daemon_runtime::daemon_owner_metadata_identity_matches(&lock, &executable),
        Ok(true)
    ) {
        return Ok(false);
    }
    let Some(pid) = ctx_daemon_runtime::pid_from_lock_json(&lock) else {
        return Ok(false);
    };
    if !daemon_lock_is_owned_by(data_root, pid) {
        return Ok(false);
    }
    let Some(started_at_ms) = lock
        .get("started_at_ms")
        .and_then(Value::as_i64)
        .filter(|started| *started > 0)
    else {
        return Ok(false);
    };
    if !ctx_daemon_application::daemon_owner_has_fresh_heartbeat_or_is_starting(
        ctx_daemon_runtime::read_daemon_status(data_root).as_ref(),
        pid,
        started_at_ms,
        utc_now().timestamp_millis(),
    ) {
        return Ok(false);
    }
    Ok(
        ctx_daemon_runtime::read_pid_lock_json(&daemon_lock_path(data_root)).as_ref()
            == Some(&lock)
            && application.active_daemon_matches_current_executable(data_root)?,
    )
}

fn native_supervisor_is_active(data_root: &Path) -> bool {
    let path = daemon_root_path(data_root).join("supervisor.json");
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(receipt) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    native_supervisor_receipt_is_active(&receipt)
}

fn native_supervisor_receipt_is_active(receipt: &Value) -> bool {
    let native_kind = matches!(
        receipt.get("kind").and_then(Value::as_str),
        Some("systemd_user" | "launch_agent" | "windows_task_scheduler")
    );
    let active_status = matches!(
        receipt.get("status").and_then(Value::as_str),
        Some(
            "installed" | "registered_not_running" | "registered_unverified" | "stale_registration"
        )
    );
    native_kind && active_status
}

fn daemon_start_failure_message(supervised: bool) -> &'static str {
    if supervised {
        "ctx daemon did not start. Run `ctx status --format json` to inspect the managed service"
    } else {
        "ctx daemon did not start. Run `ctx status --format json`, then `ctx daemon run` for details"
    }
}

fn daemon_ready_failure_message(supervised: bool) -> &'static str {
    if supervised {
        "ctx daemon did not become ready. Run `ctx status --format json` to inspect the managed service and wait for startup to finish"
    } else {
        "ctx daemon did not become ready. Run `ctx status --format json`, then `ctx daemon run` for details"
    }
}

fn credential_restart_start_failure_message(supervised: bool, error: &anyhow::Error) -> String {
    if supervised {
        format!(
            "ctx daemon credential-bound restart failed: {error:#}. Run `ctx status --format json` to inspect the managed service"
        )
    } else {
        format!(
            "ctx daemon credential-bound restart failed: {error:#}. Run `ctx status --format json`, then `ctx daemon run` for details"
        )
    }
}

fn credential_restart_ready_failure_message(supervised: bool, error: &anyhow::Error) -> String {
    if supervised {
        format!(
            "ctx daemon did not become ready after credential-bound restart: {error}. Run `ctx status --format json` to inspect the managed service and wait for startup to finish"
        )
    } else {
        format!(
            "ctx daemon did not become ready after credential-bound restart: {error}. Run `ctx status --format json`, then `ctx daemon run` for details"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_daemon_runtime::{write_daemon_status, DaemonLock};

    #[test]
    fn core_reuse_guard_preserves_live_same_executable_owner_even_without_ipc_and_rejects_failed(
    ) -> Result<()> {
        let temp = tempfile::tempdir()?;
        let config = application_config(&DaemonRuntimeConfig::default());
        let lock = DaemonLock::acquire(temp.path())?
            .ok_or_else(|| anyhow!("test process could not acquire daemon lock"))?;
        let started_at_ms = lock.started_at_ms().expect("lock start timestamp");

        // No IPC endpoint and no status.json yet (starting up) -> still preserved!
        let preserved = super::super::super::daemon_supervisor::with_daemon_application(|app| {
            core_daemon_has_live_same_executable_owner(app, temp.path(), &config)
        })?;
        assert!(preserved);

        // Running status without IPC -> preserved!
        write_daemon_status(
            temp.path(),
            &json!({
                "status": "running",
                "pid": process::id(),
                "started_at_ms": started_at_ms,
                "heartbeat_at_ms": utc_now().timestamp_millis(),
            }),
        )?;
        let preserved = super::super::super::daemon_supervisor::with_daemon_application(|app| {
            core_daemon_has_live_same_executable_owner(app, temp.path(), &config)
        })?;
        assert!(preserved);

        // Stale heartbeat with applied config -> NOT preserved.
        write_daemon_status(
            temp.path(),
            &json!({
                "status": "running",
                "pid": process::id(),
                "started_at_ms": started_at_ms,
                "heartbeat_at_ms": utc_now().timestamp_millis() - 60_000,
                "config_reload": { "status": "applied" },
            }),
        )?;
        let preserved = super::super::super::daemon_supervisor::with_daemon_application(|app| {
            core_daemon_has_live_same_executable_owner(app, temp.path(), &config)
        })?;
        assert!(!preserved);

        // Stale heartbeat while config reload is still pending -> preserved!
        write_daemon_status(
            temp.path(),
            &json!({
                "status": "running",
                "pid": process::id(),
                "started_at_ms": started_at_ms,
                "heartbeat_at_ms": utc_now().timestamp_millis() - 60_000,
                "config_reload": { "status": "pending" },
            }),
        )?;
        let preserved = super::super::super::daemon_supervisor::with_daemon_application(|app| {
            core_daemon_has_live_same_executable_owner(app, temp.path(), &config)
        })?;
        assert!(preserved);

        // Explicitly failed status for this owner -> NOT preserved.
        write_daemon_status(
            temp.path(),
            &json!({
                "status": "failed",
                "pid": process::id(),
                "started_at_ms": started_at_ms,
                "heartbeat_at_ms": utc_now().timestamp_millis(),
            }),
        )?;
        let preserved = super::super::super::daemon_supervisor::with_daemon_application(|app| {
            core_daemon_has_live_same_executable_owner(app, temp.path(), &config)
        })?;
        assert!(!preserved);

        // Missing status.json with stale started_at_ms in lock -> NOT preserved.
        fs::remove_file(ctx_daemon_runtime::daemon_status_path(temp.path()))?;
        let lock_path = daemon_lock_path(temp.path());
        let original_lock = fs::read(&lock_path)?;
        let mut stale_lock: Value = serde_json::from_slice(&original_lock)?;
        stale_lock["started_at_ms"] = json!(utc_now().timestamp_millis() - 60_000);
        fs::write(&lock_path, serde_json::to_vec(&stale_lock)?)?;
        let preserved = super::super::super::daemon_supervisor::with_daemon_application(|app| {
            core_daemon_has_live_same_executable_owner(app, temp.path(), &config)
        })?;
        assert!(!preserved);
        fs::write(&lock_path, original_lock)?;

        drop(lock);
        let preserved = super::super::super::daemon_supervisor::with_daemon_application(|app| {
            core_daemon_has_live_same_executable_owner(app, temp.path(), &config)
        })?;
        assert!(!preserved);
        Ok(())
    }

    #[test]
    fn autostart_persistent_daemon_and_wait_preserves_fast_path_and_reconciles_drift_under_policy(
    ) -> Result<()> {
        struct ReadyLiveOwnerHost {
            config: ctx_daemon_application::DaemonConfigSnapshot,
            managed_executable: Option<PathBuf>,
        }

        impl ctx_daemon_application::DaemonApplicationHost for ReadyLiveOwnerHost {
            fn hosted_uninstall_active(&self) -> Result<bool> {
                Ok(false)
            }
            fn hosted_uninstall_active_for_executable(&self, _executable: &Path) -> Result<bool> {
                Ok(false)
            }
            fn managed_install_executable(&self) -> Result<Option<PathBuf>> {
                Ok(self.managed_executable.clone())
            }
            fn installation_upgrade_active(&self) -> Result<bool> {
                Ok(false)
            }
            fn automatic_upgrade_recovery_allowed(&self, _data_root: &Path) -> Result<bool> {
                Ok(false)
            }
            fn daemon_config(
                &self,
                _data_root: &Path,
            ) -> Result<ctx_daemon_application::DaemonConfigSnapshot> {
                Ok(self.config.clone())
            }
            fn observe_daemon_config(
                &self,
                _data_root: &Path,
            ) -> Result<ctx_daemon_application::DaemonConfigSnapshot> {
                Ok(self.config.clone())
            }
            fn persisted_daemon_enabled(&self, _data_root: &Path) -> Result<bool> {
                Ok(self.config.enabled)
            }
            fn defer_restart_for_upgrade_handoff(
                &self,
                _data_root: &Path,
                _trigger: ctx_daemon_application::DaemonTrigger,
            ) -> Result<Option<ctx_daemon_runtime::DaemonHandoffRestartDeferral>> {
                Ok(None)
            }
            fn request_lifecycle_wakeup(
                &self,
                _data_root: &Path,
                _request: Value,
                _timeout: StdDuration,
                _response_limit: u64,
            ) -> Result<Option<Value>> {
                Ok(Some(json!({
                    "schema_version": 1,
                    "ok": true,
                    "owner": "daemon",
                    "service": "lifecycle",
                    "pid": process::id(),
                    "readiness": "ready",
                })))
            }
            fn home_dir(&self) -> Option<PathBuf> {
                env::var_os("HOME").map(PathBuf::from)
            }
            fn run_daemon_service(
                &self,
                _data_root: &Path,
                _request: ctx_daemon_application::DaemonHostRunRequest,
            ) -> Result<()> {
                Ok(())
            }
            fn set_daemon_enabled(&self, _data_root: &Path, _enabled: bool) -> Result<()> {
                Ok(())
            }
            fn request_daemon_shutdown(
                &self,
                _data_root: &Path,
                _timeout: StdDuration,
                _response_limit: u64,
            ) -> Result<()> {
                Ok(())
            }
            fn terminate_current_executable_daemon(&self, _data_root: &Path) -> Result<()> {
                Ok(())
            }
            fn remove_released_daemon_service_artifacts(&self, _data_root: &Path) -> Result<()> {
                Ok(())
            }
            fn cancel_core_finalization_generation_lease(
                &self,
                _data_root: &Path,
                _reason: &str,
            ) -> Result<()> {
                Ok(())
            }
            fn observe_source_refresh_endpoint(
                &self,
                _identity_path: &Path,
            ) -> ctx_daemon_application::DaemonEndpointObservation {
                ctx_daemon_application::DaemonEndpointObservation::default()
            }
            fn deliver_daemon_events(
                &self,
                _data_root: &Path,
                _events: &[ctx_client_observability::analytics::PublicEventV1],
            ) {
            }
        }

        let environment = crate::test_environment::EnvironmentGuard::capture(&[
            "CI",
            DAEMON_AUTOSTART_OFF_ENV,
            DAEMON_BACKGROUND_CHILD_ENV,
            "CTX_DATA_ROOT",
        ]);
        environment.set("CI", None);
        environment.set(DAEMON_AUTOSTART_OFF_ENV, None);
        environment.set(DAEMON_BACKGROUND_CHILD_ENV, None);

        let temp = tempfile::tempdir()?;
        environment.set("CTX_DATA_ROOT", Some(temp.path().as_os_str()));
        let config = DaemonRuntimeConfig::default();
        let app_config = application_config(&config);
        let lock = DaemonLock::acquire(temp.path())?
            .ok_or_else(|| anyhow!("test process could not acquire daemon lock"))?;
        let started_at_ms = lock.started_at_ms().expect("lock start timestamp");
        let now_ms = utc_now().timestamp_millis();
        write_daemon_status(
            temp.path(),
            &json!({
                "status": "running",
                "pid": process::id(),
                "started_at_ms": started_at_ms,
                "heartbeat_at_ms": now_ms,
                "config_reload": {
                    "status": "applied",
                    "applied": {
                        "daemon_enabled": app_config.enabled,
                        "daemon_mode": app_config.mode.as_str(),
                        "semantic_enabled": app_config.semantic_enabled,
                        "semantic_executor": app_config.semantic_executor,
                        "semantic_contract_fingerprint": app_config.semantic_contract_fingerprint,
                        "semantic_builtin_throttling_configured": app_config.semantic_builtin_throttling_configured,
                        "semantic_builtin_throttling_effective": app_config.semantic_builtin_throttling_effective,
                    }
                }
            }),
        )?;

        let host = ReadyLiveOwnerHost {
            config: app_config.clone(),
            managed_executable: None,
        };
        let application = ctx_daemon_application::DaemonApplication::new(&host);

        // Missing supervisor.json -> enters reconciliation under PreserveLiveOwner
        // and still completes retrieval even if supervision returns an error.
        let mut observed_policies = Vec::new();
        let handoff = autostart_persistent_daemon_and_wait_with(
            &application,
            temp.path(),
            &config,
            DaemonTriggerCommandArg::Search,
            PersistentDaemonReadiness::Core,
            &mut || Ok(()),
            |_, _, policy| {
                observed_policies.push(policy);
                Err(anyhow!("simulated supervisor drift"))
            },
        )?;
        assert_eq!(handoff.handoff.pid, process::id());
        assert_eq!(
            observed_policies,
            vec![ctx_daemon_application::SupervisorMutationPolicy::PreserveLiveOwner]
        );

        // Steady-state healthy receipt -> zero-subprocess fast path (0 supervisor calls,
        // no supervisor-installation.lock created).
        let receipt_path = daemon_root_path(temp.path()).join("supervisor.json");
        write_private_json_file(
            &receipt_path,
            &json!({
                "schema_version": 1,
                "kind": "cli_self_heal",
                "status": "fallback",
            }),
        )?;
        let mut steady_calls = 0usize;
        let handoff = autostart_persistent_daemon_and_wait_with(
            &application,
            temp.path(),
            &config,
            DaemonTriggerCommandArg::Search,
            PersistentDaemonReadiness::Core,
            &mut || Ok(()),
            |_, _, _| {
                steady_calls += 1;
                Ok(super::super::super::daemon_supervisor::DaemonSupervisorStart::Fallback)
            },
        )?;
        assert_eq!(handoff.handoff.pid, process::id());
        assert_eq!(
            steady_calls, 0,
            "steady-state Core search must stay on the zero-subprocess fast path"
        );
        assert!(
            !daemon_root_path(temp.path())
                .join("supervisor-installation.lock")
                .exists(),
            "steady-state Core search must not acquire supervisor-installation.lock"
        );

        // Managed install with unhealthy/drifted receipt -> enters PreserveLiveOwner.
        let managed_host = ReadyLiveOwnerHost {
            config: app_config,
            managed_executable: Some(daemon_autostart_exe_path()?),
        };
        let managed_app = ctx_daemon_application::DaemonApplication::new(&managed_host);
        let mut drift_policies = Vec::new();
        let handoff = autostart_persistent_daemon_and_wait_with(
            &managed_app,
            temp.path(),
            &config,
            DaemonTriggerCommandArg::Search,
            PersistentDaemonReadiness::Core,
            &mut || Ok(()),
            |_, _, policy| {
                drift_policies.push(policy);
                Ok(super::super::super::daemon_supervisor::DaemonSupervisorStart::Fallback)
            },
        )?;
        assert_eq!(handoff.handoff.pid, process::id());
        assert_eq!(
            drift_policies,
            vec![ctx_daemon_application::SupervisorMutationPolicy::PreserveLiveOwner]
        );

        // Explicit Setup always reconciles with AllowReinstall and propagates failures.
        let mut setup_policies = Vec::new();
        let setup_error = autostart_persistent_daemon_and_wait_with(
            &managed_app,
            temp.path(),
            &config,
            DaemonTriggerCommandArg::Setup,
            PersistentDaemonReadiness::Full,
            &mut || Ok(()),
            |_, _, policy| {
                setup_policies.push(policy);
                Err(anyhow!("setup supervisor error"))
            },
        )
        .err()
        .expect("setup must propagate supervisor errors");
        assert_eq!(
            setup_policies,
            vec![ctx_daemon_application::SupervisorMutationPolicy::AllowReinstall]
        );
        assert!(
            format!("{setup_error:#}").contains("establish ctx daemon supervision"),
            "{setup_error:#}"
        );
        drop(lock);
        Ok(())
    }

    #[test]
    fn supervisor_mutation_policy_reserves_reinstall_for_setup_and_preserves_live_owner_for_retrieval(
    ) {
        assert_eq!(
            supervisor_mutation_policy_for_trigger(DaemonTriggerCommandArg::Setup),
            ctx_daemon_application::SupervisorMutationPolicy::AllowReinstall
        );
        for trigger in [
            DaemonTriggerCommandArg::Search,
            DaemonTriggerCommandArg::Import,
            DaemonTriggerCommandArg::Semantic,
        ] {
            assert_eq!(
                supervisor_mutation_policy_for_trigger(trigger),
                ctx_daemon_application::SupervisorMutationPolicy::PreserveLiveOwner
            );
        }
    }

    #[test]
    fn supervised_readiness_error_hints_avoid_suggesting_foreground_daemon_run() -> Result<()> {
        let temp = tempfile::tempdir()?;
        assert!(!native_supervisor_is_active(temp.path()));
        assert!(daemon_ready_failure_message(false).contains("ctx daemon run"));
        assert!(daemon_ready_failure_message(true).contains("ctx status --format json"));
        assert!(!daemon_ready_failure_message(true).contains("ctx daemon run"));
        assert!(!daemon_start_failure_message(true).contains("ctx daemon run"));

        write_private_json_file(
            &daemon_root_path(temp.path()).join("supervisor.json"),
            &json!({
                "schema_version": 1,
                "kind": "launch_agent",
                "status": "installed",
            }),
        )?;
        assert!(native_supervisor_is_active(temp.path()));

        write_private_json_file(
            &daemon_root_path(temp.path()).join("supervisor.json"),
            &json!({
                "schema_version": 1,
                "kind": "launch_agent",
                "status": "disabled",
            }),
        )?;
        assert!(!native_supervisor_is_active(temp.path()));

        write_private_json_file(
            &daemon_root_path(temp.path()).join("supervisor.json"),
            &json!({
                "schema_version": 1,
                "kind": "cli_self_heal",
                "status": "fallback",
            }),
        )?;
        assert!(!native_supervisor_is_active(temp.path()));
        Ok(())
    }
}

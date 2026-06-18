use super::*;
use crate::desktop_daemon::daemon_health_with_auth;
use ctx_desktop_ipc::DesktopRemoteDaemonUpdateState;

struct ConnectedRemoteDaemon {
    base_url: String,
    token: String,
    tunnel: TunnelHandle,
    runtime: SshRuntimeMetadata,
    platform: RemoteLinuxPlatform,
    pending_remote_update_on_idle: bool,
}

struct BootstrapPlanContext {
    target: SshConnectTarget,
    platform: RemoteLinuxPlatform,
    decision: RemoteBootstrapPlan,
}

enum InitialConnectOutcome {
    Connected(ConnectedRemoteDaemon),
    Planned(BootstrapPlanContext),
}

fn read_and_validate_remote_daemon_auth(
    target: &SshConnectTarget,
    tunnel: &mut TunnelHandle,
    base_url: &str,
    quick_probe: bool,
) -> Result<String> {
    let auth = read_remote_daemon_auth_with_retry(
        &target.host,
        target.user.as_deref(),
        target.remote_data_dir.as_deref(),
    )?;
    let auth_probe = if quick_probe {
        tunnel.probe_health_quick_for_bootstrap(base_url, Some(auth.token.as_str()))
    } else {
        tunnel.probe_health_with_retry(base_url, Some(auth.token.as_str()))
    };
    auth_probe.context("validating remote daemon auth")?;
    Ok(auth.token)
}

pub(super) fn cleanup_ephemeral_tunnel_on_error<T>(
    tunnel: TunnelHandle,
    err: anyhow::Error,
) -> Result<T> {
    let _ = tunnel.kill();
    Err(err)
}

fn set_job_phase(job_id: Option<&str>, phase: ConnectJobPhase) {
    if let Some(job_id) = job_id {
        record_connect_job_phase(job_id, phase);
    }
}

fn normalize_connect_target(req: SshConnectReq) -> Result<SshConnectTarget, String> {
    let host = req.host.trim().to_string();
    if host.is_empty() {
        return Err("host is required".to_string());
    }
    Ok(SshConnectTarget {
        host,
        user: normalize_optional_text(req.user.as_deref()),
        password_once: normalize_optional_text(req.password_once.as_deref()),
        remote_port: req.remote_port.unwrap_or(4399),
        start_remote: req.start_remote,
        remote_data_dir: normalize_optional_text(req.remote_data_dir.as_deref()),
    })
}

fn prepare_initial_connect(
    target: SshConnectTarget,
    job_id: Option<String>,
) -> Result<InitialConnectOutcome> {
    set_job_phase(job_id.as_deref(), ConnectJobPhase::Probing);
    let (platform, auth_bootstrap_used) = probe_remote_linux_platform_with_optional_password(
        &target.host,
        target.user.as_deref(),
        target.password_once.as_deref(),
    )?;
    let managed_binary_usable = remote_ctx_bin_usable_over_ssh(
        &target.host,
        target.user.as_deref(),
        MANAGED_REMOTE_CTX_BIN,
    )?;
    set_job_phase(job_id.as_deref(), ConnectJobPhase::OpeningTunnel);
    let local_port = pick_unused_local_port()?;
    let mut tunnel = TunnelHandle::start(
        &target.host,
        target.user.as_deref(),
        local_port,
        target.remote_port,
    )?;
    let base_url = tunnel.base_url();
    let no_start_remote = env_bool("CTX_DESKTOP_SSH_NO_START_REMOTE", false);
    let quick_probe = target.start_remote && !no_start_remote;
    let existing_daemon_reachable = if quick_probe {
        tunnel
            .probe_health_quick_for_bootstrap(&base_url, None)
            .is_ok()
    } else {
        tunnel.probe_health_with_retry(&base_url, None).is_ok()
    };
    let existing_daemon_auth = if existing_daemon_reachable {
        set_job_phase(job_id.as_deref(), ConnectJobPhase::ReadingAuth);
        match read_and_validate_remote_daemon_auth(&target, &mut tunnel, &base_url, quick_probe) {
            Ok(token) => Some(token),
            Err(err) => return cleanup_ephemeral_tunnel_on_error(tunnel, err),
        }
    } else {
        None
    };
    let probe = RemoteProbe {
        platform,
        auth_bootstrap_used,
        managed_binary_present: managed_binary_usable,
        existing_daemon_reachable: existing_daemon_auth.is_some(),
    };
    set_job_phase(job_id.as_deref(), ConnectJobPhase::Planning);
    let decision = plan_remote_bootstrap(RemoteBootstrapPlannerInput {
        start_remote: target.start_remote,
        no_start_remote,
        existing_daemon_reachable: probe.existing_daemon_reachable,
        managed_binary_present: probe.managed_binary_present,
    });
    match decision {
        RemoteBootstrapPlan::ConnectToRunningDaemon => {
            let token = existing_daemon_auth
                .ok_or_else(|| anyhow!("running remote daemon was not auth-validated"))?;
            let active_ctx_bin = probe
                .managed_binary_present
                .then(|| MANAGED_REMOTE_CTX_BIN.to_string());
            Ok(InitialConnectOutcome::Connected(ConnectedRemoteDaemon {
                base_url,
                token,
                tunnel,
                runtime: SshRuntimeMetadata {
                    managed_ctx_bin: MANAGED_REMOTE_CTX_BIN.to_string(),
                    active_ctx_bin,
                    ssh_password_once: target.password_once.clone(),
                    admin_password_once: None,
                },
                platform,
                pending_remote_update_on_idle: false,
            }))
        }
        RemoteBootstrapPlan::RefuseBecauseStartRemoteDisabled => {
            let _ = tunnel.kill();
            Err(anyhow!(
                "failed to reach remote daemon: remote start skipped (start_remote={}, no_start_remote={})",
                target.start_remote,
                no_start_remote
            ))
        }
        _ => {
            let _ = tunnel.kill();
            Ok(InitialConnectOutcome::Planned(BootstrapPlanContext {
                target,
                platform: probe.platform,
                decision,
            }))
        }
    }
}

fn execute_bootstrap_plan(
    app: tauri::AppHandle,
    plan: BootstrapPlanContext,
    channel: String,
    job_id: Option<String>,
) -> Result<ConnectedRemoteDaemon> {
    if matches!(
        plan.decision,
        RemoteBootstrapPlan::InstallManagedDaemonThenStart
    ) {
        set_job_phase(job_id.as_deref(), ConnectJobPhase::InstallingManagedDaemon);
        install_remote_daemon_over_ssh(
            &app,
            &plan.target.host,
            plan.target.user.as_deref(),
            plan.platform,
            MANAGED_REMOTE_CTX_BIN,
            &channel,
        )
        .map_err(|install_err| install_err.context(REMOTE_BOOTSTRAP_CAPABILITY_MSG))?;
    }

    set_job_phase(job_id.as_deref(), ConnectJobPhase::StartingRemoteDaemon);
    sync_remote_bundle_metadata_over_ssh(
        &app,
        &plan.target.host,
        plan.target.user.as_deref(),
        plan.target.remote_data_dir.as_deref(),
        plan.platform.arch,
        &channel,
    )?;
    let release_base_url = bootstrap_download_base_url();
    start_remote_daemon_over_ssh(
        &plan.target.host,
        plan.target.user.as_deref(),
        plan.target.remote_port,
        plan.target.remote_data_dir.as_deref(),
        MANAGED_REMOTE_CTX_BIN,
        Some(&channel),
        Some(&release_base_url),
    )?;

    set_job_phase(job_id.as_deref(), ConnectJobPhase::OpeningTunnel);
    let local_port = pick_unused_local_port()?;
    let mut tunnel = TunnelHandle::start(
        &plan.target.host,
        plan.target.user.as_deref(),
        local_port,
        plan.target.remote_port,
    )?;
    let base_url = tunnel.base_url();
    set_job_phase(job_id.as_deref(), ConnectJobPhase::ReadingAuth);
    let auth = match read_remote_daemon_auth_with_retry(
        &plan.target.host,
        plan.target.user.as_deref(),
        plan.target.remote_data_dir.as_deref(),
    ) {
        Ok(auth) => auth,
        Err(err) => return cleanup_ephemeral_tunnel_on_error(tunnel, err),
    };
    if let Err(err) = tunnel.probe_health_with_retry(&base_url, Some(auth.token.as_str())) {
        return cleanup_ephemeral_tunnel_on_error(tunnel, err);
    }
    Ok(ConnectedRemoteDaemon {
        base_url,
        token: auth.token,
        tunnel,
        runtime: SshRuntimeMetadata {
            managed_ctx_bin: MANAGED_REMOTE_CTX_BIN.to_string(),
            active_ctx_bin: Some(MANAGED_REMOTE_CTX_BIN.to_string()),
            ssh_password_once: plan.target.password_once.clone(),
            admin_password_once: None,
        },
        platform: plan.platform,
        pending_remote_update_on_idle: false,
    })
}

fn update_connected_remote_if_needed(
    app: &tauri::AppHandle,
    target: &SshConnectTarget,
    mut connected: ConnectedRemoteDaemon,
    channel: &str,
    expected_identity: &DesktopBuildIdentity,
) -> Result<ConnectedRemoteDaemon> {
    let health = daemon_health_with_auth(&connected.base_url, Some(connected.token.as_str()))
        .context("reading remote daemon health for compatibility classification")?;
    match classify_daemon_compatibility(&health, expected_identity) {
        DaemonCompatibilityState::Exact => Ok(connected),
        DaemonCompatibilityState::CompatibleMismatch => {
            if connected.runtime.active_ctx_bin.as_deref() != Some(MANAGED_REMOTE_CTX_BIN) {
                anyhow::bail!(
                    "remote daemon is compatible but was not started from the managed ctx binary; reconnect with remote start enabled to update it"
                );
            }
            let drained = begin_remote_update_drain(
                &connected.base_url,
                &connected.token,
                "desktop_connect",
            )?;
            if !drained {
                connected.pending_remote_update_on_idle = true;
                return Ok(connected);
            }
            let release_base_url = bootstrap_download_base_url();
            let update_result = run_remote_daemon_self_update(
                app,
                &target.host,
                target.user.as_deref(),
                target.remote_port,
                target.remote_data_dir.as_deref(),
                MANAGED_REMOTE_CTX_BIN,
                connected.platform.arch,
                channel,
                &connected.base_url,
                &connected.token,
                &release_base_url,
            );
            if let Err(err) = update_result {
                release_remote_update_drain(&connected.base_url, &connected.token);
                return Err(err);
            }
            let auth = read_remote_daemon_auth_with_retry(
                &target.host,
                target.user.as_deref(),
                target.remote_data_dir.as_deref(),
            )?;
            connected.token = auth.token;
            connected.pending_remote_update_on_idle = false;
            Ok(connected)
        }
        DaemonCompatibilityState::IncompatibleMismatch => {
            if connected.runtime.active_ctx_bin.as_deref() != Some(MANAGED_REMOTE_CTX_BIN) {
                anyhow::bail!(
                    "remote daemon is incompatible and was not started from the managed ctx binary; restart remote daemon with managed remote start enabled"
                );
            }
            let release_base_url = bootstrap_download_base_url();
            run_remote_daemon_self_update(
                app,
                &target.host,
                target.user.as_deref(),
                target.remote_port,
                target.remote_data_dir.as_deref(),
                MANAGED_REMOTE_CTX_BIN,
                connected.platform.arch,
                channel,
                &connected.base_url,
                &connected.token,
                &release_base_url,
            )?;
            let auth = read_remote_daemon_auth_with_retry(
                &target.host,
                target.user.as_deref(),
                target.remote_data_dir.as_deref(),
            )?;
            connected.token = auth.token;
            connected.pending_remote_update_on_idle = false;
            Ok(connected)
        }
    }
}

async fn desktop_connect_ssh_inner(
    app: tauri::AppHandle,
    scope: String,
    req: SshConnectReq,
    job_id: Option<String>,
) -> Result<DesktopConnectionInfo, String> {
    // Preserve any currently active transport until the SSH tunnel and remote daemon are fully
    // ready, then swap over with ConnectionManager::set_ssh_with_blocking_cleanup. Dropping the
    // connection to `none` up front invites background local auto-connect paths to race the SSH
    // bootstrap and can strand the wizard on Create against the wrong daemon.
    let target = normalize_connect_target(req)?;
    {
        let state = app.state::<ConnectionManager>();
        state.mark_explicit_remote_intent_for_scope(&scope);
    }
    let expected_identity = load_desktop_build_identity(&app)
        .map_err(|err| format!("failed to load desktop identity: {err:#}"))?;
    let channel = resolve_desktop_update_channel(&app, None)?;
    let prepared = tauri::async_runtime::spawn_blocking({
        let target = target.clone();
        let job_id = job_id.clone();
        move || prepare_initial_connect(target, job_id)
    })
    .await
    .map_err(|e| format!("failed to reach remote daemon: {e}"))?
    .map_err(|e| format!("failed to reach remote daemon: {e:#}"))?;

    let connected = match prepared {
        InitialConnectOutcome::Connected(connected) => tauri::async_runtime::spawn_blocking({
            let app = app.clone();
            let target = target.clone();
            let channel = channel.clone();
            let expected_identity = expected_identity.clone();
            move || {
                update_connected_remote_if_needed(
                    &app,
                    &target,
                    connected,
                    &channel,
                    &expected_identity,
                )
            }
        })
        .await
        .map_err(|e| format!("failed to update remote daemon: {e}"))?
        .map_err(|e| format!("failed to update remote daemon: {e:#}"))?,
        InitialConnectOutcome::Planned(plan) => {
            desktop_updater::ensure_desktop_app_current_for_remote_bootstrap(&app, &channel)
                .await?;
            let connected = tauri::async_runtime::spawn_blocking({
                let app = app.clone();
                let channel = channel.clone();
                let job_id = job_id.clone();
                move || execute_bootstrap_plan(app, plan, channel, job_id)
            })
            .await
            .map_err(|e| format!("failed to reach remote daemon: {e}"))?
            .map_err(|e| format!("failed to reach remote daemon: {e:#}"))?;
            tauri::async_runtime::spawn_blocking({
                let app = app.clone();
                let target = target.clone();
                let channel = channel.clone();
                let expected_identity = expected_identity.clone();
                move || {
                    update_connected_remote_if_needed(
                        &app,
                        &target,
                        connected,
                        &channel,
                        &expected_identity,
                    )
                }
            })
            .await
            .map_err(|e| format!("failed to update remote daemon: {e}"))?
            .map_err(|e| format!("failed to update remote daemon: {e:#}"))?
        }
    };

    set_job_phase(job_id.as_deref(), ConnectJobPhase::HandingOffConnection);
    let state = app.state::<ConnectionManager>();
    let pending_remote_update_on_idle = connected.pending_remote_update_on_idle;
    let pending_remote_update_key = remote_update_target_key(
        &target.host,
        target.user.as_deref(),
        target.remote_port,
        target.remote_data_dir.as_deref(),
    );
    let prewarm_host = target.host.clone();
    let prewarm_user = target.user.clone();
    let prewarm_remote_data_dir = target.remote_data_dir.clone();
    state
        .set_ssh_with_blocking_cleanup_for_scope(
            &scope,
            connected.base_url,
            Some(connected.token),
            connected
                .tunnel
                .into_connection_child()
                .map_err(|err| format!("failed to hand off ssh tunnel: {err:#}"))?,
            target.host,
            target.user,
            target.remote_port,
            target.remote_data_dir,
            connected.runtime,
        )
        .await?;
    if pending_remote_update_on_idle {
        state
            .set_ssh_remote_update_state_for_matching_target(
                &prewarm_host,
                prewarm_user.as_deref(),
                target.remote_port,
                prewarm_remote_data_dir.as_deref(),
                DesktopRemoteDaemonUpdateState::Pending,
                Some(
                    "Remote daemon update is queued and will restart automatically when no turns are queued or running."
                        .to_string(),
                ),
            )
            .map_err(|err| format!("failed to record pending remote daemon update: {err:#}"))?;
        schedule_pending_remote_daemon_update(
            &app,
            scope.clone(),
            pending_remote_update_key,
            channel.clone(),
        );
    } else {
        let _ = state.clear_ssh_remote_update_state_for_matching_target(
            &prewarm_host,
            prewarm_user.as_deref(),
            target.remote_port,
            prewarm_remote_data_dir.as_deref(),
        );
    }
    let _ = super::commands::schedule_remote_prewarm_request(
        app.clone(),
        scope.clone(),
        prewarm_host,
        prewarm_user,
        target.remote_port,
        prewarm_remote_data_dir,
    );
    Ok(state.info_for_scope(&scope))
}

#[tauri::command]
pub(crate) async fn desktop_connect_ssh(
    app: tauri::AppHandle,
    window: tauri::Window,
    req: SshConnectReq,
) -> Result<DesktopConnectionInfo, String> {
    desktop_connect_ssh_inner(app, window.label().to_string(), req, None).await
}

#[tauri::command]
pub(crate) fn desktop_connect_ssh_begin(
    app: tauri::AppHandle,
    window: tauri::Window,
    req: SshConnectReq,
) -> Result<String, String> {
    let _ = normalize_connect_target(req.clone())?;
    let scope = window.label().to_string();
    {
        let state = app.state::<ConnectionManager>();
        state.mark_explicit_remote_intent_for_scope(&scope);
    }
    let job_id = begin_connect_job()?;
    let app_for_job = app.clone();
    let job_id_for_task = job_id.clone();
    tauri::async_runtime::spawn(async move {
        let result =
            desktop_connect_ssh_inner(app_for_job, scope, req, Some(job_id_for_task.clone())).await;
        match result {
            Ok(info) => complete_connect_job_success(&job_id_for_task, info),
            Err(err) => complete_connect_job_failure(&job_id_for_task, err),
        }
    });
    Ok(job_id)
}

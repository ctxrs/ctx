use super::*;

struct LockedDaemonRoot {
    data_root: PathBuf,
    control: DaemonLifecycleControlLock,
}

/// Hosted uninstallers call this command before deleting the installed
/// executable. Each phase is idempotent so an interrupted uninstaller can
/// invoke it again safely.
pub fn prepare_daemon_uninstall(data_root: &Path) -> Result<Value> {
    let expected_executable =
        env::current_exe().context("resolve installed ctx executable before uninstall")?;
    let canonical_root =
        ctx_history_platform::managed_data_root().context("resolve canonical ctx data root")?;
    let mut roots = BTreeSet::from([data_root.to_path_buf(), canonical_root.clone()]);
    let lifecycle_controls = lock_discovered_installation_roots(&mut roots)?;
    disable_installation_roots(&roots)?;
    if cfg!(debug_assertions) && env::var_os(DAEMON_UNINSTALL_ABORT_AFTER_DISABLE_ENV).is_some() {
        process::exit(89);
    }

    crate::daemon_supervisor::disable_daemon_supervisor(&canonical_root)
        .context("remove canonical ctx daemon supervisor before uninstall")?;

    let installation_deadline = Instant::now() + DAEMON_INSTALLATION_QUIESCE_TIMEOUT;
    let installation_quiescence = loop {
        reject_undiscovered_installation_roots(&roots)?;
        quiesce_daemon_roots(&roots, &expected_executable)?;
        if let Some(quiescence) =
            crate::daemon_autostart::installation::try_acquire_installation_daemon_quiescence()?
        {
            break quiescence;
        }
        if Instant::now() >= installation_deadline {
            return Err(anyhow!(
                "timed out waiting for installation-wide ctx daemon quiescence; keep the ctx binary and retry `ctx daemon disable --prepare-uninstall`"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    };

    reject_undiscovered_installation_roots(&roots)?;
    for root in &roots {
        if daemon_lock_is_active(root) {
            return Err(anyhow!(
                "ctx daemon lifecycle ownership appeared after installation quiescence for {}; keep the ctx binary and retry `ctx daemon disable --prepare-uninstall`",
                root.display()
            ));
        }
    }
    crate::daemon_autostart::installation::remove_installation_daemon_coordination()
        .context("remove installation-wide ctx daemon coordination before uninstall")?;
    for locked in lifecycle_controls {
        remove_daemon_lifecycle_coordination(&locked.data_root)?;
        locked.control.remove_after_quiescence()?;
    }
    let coordination_state_removed = daemon_coordination_state_is_removed(&roots)?;
    if !coordination_state_removed {
        return Err(anyhow!(
            "ctx daemon coordination state remained after installation quiescence"
        ));
    }
    drop(installation_quiescence);
    let quiesced_roots = roots.into_iter().collect::<Vec<_>>();
    let quiesced_root_count = quiesced_roots.len();
    Ok(compact_json(json!({
        "schema_version": 1,
        "command": "daemon_prepare_uninstall",
        "ok": true,
        "scope": "installation",
        "requested_data_root": data_root,
        "canonical_data_root": canonical_root,
        "quiesced_roots": quiesced_roots,
        "quiesced_root_count": quiesced_root_count,
        "installation_quiescent": true,
        "daemon_enabled": false,
        "daemon_running": false,
        "owner_lock_released": true,
        "endpoint_released": true,
        "supervisor_removed": true,
        "coordination_state_removed": coordination_state_removed,
        "binary_retained": true,
        "retry_safe": true,
        "local_only": true,
    })))
}

fn lock_discovered_installation_roots(
    roots: &mut BTreeSet<PathBuf>,
) -> Result<Vec<LockedDaemonRoot>> {
    // The installer's uninstall fence is outermost. Acquire every per-root
    // control in lexical order before config, supervisor, daemon, or
    // transition work.
    let deadline = Instant::now() + DAEMON_INSTALLATION_QUIESCE_TIMEOUT;
    loop {
        roots
            .extend(crate::daemon_autostart::installation::registered_installation_daemon_roots()?);
        let locked = roots
            .iter()
            .map(|data_root| {
                DaemonLifecycleControlLock::acquire(data_root).map(|control| LockedDaemonRoot {
                    data_root: data_root.clone(),
                    control,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let discovered =
            crate::daemon_autostart::installation::registered_installation_daemon_roots()?;
        if discovered.iter().all(|root| roots.contains(root)) {
            return Ok(locked);
        }
        drop(locked);
        roots.extend(discovered);
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out stabilizing registered ctx daemon roots before uninstall"
            ));
        }
    }
}

fn reject_undiscovered_installation_roots(roots: &BTreeSet<PathBuf>) -> Result<()> {
    let undiscovered =
        crate::daemon_autostart::installation::registered_installation_daemon_roots()?
            .into_iter()
            .filter(|root| !roots.contains(root))
            .collect::<Vec<_>>();
    if undiscovered.is_empty() {
        return Ok(());
    }
    Err(anyhow!(
        "ctx daemon roots appeared after lifecycle-control acquisition: {}; keep the ctx binary and retry `ctx daemon disable --prepare-uninstall`",
        undiscovered
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn disable_installation_roots(roots: &BTreeSet<PathBuf>) -> Result<()> {
    for root in roots {
        crate::config::set_daemon_enabled(root, false).with_context(|| {
            format!(
                "durably set manual indexing for ctx root {} before uninstall",
                root.display()
            )
        })?;
    }
    Ok(())
}

fn daemon_coordination_state_is_removed(roots: &BTreeSet<PathBuf>) -> Result<bool> {
    let (_, registrations) = ctx_upgrade_engine::installation_daemon_coordination_paths()?;
    let registrations_removed = match fs::read_dir(&registrations) {
        Ok(mut entries) => entries.next().is_none(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspect ctx daemon installation coordination {}",
                    registrations.display()
                )
            })
        }
    };
    Ok(registrations_removed
        && roots.iter().all(|root| {
            !ctx_daemon_runtime::daemon_lifecycle_control_lock_path(root).exists()
                && !ctx_daemon_runtime::daemon_lifecycle_transition_lock_path(root).exists()
        }))
}

fn quiesce_daemon_roots(roots: &BTreeSet<PathBuf>, expected_executable: &Path) -> Result<()> {
    for root in roots {
        request_disabled_daemon_shutdown(root);
    }
    let cooperative_deadline = Instant::now() + DAEMON_UPGRADE_STOP_TIMEOUT;
    while roots.iter().any(|root| daemon_lock_is_active(root))
        && Instant::now() < cooperative_deadline
    {
        for root in roots {
            if daemon_lock_is_active(root) {
                request_disabled_daemon_shutdown(root);
            }
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    for root in roots {
        if daemon_lock_is_active(root) {
            terminate_identity_verified_residual_daemon(root, expected_executable).with_context(
                || {
                    format!(
                        "stop identity-verified residual ctx daemon for {} before uninstall",
                        root.display()
                    )
                },
            )?;
        }
        #[cfg(windows)]
        wait_for_released_residual_daemon(root, expected_executable).with_context(|| {
            format!(
                "wait for released residual ctx daemon for {} before uninstall",
                root.display()
            )
        })?;
        wait_for_daemon_lifecycle_release(root)?;
    }
    Ok(())
}

fn request_disabled_daemon_shutdown(data_root: &Path) {
    let _ = ctx_daemon_service::daemon_source_refresh_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "shutdown",
        })),
        DAEMON_HEALTH_TIMEOUT,
        DAEMON_HEALTH_RESPONSE_MAX_BYTES,
    );
}

pub(super) fn wait_for_daemon_lifecycle_release(data_root: &Path) -> Result<()> {
    let deadline = Instant::now() + DAEMON_UPGRADE_RESTART_TIMEOUT;
    while daemon_lock_is_active(data_root) {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "ctx daemon retained lifecycle ownership after verified termination"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    Ok(())
}

/// The caller must hold an external uninstall/quiescence fence that excludes
/// new transition participants for this data root.
fn remove_daemon_lifecycle_coordination(data_root: &Path) -> Result<()> {
    let lifecycle_transition = DaemonLifecycleTransitionLock::acquire(data_root)?;
    remove_daemon_restart_requests(data_root);
    let root = daemon_root_path(data_root);
    for path in [
        daemon_upgrade_handoff_path(data_root),
        daemon_query_endpoint_path(data_root),
        root.join("source-refresh-endpoint.json"),
        root.join("query.sock"),
        root.join("source-refresh.sock"),
        root.join("supervisor.json"),
        daemon_lock_path(data_root),
        pid_lock_guard_path(&daemon_lock_path(data_root)),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("remove daemon coordination {}", path.display()))
            }
        }
    }
    lifecycle_transition.remove_after_quiescence()
}

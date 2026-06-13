use super::*;
use crate::desktop_daemon::{
    daemon_data_dir, daemon_health_with_auth, existing_local_daemon_matches_with_auth,
    normalize_daemon_pid, probe_daemon_health_with_auth, probe_local_daemon_health_with_retry_auth,
    read_daemon_auth_with_retry, resolve_env_local_daemon, resolve_existing_local_daemon,
    spawn_and_validate_local_daemon, SpawnedLocalDaemonReady,
};
pub(super) use ctx_desktop_ipc::DesktopRestartLocalDaemonReq;

fn local_connect_mutex() -> &'static std::sync::Mutex<()> {
    static LOCAL_CONNECT_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> =
        std::sync::OnceLock::new();
    LOCAL_CONNECT_MUTEX.get_or_init(|| std::sync::Mutex::new(()))
}

const LOCAL_SPAWN_LOCK_RACE_RETRY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const LOCAL_SPAWN_LOCK_RACE_RETRY_DELAY: std::time::Duration =
    std::time::Duration::from_millis(200);

pub(super) fn lock_local_connect_gate() -> Result<std::sync::MutexGuard<'static, ()>> {
    local_connect_mutex()
        .lock()
        .map_err(|err| anyhow!("local connect mutex poisoned: {err}"))
}

#[tauri::command]
pub(super) async fn desktop_connect_local(
    app: tauri::AppHandle,
    window: tauri::Window,
) -> Result<DesktopConnectionInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = lock_local_connect_gate().map_err(to_err)?;
        let state = app.state::<ConnectionManager>();
        let scope = window.label().to_string();
        let data_dir = daemon_data_dir(&app).map_err(to_err)?;
        let desktop_identity = load_desktop_build_identity(&app).map_err(to_err)?;
        let result = connect_local_with_sources_for_scope(
            state.inner(),
            &scope,
            |url, auth_token| {
                existing_local_daemon_matches_with_auth(
                    url,
                    auth_token,
                    &data_dir,
                    &desktop_identity,
                )
                .unwrap_or(false)
            },
            || resolve_env_local_daemon(&app),
            probe_daemon_health_with_auth,
            || resolve_existing_local_daemon(&app, &data_dir),
            || spawn_and_validate_local_daemon(&app, &data_dir, &desktop_identity),
        );
        if let Err(err) = &result {
            log_desktop_startup_error(&format!(
                "desktop_startup: daemon_connect_failed kind=local error={}",
                serde_json::to_string(&err.to_string())
                    .unwrap_or_else(|_| "\"unknown\"".to_string()),
            ));
        }
        result.map_err(to_err)
    })
    .await
    .map_err(|e| format!("failed to connect to daemon: {e}"))?
}

#[cfg(test)]
pub(super) fn connect_local_with_sources<
    CurrentLocalMatchesFn,
    ResolveEnvFn,
    ProbeHealthFn,
    ResolveExistingFn,
    SpawnFn,
>(
    state: &ConnectionManager,
    current_local_matches_or_absent: CurrentLocalMatchesFn,
    resolve_env_local_daemon: ResolveEnvFn,
    probe_health: ProbeHealthFn,
    resolve_existing_local_daemon: ResolveExistingFn,
    spawn_and_validate_local_daemon: SpawnFn,
) -> Result<DesktopConnectionInfo>
where
    CurrentLocalMatchesFn: Fn(&str, Option<&str>) -> bool,
    ResolveEnvFn: FnOnce() -> Result<Option<(String, String)>>,
    ProbeHealthFn: Fn(&str, Option<&str>) -> Result<()>,
    ResolveExistingFn: FnMut() -> Result<Option<(String, String, Option<u32>)>>,
    SpawnFn: FnOnce() -> Result<SpawnedLocalDaemonReady>,
{
    connect_local_with_sources_for_scope(
        state,
        DEFAULT_CONNECTION_SCOPE,
        current_local_matches_or_absent,
        resolve_env_local_daemon,
        probe_health,
        resolve_existing_local_daemon,
        spawn_and_validate_local_daemon,
    )
}

pub(super) fn connect_local_with_sources_for_scope<
    CurrentLocalMatchesFn,
    ResolveEnvFn,
    ProbeHealthFn,
    ResolveExistingFn,
    SpawnFn,
>(
    state: &ConnectionManager,
    scope: &str,
    current_local_matches_or_absent: CurrentLocalMatchesFn,
    resolve_env_local_daemon: ResolveEnvFn,
    probe_health: ProbeHealthFn,
    mut resolve_existing_local_daemon: ResolveExistingFn,
    spawn_and_validate_local_daemon: SpawnFn,
) -> Result<DesktopConnectionInfo>
where
    CurrentLocalMatchesFn: Fn(&str, Option<&str>) -> bool,
    ResolveEnvFn: FnOnce() -> Result<Option<(String, String)>>,
    ProbeHealthFn: Fn(&str, Option<&str>) -> Result<()>,
    ResolveExistingFn: FnMut() -> Result<Option<(String, String, Option<u32>)>>,
    SpawnFn: FnOnce() -> Result<SpawnedLocalDaemonReady>,
{
    // Idempotent: if we're already connected to a healthy local daemon, keep the connection.
    // The workspace wizard calls connect_local as part of its flow; disconnecting here can
    // kill a just-started daemon and introduce flakiness on cold start.
    let info = state.info_for_scope(scope);
    if matches!(info.kind, DesktopConnectionKind::Local) {
        if let Some(url) = info.base_url.as_deref() {
            if current_local_matches_or_absent(url, info.token.as_deref()) {
                state.mark_explicit_local_intent_if_local_for_scope(scope);
                return Ok(state.info_for_scope(scope));
            }
        }
    }

    // Keep any currently healthy connection active until a replacement has been validated.
    // ConnectionManager swaps and cleans up the old transport only after the new one is ready.
    if let Some((url, token)) = resolve_env_local_daemon()? {
        probe_health(&url, Some(token.as_str()))?;
        state.set_local_attached_for_scope(
            scope,
            url,
            token,
            None,
            LocalConnectionSource::EnvOverride,
        );
        return Ok(state.info_for_scope(scope));
    }
    if let Some((url, token, daemon_pid)) = resolve_existing_local_daemon()? {
        state.set_local_attached_for_scope(
            scope,
            url,
            token,
            daemon_pid,
            LocalConnectionSource::ExistingCompatibleDaemon,
        );
        return Ok(state.info_for_scope(scope));
    }

    // Block until the daemon is actually reachable before returning. The workspace wizard
    // applies the connection and navigates immediately after `desktop_connect_local` resolves;
    // returning early causes the workbench to briefly render a "daemon unavailable" overlay.
    let spawned = spawn_and_validate_local_daemon();
    let fallback_existing = match &spawned {
        Ok(_) => None,
        Err(err) => {
            resolve_existing_local_after_spawn_failure(&mut resolve_existing_local_daemon, err)
        }
    };
    apply_validated_local_connection_for_scope(state, scope, spawned, fallback_existing)
}

fn spawn_failure_may_be_local_lock_race(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}");
    text.contains("daemon already running")
        || text.contains("ctx daemon already running")
        || text.contains("daemon.lock")
        || text.contains("lockfile")
}

fn resolve_existing_local_after_spawn_failure<ResolveExistingFn>(
    resolve_existing_local_daemon: &mut ResolveExistingFn,
    spawn_err: &anyhow::Error,
) -> Option<(String, String, Option<u32>)>
where
    ResolveExistingFn: FnMut() -> Result<Option<(String, String, Option<u32>)>>,
{
    if let Some(existing) = resolve_existing_local_daemon().ok().flatten() {
        return Some(existing);
    }
    if !spawn_failure_may_be_local_lock_race(spawn_err) {
        return None;
    }

    let deadline = std::time::Instant::now() + LOCAL_SPAWN_LOCK_RACE_RETRY_TIMEOUT;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(LOCAL_SPAWN_LOCK_RACE_RETRY_DELAY);
        if let Some(existing) = resolve_existing_local_daemon().ok().flatten() {
            return Some(existing);
        }
    }
    None
}

#[cfg(test)]
pub(super) fn apply_validated_local_connection(
    state: &ConnectionManager,
    spawned: Result<SpawnedLocalDaemonReady>,
    fallback_existing: Option<(String, String, Option<u32>)>,
) -> Result<DesktopConnectionInfo> {
    apply_validated_local_connection_for_scope(
        state,
        DEFAULT_CONNECTION_SCOPE,
        spawned,
        fallback_existing,
    )
}

pub(super) fn apply_validated_local_connection_for_scope(
    state: &ConnectionManager,
    scope: &str,
    spawned: Result<SpawnedLocalDaemonReady>,
    fallback_existing: Option<(String, String, Option<u32>)>,
) -> Result<DesktopConnectionInfo> {
    match spawned {
        Ok(spawned) => {
            state.set_local_for_scope_with_shutdown_token(
                scope,
                spawned.url,
                spawned.token,
                Some(spawned.local_shutdown_token),
                spawned.child,
                spawned.systemd_scope,
            );
            Ok(state.info_for_scope(scope))
        }
        Err(err) => {
            if let Some((url, token, daemon_pid)) = fallback_existing {
                state.set_local_attached_for_scope(
                    scope,
                    url,
                    token,
                    daemon_pid,
                    LocalConnectionSource::ExistingCompatibleDaemon,
                );
                return Ok(state.info_for_scope(scope));
            }
            Err(err)
        }
    }
}

pub(super) fn restart_local_with_spawn<SpawnFn>(
    state: &ConnectionManager,
    spawn_and_validate_local_daemon: SpawnFn,
) -> Result<DesktopConnectionInfo>
where
    SpawnFn: FnOnce() -> Result<SpawnedLocalDaemonReady>,
{
    restart_local_with_spawn_for_scope(
        DEFAULT_CONNECTION_SCOPE,
        state,
        spawn_and_validate_local_daemon,
    )
}

pub(super) fn restart_local_with_spawn_for_scope<SpawnFn>(
    scope: &str,
    state: &ConnectionManager,
    spawn_and_validate_local_daemon: SpawnFn,
) -> Result<DesktopConnectionInfo>
where
    SpawnFn: FnOnce() -> Result<SpawnedLocalDaemonReady>,
{
    let _guard = lock_local_connect_gate()?;
    state.disconnect_owned_local_daemons_for_restart()?;
    let spawned = spawn_and_validate_local_daemon()?;
    state.set_local_for_scope_with_shutdown_token(
        scope,
        spawned.url,
        spawned.token,
        Some(spawned.local_shutdown_token),
        spawned.child,
        spawned.systemd_scope,
    );
    Ok(state.info_for_scope(scope))
}

#[derive(Debug, Clone, Copy)]
enum EnsureLocalConnectionMode {
    AutoBootstrap,
    ExplicitLocal,
}

fn ensure_mode_allows_connect(
    mode: EnsureLocalConnectionMode,
    state: &ConnectionManager,
    scope: &str,
) -> bool {
    match mode {
        EnsureLocalConnectionMode::AutoBootstrap => {
            state.local_auto_bootstrap_allowed_for_scope(scope)
        }
        EnsureLocalConnectionMode::ExplicitLocal => true,
    }
}

fn ensure_mode_needs_connect(
    mode: EnsureLocalConnectionMode,
    info: &DesktopConnectionInfo,
) -> bool {
    match mode {
        EnsureLocalConnectionMode::AutoBootstrap => {
            matches!(info.kind, DesktopConnectionKind::None)
        }
        EnsureLocalConnectionMode::ExplicitLocal => {
            !matches!(info.kind, DesktopConnectionKind::Local)
        }
    }
}

fn current_local_connection_stale<ProbeHealthFn>(
    info: &DesktopConnectionInfo,
    probe_health: ProbeHealthFn,
) -> bool
where
    ProbeHealthFn: Fn(&str, Option<&str>) -> Result<()>,
{
    if !matches!(info.kind, DesktopConnectionKind::Local) {
        return false;
    }
    let Some(url) = info.base_url.as_deref() else {
        return true;
    };
    probe_health(url, info.token.as_deref()).is_err()
}

fn set_attached_local_for_ensure_mode(
    state: &ConnectionManager,
    scope: &str,
    mode: EnsureLocalConnectionMode,
    url: String,
    token: String,
    daemon_pid: Option<u32>,
    source: LocalConnectionSource,
) {
    match mode {
        EnsureLocalConnectionMode::AutoBootstrap => {
            state
                .set_local_attached_auto_bootstrap_for_scope(scope, url, token, daemon_pid, source);
        }
        EnsureLocalConnectionMode::ExplicitLocal => {
            state.set_local_attached_for_scope(scope, url, token, daemon_pid, source);
        }
    }
}

fn set_spawned_local_for_ensure_mode(
    state: &ConnectionManager,
    scope: &str,
    mode: EnsureLocalConnectionMode,
    spawned: SpawnedLocalDaemonReady,
) {
    match mode {
        EnsureLocalConnectionMode::AutoBootstrap => {
            state.set_local_auto_bootstrap_for_scope_with_shutdown_token(
                scope,
                spawned.url,
                spawned.token,
                Some(spawned.local_shutdown_token),
                spawned.child,
                spawned.systemd_scope,
            );
        }
        EnsureLocalConnectionMode::ExplicitLocal => {
            state.set_local_for_scope_with_shutdown_token(
                scope,
                spawned.url,
                spawned.token,
                Some(spawned.local_shutdown_token),
                spawned.child,
                spawned.systemd_scope,
            );
        }
    }
}

#[tauri::command]
pub(super) async fn desktop_restart_local_daemon(
    app: tauri::AppHandle,
    window: tauri::Window,
    req: DesktopRestartLocalDaemonReq,
) -> Result<DesktopConnectionInfo, String> {
    if !req.confirm {
        return Err("confirm required".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = lock_local_connect_gate().map_err(to_err)?;
        let state = app.state::<ConnectionManager>();
        let manager: &ConnectionManager = state.inner();
        let scope = window.label().to_string();
        let data_dir = daemon_data_dir(&app).map_err(to_err)?;
        let desktop_identity = load_desktop_build_identity(&app).map_err(to_err)?;
        restart_local_with_spawn_for_scope(&scope, manager, || {
            spawn_and_validate_local_daemon(&app, &data_dir, &desktop_identity)
        })
        .map_err(to_err)
    })
    .await
    .map_err(|e| format!("failed to restart local daemon: {e}"))?
}

pub(super) fn ensure_local_connection(
    app: &tauri::AppHandle,
    state: &ConnectionManager,
) -> Result<()> {
    ensure_local_connection_for_scope(app, state, DEFAULT_CONNECTION_SCOPE)
}

pub(super) fn ensure_local_connection_for_scope(
    app: &tauri::AppHandle,
    state: &ConnectionManager,
    scope: &str,
) -> Result<()> {
    ensure_local_connection_with_mode(app, state, scope, EnsureLocalConnectionMode::AutoBootstrap)
}

pub(super) fn ensure_local_connection_for_user_action(
    app: &tauri::AppHandle,
    state: &ConnectionManager,
) -> Result<()> {
    ensure_local_connection_for_user_action_for_scope(app, state, DEFAULT_CONNECTION_SCOPE)
}

pub(super) fn ensure_local_connection_for_user_action_for_scope(
    app: &tauri::AppHandle,
    state: &ConnectionManager,
    scope: &str,
) -> Result<()> {
    ensure_local_connection_with_mode(app, state, scope, EnsureLocalConnectionMode::ExplicitLocal)
}

fn ensure_local_connection_with_mode(
    app: &tauri::AppHandle,
    state: &ConnectionManager,
    scope: &str,
    mode: EnsureLocalConnectionMode,
) -> Result<()> {
    let result = (|| -> Result<()> {
        let initial_info = state.info_for_scope(scope);
        if !ensure_mode_needs_connect(mode, &initial_info)
            && !current_local_connection_stale(&initial_info, probe_daemon_health_with_auth)
        {
            return Ok(());
        }
        if !ensure_mode_allows_connect(mode, state, scope) {
            return Ok(());
        }
        // Multiple webview requests can race on cold start (overlay pollers, initial data loads, etc.).
        // Serialize the "connect local" path so we don't concurrently spawn the daemon and trip the
        // daemon's lockfile, which can surface as spurious "daemon unavailable" errors in the UI.
        let _guard = lock_local_connect_gate()?;
        let current_info = state.info_for_scope(scope);
        if !ensure_mode_needs_connect(mode, &current_info)
            && !current_local_connection_stale(&current_info, probe_daemon_health_with_auth)
        {
            return Ok(());
        }
        if !ensure_mode_allows_connect(mode, state, scope) {
            return Ok(());
        }
        let data_dir = daemon_data_dir(app)?;
        let desktop_identity = load_desktop_build_identity(app)?;
        if let Some((url, token)) = resolve_env_local_daemon(app)? {
            probe_daemon_health_with_auth(&url, Some(token.as_str()))?;
            set_attached_local_for_ensure_mode(
                state,
                scope,
                mode,
                url,
                token,
                None,
                LocalConnectionSource::EnvOverride,
            );
            return Ok(());
        }
        if let Some((url, token, daemon_pid)) = resolve_existing_local_daemon(app, &data_dir)? {
            set_attached_local_for_ensure_mode(
                state,
                scope,
                mode,
                url,
                token,
                daemon_pid,
                LocalConnectionSource::ExistingCompatibleDaemon,
            );
            return Ok(());
        }
        let spawned = match spawn_and_validate_local_daemon(app, &data_dir, &desktop_identity) {
            Ok(value) => value,
            Err(err) => {
                // This can happen if another thread already started the daemon but we raced before
                // the auth file became visible or health was reachable. Retry by waiting for the auth
                // file + health and then attaching as an external local connection.
                let auth = read_daemon_auth_with_retry(&data_dir)
                    .with_context(|| format!("spawning local daemon failed: {err:#}"))?;
                let Some(url) = auth.daemon_url.as_deref() else {
                    return Err(err)
                        .context("spawning local daemon failed (auth file missing daemon_url)");
                };
                probe_local_daemon_health_with_retry_auth(url, Some(auth.token.as_str()))?;
                let compatible = existing_local_daemon_matches_with_auth(
                    url,
                    Some(auth.token.as_str()),
                    &data_dir,
                    &desktop_identity,
                )
                .with_context(|| {
                    format!(
                        "spawning local daemon failed: {err:#}; validating existing local daemon compatibility"
                    )
                })?;
                if !compatible {
                    return Err(err).context(format!(
                        "spawning local daemon failed and existing daemon is incompatible (url={url})"
                    ));
                }
                let daemon_pid = daemon_health_with_auth(url, Some(auth.token.as_str()))
                    .ok()
                    .and_then(|health| normalize_daemon_pid(health.pid));
                set_attached_local_for_ensure_mode(
                    state,
                    scope,
                    mode,
                    url.to_string(),
                    auth.token,
                    daemon_pid,
                    LocalConnectionSource::ExistingCompatibleDaemon,
                );
                return Ok(());
            }
        };
        set_spawned_local_for_ensure_mode(state, scope, mode, spawned);
        Ok(())
    })();
    if let Err(err) = &result {
        log_desktop_startup_error(&format!(
            "desktop_startup: daemon_connect_failed kind=local error={}",
            serde_json::to_string(&err.to_string()).unwrap_or_else(|_| "\"unknown\"".to_string()),
        ));
    }
    result
}

#[cfg(test)]
#[path = "desktop_local_daemon_tests.rs"]
mod desktop_local_daemon_tests;

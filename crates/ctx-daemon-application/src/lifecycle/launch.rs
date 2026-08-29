use super::*;
use crate::{SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV, SEMANTIC_EMBEDDING_TOKEN_ENV};

pub fn daemon_autostart_command(
    exe: &Path,
    data_root: &Path,
    trigger: DaemonTrigger,
    loop_interval: Option<u64>,
    handoff_token: Option<&str>,
) -> io::Result<NormalizedLaunch> {
    daemon_autostart_command_with_environment_overrides(
        exe,
        data_root,
        trigger,
        loop_interval,
        handoff_token,
        BTreeMap::new(),
        DaemonLaunchProfile::Persistent,
    )
}

fn daemon_autostart_command_with_environment_overrides(
    exe: &Path,
    data_root: &Path,
    trigger: DaemonTrigger,
    loop_interval: Option<u64>,
    handoff_token: Option<&str>,
    overrides: BTreeMap<OsString, OsString>,
    profile: DaemonLaunchProfile,
) -> io::Result<NormalizedLaunch> {
    let mut args = vec![
        OsString::from("--data-root"),
        data_root.as_os_str().to_os_string(),
        OsString::from("daemon"),
        OsString::from("run"),
        OsString::from("--start-mode"),
        OsString::from(DaemonStartMode::Auto.as_str()),
        OsString::from("--trigger-command"),
        OsString::from(trigger.as_str()),
        OsString::from("--format=json"),
    ];
    if profile == DaemonLaunchProfile::FiniteCoreWorker {
        args.push(OsString::from("--finite-core-worker"));
        args.push(OsString::from("--force"));
    }
    if let Some(loop_interval) = loop_interval {
        args.push(OsString::from("--loop-interval-seconds"));
        args.push(OsString::from(loop_interval.to_string()));
    }
    let mut environment = daemon_child_environment();
    environment.insert(
        OsString::from(DAEMON_BACKGROUND_CHILD_ENV),
        OsString::from("1"),
    );
    if let Some(token) = handoff_token {
        environment.insert(
            OsString::from(DAEMON_UPGRADE_HANDOFF_TOKEN_ENV),
            OsString::from(token),
        );
    }
    for (name, value) in overrides {
        environment.insert(name, value);
    }
    validate_daemon_launch_environment(&environment)?;
    Ok(NormalizedLaunch::new(exe.to_path_buf(), args, environment))
}

#[cfg(test)]
pub(super) fn normalized_daemon_launch_for_test(
    program: PathBuf,
    args: Vec<OsString>,
    overrides: BTreeMap<OsString, OsString>,
) -> io::Result<NormalizedLaunch> {
    let mut environment = daemon_child_environment();
    environment.extend(overrides);
    validate_daemon_launch_environment(&environment)?;
    Ok(NormalizedLaunch::new(program, args, environment))
}

const DAEMON_CHILD_ENV_ALLOWLIST: &[&str] = &[
    "APPDATA",
    "ASTRBOT_ROOT",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "COPILOT_HOME",
    "CTX_ANALYTICS_DEBUG",
    "CTX_ANALYTICS_DRY_RUN",
    "CTX_ANALYTICS_ENABLED",
    "CTX_ANALYTICS_ENDPOINT",
    "CTX_HISTORY_PLUGIN_PATH",
    "CTX_LOCAL_USAGE_ENABLED",
    "CTX_MACHINE_ID",
    "CTX_PRO_HELPER",
    "CTX_RUNTIME_DIR",
    "CTX_SEARCH_SEMANTIC",
    "CTX_SEMANTIC_COREML_NATIVE_COMPUTE",
    SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV,
    SEMANTIC_EMBEDDING_TOKEN_ENV,
    "CTX_SEMANTIC_MODEL_ONNX",
    "CTX_UPGRADE_AUTO",
    "CTX_UPGRADE_CHANNEL",
    "CTX_UPGRADE_INTERVAL_SECONDS",
    "DBUS_SESSION_BUS_ADDRESS",
    "DSH_HOME",
    "FORGE_CONFIG",
    "GROK_HOME",
    "HERMES_HOME",
    "HOME",
    "HOMEDRIVE",
    "HOMEPATH",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "KILO_DB",
    "LANG",
    "LC_ALL",
    "LOCALAPPDATA",
    "MIMOCODE_CONFIG_DIR",
    "MIMOCODE_DB",
    "MIMOCODE_DISABLE_CHANNEL_DB",
    "MIMOCODE_HOME",
    "NO_PROXY",
    "OPENCLAW_STATE_DIR",
    "PATH",
    "PI_CODING_AGENT_SESSION_DIR",
    "SHELLEY_DB",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "SystemRoot",
    "TEMP",
    "TMP",
    "TMPDIR",
    "TZ",
    "USER",
    "USERPROFILE",
    "VIBE_HOME",
    "WINDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_STATE_HOME",
    "https_proxy",
    "http_proxy",
    "no_proxy",
];
fn daemon_child_environment() -> BTreeMap<OsString, OsString> {
    let mut environment = DAEMON_CHILD_ENV_ALLOWLIST
        .iter()
        .filter_map(|name| env::var_os(name).map(|value| (OsString::from(name), value)))
        .collect::<BTreeMap<_, _>>();
    let token = OsString::from(SEMANTIC_EMBEDDING_TOKEN_ENV);
    let endpoint = OsString::from(SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV);
    if !environment.contains_key(&token) || !environment.contains_key(&endpoint) {
        environment.remove(&token);
        environment.remove(&endpoint);
    }
    environment
}

fn validate_daemon_launch_environment(
    environment: &BTreeMap<OsString, OsString>,
) -> io::Result<()> {
    if let Some(name) = environment
        .keys()
        .find(|name| is_release_authority_environment_name(name.as_os_str()))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "detached daemon environment may not contain release authority variable {}",
                name.to_string_lossy()
            ),
        ));
    }
    Ok(())
}

pub fn spawn_daemon_child(
    host: &dyn DaemonApplicationHost,
    launch: NormalizedLaunch,
) -> io::Result<Child> {
    if hosted_uninstall_fences_daemon_autostart(host) {
        return Err(hosted_uninstall_daemon_fence_error());
    }
    spawn_detached_daemon_child(launch)
}

pub fn spawn_daemon_child_for_upgrade_handoff(
    host: &dyn DaemonApplicationHost,
    launch: NormalizedLaunch,
    replacement_executable: &Path,
) -> io::Result<Child> {
    if host
        .hosted_uninstall_active_for_executable(replacement_executable)
        .unwrap_or(true)
    {
        return Err(hosted_uninstall_daemon_fence_error());
    }
    spawn_detached_daemon_child(launch)
}

fn hosted_uninstall_daemon_fence_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "ctx daemon autostart is fenced by hosted uninstall",
    )
}

pub fn spawn_detached_daemon_child(launch: NormalizedLaunch) -> io::Result<Child> {
    spawn_detached(launch)
}

fn is_release_authority_environment_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_uppercase();
    name.starts_with("CTX_RELEASE_") || name == "CTX_ALLOW_CUSTOM_RELEASE_BASE_URL"
}

pub fn configured_daemon_autostart_command(
    exe: &Path,
    data_root: &Path,
    trigger: DaemonTrigger,
    handoff_token: Option<&str>,
) -> io::Result<NormalizedLaunch> {
    let mut overrides = BTreeMap::new();
    if let Some(mode) = env::var_os(DAEMON_MODE_ENV) {
        overrides.insert(OsString::from(DAEMON_MODE_ENV), mode);
    }
    let loop_interval_seconds =
        crate::supervisor::persisted_supervisor_loop_interval_seconds(data_root).or_else(|| {
            daemon_autostart_u64_env("CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS", 3_600)
        });
    daemon_autostart_command_with_environment_overrides(
        exe,
        data_root,
        trigger,
        loop_interval_seconds,
        handoff_token,
        overrides,
        DaemonLaunchProfile::Persistent,
    )
}

pub(super) fn configured_finite_core_worker_command(
    exe: &Path,
    data_root: &Path,
    trigger: DaemonTrigger,
) -> io::Result<NormalizedLaunch> {
    daemon_autostart_command_with_environment_overrides(
        exe,
        data_root,
        trigger,
        None,
        None,
        BTreeMap::new(),
        DaemonLaunchProfile::FiniteCoreWorker,
    )
}

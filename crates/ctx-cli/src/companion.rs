use std::{
    ffi::{OsStr, OsString},
    io::Write as _,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::OnceLock,
    time::Duration,
};

use ctx_companion_bridge::{
    BridgeError, BridgeLimits, CancellationToken, CliRequest, CompanionBridge,
    CompanionEnvironment, EnvironmentKey, ExitClass, InstalledCompanion, LimitConfiguration,
    MaintenanceRequest, ManagedPairExpectations, McpRequest, ProtocolVersion, ReleaseChannel,
    TerminationReason, MAX_ADMISSION_WAIT, MAX_ARGUMENTS, MAX_CAPTURED_WALL_TIME,
    MAX_CONCURRENT_PROCESSES, MAX_CONTROL_BYTES, MAX_ENVIRONMENT_ENTRIES, MAX_STDERR_BYTES,
};
use serde_json::{json, Value};

const MCP_PROXY_MAX_BYTES: usize = 1024 * 1024;
const PRO_PATH_OVERRIDE_ENVIRONMENT: &str = "CTX_PRO_PATH";
const SUPERVISOR_ENV_NAMES: &str = "CTX_CORE_SUPERVISOR_ENVIRONMENT_NAMES_V1";
const FORWARDED_ENVIRONMENT: [(EnvironmentKey, &str); 9] = [
    (EnvironmentKey::Home, "HOME"),
    (EnvironmentKey::Path, "PATH"),
    (EnvironmentKey::Lang, "LANG"),
    (EnvironmentKey::LcAll, "LC_ALL"),
    (EnvironmentKey::TimeZone, "TZ"),
    (
        EnvironmentKey::DbusSessionBusAddress,
        "DBUS_SESSION_BUS_ADDRESS",
    ),
    (EnvironmentKey::XdgRuntimeDir, "XDG_RUNTIME_DIR"),
    (EnvironmentKey::LocalUsageEnabled, "CTX_LOCAL_USAGE_ENABLED"),
    (
        EnvironmentKey::HostedInstallerSetup,
        "CTX_HOSTED_INSTALLER_SETUP",
    ),
];
const FORWARDED_TERMINAL_ENVIRONMENT: [(EnvironmentKey, &str); 6] = [
    (EnvironmentKey::Term, "TERM"),
    (EnvironmentKey::ColorTerm, "COLORTERM"),
    (EnvironmentKey::NoColor, "NO_COLOR"),
    (EnvironmentKey::CliColor, "CLICOLOR"),
    (EnvironmentKey::CliColorForce, "CLICOLOR_FORCE"),
    (EnvironmentKey::Ci, "CI"),
];
static COMPANION_CANCELLATION: OnceLock<Result<CancellationToken, ()>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompanionRouteError {
    Unavailable,
    Incompatible,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CompanionLaunchError {
    MissingExecutable {
        path: PathBuf,
    },
    ProtocolMismatch {
        expected: ProtocolVersion,
        observed: ProtocolVersion,
    },
    LaunchFailed {
        stderr: Vec<u8>,
        stderr_truncated: bool,
    },
    InvalidLaunch {
        reason: &'static str,
    },
    Unavailable,
}

impl CompanionRouteError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "companion_unavailable",
            Self::Incompatible => "companion_incompatible",
        }
    }
}

impl CompanionLaunchError {
    const fn code(&self) -> &'static str {
        match self {
            Self::MissingExecutable { .. } => "companion_missing_executable",
            Self::ProtocolMismatch { .. } => "companion_protocol_mismatch",
            Self::LaunchFailed { .. } => "companion_unavailable",
            Self::InvalidLaunch { .. } => "companion_launch_invalid",
            Self::Unavailable => "companion_unavailable",
        }
    }

    const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::MissingExecutable { .. } | Self::LaunchFailed { .. } | Self::Unavailable
        )
    }
}

impl From<CompanionLaunchError> for CompanionRouteError {
    fn from(error: CompanionLaunchError) -> Self {
        match error {
            CompanionLaunchError::MissingExecutable { .. }
            | CompanionLaunchError::LaunchFailed { .. }
            | CompanionLaunchError::Unavailable => Self::Unavailable,
            CompanionLaunchError::ProtocolMismatch { .. }
            | CompanionLaunchError::InvalidLaunch { .. } => Self::Incompatible,
        }
    }
}

impl From<CompanionRouteError> for CompanionLaunchError {
    fn from(error: CompanionRouteError) -> Self {
        match error {
            CompanionRouteError::Unavailable => Self::Unavailable,
            CompanionRouteError::Incompatible => Self::InvalidLaunch {
                reason: "invalid Core-to-Pro launch boundary",
            },
        }
    }
}

pub(crate) fn forward_paid_cli_if_selected(arguments: Vec<OsString>) -> Option<ExitCode> {
    let forwarded = paid_family_arguments(&arguments)?;
    let result = forward_paid_cli(forwarded);
    Some(match result {
        Ok(exit) => exit,
        Err(error) => {
            write_cli_launch_error(&error);
            ExitCode::FAILURE
        }
    })
}

pub(crate) fn proxy_paid_mcp(
    request_line: &[u8],
    data_root: &Path,
) -> Result<Vec<u8>, CompanionRouteError> {
    if request_line.len() > MCP_PROXY_MAX_BYTES {
        return Err(CompanionRouteError::Unavailable);
    }
    let companion = installed_companion().map_err(CompanionRouteError::from)?;
    let mut request = McpRequest::new(request_line);
    forward_mcp_environment(request.environment_mut(), data_root);
    let output = CompanionBridge::new(mcp_limits()?)
        .launch_mcp(
            &companion,
            request,
            companion_cancellation().map_err(CompanionRouteError::from)?,
        )
        .map_err(classify_bridge_error)
        .map_err(CompanionRouteError::from)?;
    write_companion_stderr(output.stderr()).map_err(|_| CompanionRouteError::Unavailable)?;
    if matches!(
        output.exit_class(),
        ExitClass::Terminated(TerminationReason::Cancelled)
    ) {
        return Err(CompanionRouteError::Unavailable);
    }
    if output.stdout_truncated()
        || output.stderr_truncated()
        || output.exit_class() != ExitClass::Success
        || !is_one_framed_line(output.stdout())
    {
        return Err(CompanionRouteError::Incompatible);
    }
    Ok(output.stdout().to_vec())
}

pub(crate) fn wake_verified_private_maintenance(
    _data_root: &Path,
    cancellation: &CancellationToken,
) -> Result<(), CompanionRouteError> {
    let limits = BridgeLimits::new(LimitConfiguration {
        control_bytes: MAX_CONTROL_BYTES,
        input_bytes: 1,
        stdout_bytes: 256,
        stderr_bytes: 4 * 1024,
        arguments: 2,
        environment_entries: MAX_ENVIRONMENT_ENTRIES,
        concurrent_processes: MAX_CONCURRENT_PROCESSES,
        admission_wait: Duration::from_secs(5),
        captured_wall_time: Duration::from_secs(30),
    })
    .map_err(|_| CompanionRouteError::Incompatible)?;
    let companion = installed_companion().map_err(CompanionRouteError::from)?;
    let mut request = MaintenanceRequest::new();
    forward_environment(request.environment_mut());
    let response = CompanionBridge::new(limits)
        .launch_maintenance(&companion, request, cancellation)
        .map_err(classify_bridge_error)
        .map_err(CompanionRouteError::from)?;
    if !response.accepted() {
        return Err(CompanionRouteError::Incompatible);
    }
    Ok(())
}

fn forward_paid_cli(arguments: Vec<OsString>) -> Result<ExitCode, CompanionLaunchError> {
    let companion = installed_companion()?;
    let forwards_core_setup = forwarded_arguments_select_setup(&arguments);
    let mut request = CliRequest::new(arguments);
    forward_environment(request.environment_mut());
    if forwards_core_setup {
        forward_supervisor_environment(request.environment_mut());
    }
    forward_paid_cli_analytics_override(request.environment_mut());
    forward_terminal_environment(request.environment_mut());
    let exit = CompanionBridge::new(BridgeLimits::default())
        .launch_cli(&companion, request, companion_cancellation()?)
        .map_err(classify_bridge_error)?;
    Ok(exit_code(exit.exit_class()))
}

fn forward_environment(environment: &mut CompanionEnvironment) {
    forward_selected_environment(environment, FORWARDED_ENVIRONMENT);
}

fn forward_mcp_environment(environment: &mut CompanionEnvironment, data_root: &Path) {
    forward_environment(environment);
    let analytics_enabled = crate::config::AppConfig::load(data_root)
        .as_ref()
        .is_ok_and(|config| crate::config::resolved_analytics_consent(config));
    environment.set(
        EnvironmentKey::AnalyticsEnabled,
        if analytics_enabled { "true" } else { "false" },
    );
}

fn forward_paid_cli_analytics_override(environment: &mut CompanionEnvironment) {
    if let Some(enabled) = crate::config::normalized_analytics_environment_override() {
        environment.set(
            EnvironmentKey::AnalyticsEnabled,
            if enabled { "true" } else { "false" },
        );
    }
}

fn forward_supervisor_environment(environment: &mut CompanionEnvironment) {
    let names = ctx_daemon_cli::supervisor_environment_allowlist_names();
    environment.set_named(SUPERVISOR_ENV_NAMES, names.join("\n"));
    for name in names {
        if let Some(value) = std::env::var_os(name) {
            if name != "HOME" || !value.is_empty() {
                environment.set_named(name, value);
            }
        }
    }
}
fn forward_terminal_environment(environment: &mut CompanionEnvironment) {
    forward_selected_environment(environment, FORWARDED_TERMINAL_ENVIRONMENT);
}

fn forward_selected_environment<const N: usize>(
    environment: &mut CompanionEnvironment,
    selected: [(EnvironmentKey, &str); N],
) {
    for (key, name) in selected {
        if let Some(value) = std::env::var_os(name) {
            if environment_value_is_forwardable(key, value.as_os_str()) {
                environment.set(key, value);
            }
        }
    }
}

fn environment_value_is_forwardable(key: EnvironmentKey, value: &OsStr) -> bool {
    match key {
        EnvironmentKey::Home => !value.is_empty(),
        EnvironmentKey::HostedInstallerSetup => value == "1",
        _ => true,
    }
}

fn companion_cancellation() -> Result<&'static CancellationToken, CompanionLaunchError> {
    COMPANION_CANCELLATION
        .get_or_init(|| {
            let cancellation = CancellationToken::new();
            let trigger = cancellation.clone();
            ctrlc::set_handler(move || trigger.cancel())
                .map(|()| cancellation)
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|()| CompanionLaunchError::Unavailable)
}

fn installed_companion() -> Result<InstalledCompanion, CompanionLaunchError> {
    let core = std::env::current_exe().map_err(|_| CompanionLaunchError::Unavailable)?;
    let explicit_pro = std::env::var_os(PRO_PATH_OVERRIDE_ENVIRONMENT).map(PathBuf::from);
    installed_companion_from_parts(&core, explicit_pro)
}

fn installed_companion_from_parts(
    core: &Path,
    explicit_pro: Option<PathBuf>,
) -> Result<InstalledCompanion, CompanionLaunchError> {
    if !core.is_absolute() {
        return Err(CompanionLaunchError::InvalidLaunch {
            reason: "Core executable path must be absolute",
        });
    }
    let pro = match explicit_pro {
        Some(pro) => source_override_path(pro)?,
        None => official_companion_path(core)?,
    };
    Ok(InstalledCompanion::new(pro))
}

fn official_companion_path(core: &Path) -> Result<PathBuf, CompanionLaunchError> {
    let expected_core = if cfg!(windows) { "ctx.exe" } else { "ctx" };
    let expected_pro = if cfg!(windows) {
        "ctx-pro.exe"
    } else {
        "ctx-pro"
    };
    let bin = core
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("bin")))
        .filter(|_| core.file_name() == Some(OsStr::new(expected_core)))
        .ok_or(CompanionLaunchError::InvalidLaunch {
            reason: "installed Core path must be <root>/bin/ctx",
        })?;
    let install = bin.parent().ok_or(CompanionLaunchError::InvalidLaunch {
        reason: "installed Core path has no installation root",
    })?;
    Ok(install.join("libexec").join(expected_pro))
}

fn source_override_path(pro: PathBuf) -> Result<PathBuf, CompanionLaunchError> {
    if !pro.is_absolute() {
        return Err(CompanionLaunchError::InvalidLaunch {
            reason: "Pro override path must be absolute",
        });
    }
    Ok(pro)
}

pub(crate) fn managed_pair_expectations() -> Result<ManagedPairExpectations, CompanionRouteError> {
    let marker = match ctx_upgrade_engine::managed_install_marker_for_current_exe()
        .map_err(|_| CompanionRouteError::Unavailable)?
    {
        ctx_upgrade_engine::ManagedInstallMarker::Valid(marker) => marker,
        ctx_upgrade_engine::ManagedInstallMarker::Absent => {
            return Err(CompanionRouteError::Unavailable)
        }
        ctx_upgrade_engine::ManagedInstallMarker::Invalid { .. } => {
            return Err(CompanionRouteError::Incompatible)
        }
    };
    let channel = if marker.staging_dogfood {
        ReleaseChannel::Staging
    } else if marker.channel == "stable" {
        ReleaseChannel::Stable
    } else {
        return Err(CompanionRouteError::Incompatible);
    };
    Ok(ManagedPairExpectations::new(channel))
}

fn mcp_limits() -> Result<BridgeLimits, CompanionRouteError> {
    BridgeLimits::new(LimitConfiguration {
        control_bytes: MAX_CONTROL_BYTES,
        input_bytes: MCP_PROXY_MAX_BYTES,
        stdout_bytes: MCP_PROXY_MAX_BYTES,
        stderr_bytes: MAX_STDERR_BYTES,
        arguments: MAX_ARGUMENTS,
        environment_entries: MAX_ENVIRONMENT_ENTRIES,
        concurrent_processes: MAX_CONCURRENT_PROCESSES,
        admission_wait: MAX_ADMISSION_WAIT,
        captured_wall_time: MAX_CAPTURED_WALL_TIME,
    })
    .map_err(|_| CompanionRouteError::Incompatible)
}

fn paid_family_arguments(arguments: &[OsString]) -> Option<Vec<OsString>> {
    let explicit_pro = has_explicit_pro_selector(arguments);
    let mut index = 1;
    while let Some(argument) = arguments.get(index) {
        if is_global_help_or_version(argument) {
            return arguments[1..index]
                .iter()
                .any(|candidate| candidate == "--pro")
                .then(|| arguments[1..].to_vec());
        }
        if argument == "--" {
            index += 1;
            break;
        }
        if argument == "--data-root" || argument == "--color" {
            index = index.saturating_add(2);
            continue;
        }
        if argument == "--quiet"
            || has_attached_global_value(argument)
            || starts_with_dash(argument)
        {
            index += 1;
            continue;
        }
        if explicit_pro
            || ["pro", "blame", "referral"]
                .iter()
                .any(|family| argument == family)
        {
            return Some(arguments[1..].to_vec());
        }
        if argument == "help"
            && arguments.get(index + 1).is_some_and(|candidate| {
                [
                    "pro",
                    "blame",
                    "referral",
                    "setup",
                    "status",
                    "doctor",
                    "upgrade",
                    "uninstall",
                ]
                .iter()
                .any(|family| {
                    candidate == family
                        && (explicit_pro
                            || !matches!(
                                *family,
                                "setup" | "status" | "doctor" | "upgrade" | "uninstall"
                            ))
                })
            })
        {
            return Some(arguments[1..].to_vec());
        }
        return None;
    }
    if explicit_pro {
        return Some(arguments[1..].to_vec());
    }
    arguments.get(index).and_then(|argument| {
        ["pro", "blame", "referral"]
            .iter()
            .any(|family| argument == family)
            .then(|| arguments[1..].to_vec())
    })
}

fn has_explicit_pro_selector(arguments: &[OsString]) -> bool {
    arguments
        .iter()
        .skip(1)
        .take_while(|argument| argument.as_os_str() != OsStr::new("--"))
        .any(|argument| argument == "--pro")
}

fn is_global_help_or_version(value: &OsStr) -> bool {
    matches!(value.to_str(), Some("-h" | "--help" | "-V" | "--version"))
}

fn has_attached_global_value(value: &OsStr) -> bool {
    let bytes = value.as_encoded_bytes();
    bytes.starts_with(b"--data-root=") || bytes.starts_with(b"--color=")
}

fn starts_with_dash(value: &OsStr) -> bool {
    value.as_encoded_bytes().starts_with(b"-")
}

fn forwarded_arguments_select_setup(arguments: &[OsString]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" {
            return arguments
                .get(index + 1)
                .is_some_and(|value| value == "setup");
        }
        if argument == "--data-root" || argument == "--color" {
            index = index.saturating_add(2);
            continue;
        }
        if argument == "--quiet"
            || argument == "--pro"
            || has_attached_global_value(argument)
            || starts_with_dash(argument)
        {
            index += 1;
            continue;
        }
        return argument == "setup";
    }
    false
}

fn is_one_framed_line(bytes: &[u8]) -> bool {
    bytes.last() == Some(&b'\n') && !bytes[..bytes.len().saturating_sub(1)].contains(&b'\n')
}

fn classify_bridge_error(error: BridgeError) -> CompanionLaunchError {
    match error {
        BridgeError::MissingExecutable { path } => CompanionLaunchError::MissingExecutable { path },
        BridgeError::ProtocolMismatch { expected, observed } => {
            CompanionLaunchError::ProtocolMismatch { expected, observed }
        }
        BridgeError::HandshakeFailed {
            stderr,
            stderr_truncated,
            ..
        } => CompanionLaunchError::LaunchFailed {
            stderr,
            stderr_truncated,
        },
        BridgeError::InvalidExecutablePath => CompanionLaunchError::InvalidLaunch {
            reason: "Pro executable path must be absolute",
        },
        BridgeError::ExecutableNotFile { .. } => CompanionLaunchError::InvalidLaunch {
            reason: "Pro executable path is not a file",
        },
        BridgeError::Verification(_) => CompanionLaunchError::InvalidLaunch {
            reason: "detached installation verification failed",
        },
        BridgeError::InvalidProtocolResponse(_) => CompanionLaunchError::InvalidLaunch {
            reason: "Pro returned an invalid Protocol V3 response",
        },
        BridgeError::ExecutableMetadata { .. }
        | BridgeError::Limit(_)
        | BridgeError::InvalidEnvironmentName
        | BridgeError::QueueTimeout
        | BridgeError::CancelledBeforeSpawn
        | BridgeError::Spawn(_)
        | BridgeError::Transport(_)
        | BridgeError::WorkerFailed
        | BridgeError::UnsupportedPlatform => CompanionLaunchError::Unavailable,
    }
}

fn exit_code(exit: ExitClass) -> ExitCode {
    let code = match exit {
        ExitClass::Success => 0,
        ExitClass::Code(code) => u8::try_from(code).unwrap_or(1),
        #[cfg(unix)]
        ExitClass::Signal(signal) => u8::try_from(128_i32.saturating_add(signal)).unwrap_or(1),
        ExitClass::Terminated(TerminationReason::Cancelled) => 130,
        ExitClass::UnknownFailure | ExitClass::Terminated(_) => 1,
    };
    ExitCode::from(code)
}

fn write_companion_stderr(bytes: &[u8]) -> std::io::Result<()> {
    std::io::stderr().write_all(bytes)?;
    std::io::stderr().flush()
}

fn write_cli_launch_error(error: &CompanionLaunchError) {
    let document = cli_launch_error_document(error);
    let _ = writeln!(std::io::stderr(), "{document}");
}

fn cli_launch_error_document(error: &CompanionLaunchError) -> Value {
    let code = error.code();
    let mut document = json!({
        "error": code,
        "error_code": code,
        "retryable": error.retryable(),
    });
    let details = match error {
        CompanionLaunchError::MissingExecutable { path } => {
            json!({"executable": path})
        }
        CompanionLaunchError::ProtocolMismatch { expected, observed } => json!({
            "expected_protocol_version": expected.get(),
            "observed_protocol_version": observed.get(),
        }),
        CompanionLaunchError::LaunchFailed {
            stderr,
            stderr_truncated,
        } => json!({
            "reason": "companion exited before Protocol V3 handshake",
            "stderr": String::from_utf8_lossy(stderr),
            "stderr_truncated": stderr_truncated,
        }),
        CompanionLaunchError::InvalidLaunch { reason } => json!({"reason": reason}),
        CompanionLaunchError::Unavailable => Value::Null,
    };
    if !details.is_null() {
        document["details"] = details;
    }
    document
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANALYTICS_ENVIRONMENT_NAMES: &[&str] = &[
        "CTX_ANALYTICS_ENABLED",
        "CTX_ANALYTICS_ENDPOINT",
        "CTX_ANALYTICS_OFF",
        "CTX_DISABLE_ANALYTICS",
        "CTX_INSTALL_DIAGNOSTICS_OFF",
    ];

    struct AnalyticsEnvironment {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl AnalyticsEnvironment {
        fn new() -> Self {
            let lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let saved = ANALYTICS_ENVIRONMENT_NAMES
                .iter()
                .map(|&name| {
                    let value = std::env::var_os(name);
                    std::env::remove_var(name);
                    (name, value)
                })
                .collect();
            Self { _lock: lock, saved }
        }

        fn set(&self, name: &'static str, value: &str) {
            std::env::set_var(name, value);
        }

        fn set_os(&self, name: &'static str, value: &OsStr) {
            std::env::set_var(name, value);
        }

        fn remove(&self, name: &'static str) {
            std::env::remove_var(name);
        }
    }

    impl Drop for AnalyticsEnvironment {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }

    fn paid_cli_environment(forwards_core_setup: bool) -> CompanionEnvironment {
        let mut environment = CompanionEnvironment::new();
        forward_environment(&mut environment);
        if forwards_core_setup {
            forward_supervisor_environment(&mut environment);
        }
        forward_paid_cli_analytics_override(&mut environment);
        forward_terminal_environment(&mut environment);
        environment
    }

    #[test]
    fn paid_gate_forwards_the_original_arguments_without_paid_parsing() {
        let arguments = [
            OsString::from("ctx"),
            OsString::from("--data-root"),
            OsString::from("opaque-root"),
            OsString::from("blame"),
            OsString::from("--private-option"),
            OsString::from("opaque-value"),
        ];
        assert_eq!(
            paid_family_arguments(&arguments),
            Some(arguments[1..].to_vec())
        );
    }

    #[test]
    fn core_routes_never_enter_the_companion_gate() {
        for family in [
            "setup",
            "status",
            "doctor",
            "upgrade",
            "uninstall",
            "search",
            "show",
            "mcp",
        ] {
            let arguments = [OsString::from("ctx"), OsString::from(family)];
            assert!(paid_family_arguments(&arguments).is_none(), "{family}");
        }
    }

    #[test]
    fn explicit_pro_selector_routes_setup_and_other_core_families() {
        for arguments in [
            vec!["ctx", "--pro", "setup"],
            vec!["ctx", "setup", "--pro"],
            vec!["ctx", "--pro", "status"],
            vec!["ctx", "--pro", "--help"],
            vec!["ctx", "help", "setup", "--pro"],
        ] {
            let arguments = arguments
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>();
            assert_eq!(
                paid_family_arguments(&arguments),
                Some(arguments[1..].to_vec())
            );
        }
        assert!(paid_family_arguments(&[
            OsString::from("ctx"),
            OsString::from("setup"),
            OsString::from("--"),
            OsString::from("--pro"),
        ])
        .is_none());
    }

    #[test]
    fn forwarded_environment_is_the_complete_fixed_allowlist() {
        assert!(
            FORWARDED_ENVIRONMENT.len() + FORWARDED_TERMINAL_ENVIRONMENT.len()
                < MAX_ENVIRONMENT_ENTRIES
        );
        assert!(FORWARDED_ENVIRONMENT
            .contains(&(EnvironmentKey::LocalUsageEnabled, "CTX_LOCAL_USAGE_ENABLED")));
        assert!(!FORWARDED_ENVIRONMENT
            .iter()
            .any(|(key, _)| *key == EnvironmentKey::AnalyticsEnabled));
        assert!(FORWARDED_ENVIRONMENT.contains(&(
            EnvironmentKey::HostedInstallerSetup,
            "CTX_HOSTED_INSTALLER_SETUP"
        )));
        assert!(FORWARDED_ENVIRONMENT.contains(&(EnvironmentKey::Home, "HOME")));
        assert!(FORWARDED_ENVIRONMENT.contains(&(EnvironmentKey::Path, "PATH")));
        assert!(FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::Term, "TERM")));
        assert!(FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::ColorTerm, "COLORTERM")));
        assert!(FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::NoColor, "NO_COLOR")));
        assert!(FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::CliColor, "CLICOLOR")));
        assert!(FORWARDED_TERMINAL_ENVIRONMENT
            .contains(&(EnvironmentKey::CliColorForce, "CLICOLOR_FORCE")));
        assert!(FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::Ci, "CI")));
        assert!(environment_value_is_forwardable(
            EnvironmentKey::Home,
            OsStr::new("/home/tester")
        ));
        assert!(!environment_value_is_forwardable(
            EnvironmentKey::Home,
            OsStr::new("")
        ));
        assert!(environment_value_is_forwardable(
            EnvironmentKey::HostedInstallerSetup,
            OsStr::new("1")
        ));
        assert!(!environment_value_is_forwardable(
            EnvironmentKey::HostedInstallerSetup,
            OsStr::new("0")
        ));
    }

    #[test]
    fn paid_cli_analytics_override_is_normalized_closed_and_optional() {
        let controls = AnalyticsEnvironment::new();
        let analytics_name = EnvironmentKey::AnalyticsEnabled.as_str();

        controls.set(
            "CTX_ANALYTICS_ENDPOINT",
            "https://ambient.example.test/private",
        );
        let absent = paid_cli_environment(false);
        assert_eq!(absent.get(analytics_name), None);
        assert_eq!(absent.get("CTX_ANALYTICS_ENDPOINT"), None);
        controls.remove("CTX_ANALYTICS_ENDPOINT");

        for value in ["false", " 0 ", "NO", "off"] {
            controls.set("CTX_ANALYTICS_ENABLED", value);
            let environment = paid_cli_environment(false);
            assert_eq!(environment.get(analytics_name), Some(OsStr::new("false")));
        }
        for value in ["true", " 1 ", "YES", "on"] {
            controls.set("CTX_ANALYTICS_ENABLED", value);
            let environment = paid_cli_environment(false);
            assert_eq!(environment.get(analytics_name), Some(OsStr::new("true")));
        }
        for value in ["", "malformed", "2"] {
            controls.set("CTX_ANALYTICS_ENABLED", value);
            let environment = paid_cli_environment(false);
            assert_eq!(environment.get(analytics_name), Some(OsStr::new("false")));
        }

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            let non_unicode = OsString::from_vec(vec![0xff]);
            controls.set_os("CTX_ANALYTICS_ENABLED", &non_unicode);
            let environment = paid_cli_environment(false);
            assert_eq!(environment.get(analytics_name), Some(OsStr::new("false")));
        }

        controls.remove("CTX_ANALYTICS_ENABLED");
        for alias in [
            "CTX_ANALYTICS_OFF",
            "CTX_DISABLE_ANALYTICS",
            "CTX_INSTALL_DIAGNOSTICS_OFF",
        ] {
            controls.set(alias, "yes");
            let environment = paid_cli_environment(false);
            assert_eq!(environment.get(analytics_name), Some(OsStr::new("false")));
            controls.remove(alias);
        }

        controls.set("CTX_ANALYTICS_ENABLED", "YES");
        controls.set("CTX_ANALYTICS_OFF", "1");
        controls.set(
            "CTX_ANALYTICS_ENDPOINT",
            "https://ambient.example.test/private",
        );
        let setup = paid_cli_environment(true);
        assert_eq!(setup.get(analytics_name), Some(OsStr::new("false")));
        assert_eq!(setup.get("CTX_ANALYTICS_ENDPOINT"), None);
    }

    #[test]
    fn mcp_analytics_consent_is_resolved_from_authoritative_config() {
        let controls = AnalyticsEnvironment::new();

        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(crate::config::CONFIG_FILE),
            "[analytics]\nenabled = true\n",
        )
        .unwrap();
        let mut enabled = CompanionEnvironment::new();
        forward_mcp_environment(&mut enabled, root.path());
        assert_eq!(
            enabled.get(EnvironmentKey::AnalyticsEnabled.as_str()),
            Some(OsStr::new("true"))
        );
        assert_eq!(enabled.get("CTX_ANALYTICS_ENDPOINT"), None);

        std::env::set_var("CTX_ANALYTICS_ENABLED", "false");
        let mut overridden = CompanionEnvironment::new();
        forward_mcp_environment(&mut overridden, root.path());
        assert_eq!(
            overridden.get(EnvironmentKey::AnalyticsEnabled.as_str()),
            Some(OsStr::new("false"))
        );

        std::env::set_var("CTX_ANALYTICS_ENABLED", "true");
        let mut explicitly_enabled = CompanionEnvironment::new();
        forward_mcp_environment(&mut explicitly_enabled, root.path());
        assert_eq!(
            explicitly_enabled.get(EnvironmentKey::AnalyticsEnabled.as_str()),
            Some(OsStr::new("true"))
        );

        std::env::set_var("CTX_ANALYTICS_ENABLED", "true");
        std::fs::write(
            root.path().join(crate::config::CONFIG_FILE),
            "[analytics]\nenabled = false\n",
        )
        .unwrap();
        let mut persisted_disabled = CompanionEnvironment::new();
        forward_mcp_environment(&mut persisted_disabled, root.path());
        assert_eq!(
            persisted_disabled.get(EnvironmentKey::AnalyticsEnabled.as_str()),
            Some(OsStr::new("false"))
        );

        std::fs::write(
            root.path().join(crate::config::CONFIG_FILE),
            "[analytics]\nenabled = true\n",
        )
        .unwrap();
        for value in ["", "malformed", "2"] {
            controls.set("CTX_ANALYTICS_ENABLED", value);
            let mut malformed_override = CompanionEnvironment::new();
            forward_mcp_environment(&mut malformed_override, root.path());
            assert_eq!(
                malformed_override.get(EnvironmentKey::AnalyticsEnabled.as_str()),
                Some(OsStr::new("false")),
                "override {value:?} must fail closed"
            );
        }

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            controls.set_os("CTX_ANALYTICS_ENABLED", &OsString::from_vec(vec![0xff]));
            let mut non_unicode_override = CompanionEnvironment::new();
            forward_mcp_environment(&mut non_unicode_override, root.path());
            assert_eq!(
                non_unicode_override.get(EnvironmentKey::AnalyticsEnabled.as_str()),
                Some(OsStr::new("false"))
            );
        }

        controls.remove("CTX_ANALYTICS_ENABLED");
        for alias in [
            "CTX_ANALYTICS_OFF",
            "CTX_DISABLE_ANALYTICS",
            "CTX_INSTALL_DIAGNOSTICS_OFF",
        ] {
            controls.set(alias, "yes");
            let mut deprecated_opt_out = CompanionEnvironment::new();
            forward_mcp_environment(&mut deprecated_opt_out, root.path());
            assert_eq!(
                deprecated_opt_out.get(EnvironmentKey::AnalyticsEnabled.as_str()),
                Some(OsStr::new("false")),
                "deprecated alias {alias} must fail closed"
            );
            controls.remove(alias);
        }

        std::env::set_var("CTX_ANALYTICS_ENABLED", "true");
        std::fs::write(
            root.path().join(crate::config::CONFIG_FILE),
            "[analytics]\nenabled = malformed\n",
        )
        .unwrap();
        let mut malformed = CompanionEnvironment::new();
        forward_mcp_environment(&mut malformed, root.path());
        assert_eq!(
            malformed.get(EnvironmentKey::AnalyticsEnabled.as_str()),
            Some(OsStr::new("false"))
        );
    }

    #[test]
    fn setup_pro_forwards_the_complete_named_supervisor_environment() {
        static ENVIRONMENT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        struct Restore(Vec<(&'static str, Option<OsString>)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (name, value) in self.0.drain(..) {
                    if let Some(value) = value {
                        std::env::set_var(name, value);
                    } else {
                        std::env::remove_var(name);
                    }
                }
            }
        }

        let _lock = ENVIRONMENT_LOCK.lock().unwrap();
        let values = [
            ("CODEX_HOME", "/tmp/codex-home"),
            ("CTX_UPGRADE_AUTO", "false"),
            ("HTTP_PROXY", "http://proxy.example.test:8080"),
            ("SSL_CERT_FILE", "/tmp/private-ca.pem"),
        ];
        let _restore = Restore(
            values
                .iter()
                .map(|(name, _)| (*name, std::env::var_os(name)))
                .chain(std::iter::once(("HOME", std::env::var_os("HOME"))))
                .collect(),
        );
        for (name, value) in values {
            std::env::set_var(name, value);
        }
        std::env::set_var("HOME", "");

        let mut environment = CompanionEnvironment::new();
        forward_supervisor_environment(&mut environment);
        let names = environment
            .get(SUPERVISOR_ENV_NAMES)
            .unwrap()
            .to_str()
            .unwrap()
            .split('\n')
            .collect::<Vec<_>>();
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        for (name, value) in values {
            assert!(names.contains(&name));
            assert_eq!(environment.get(name), Some(OsStr::new(value)));
        }
        assert!(names.contains(&"CTX_SEARCH_SEMANTIC"));
        assert!(names.contains(&"CTX_UPGRADE_CHANNEL"));
        assert!(names.contains(&"HOME"));
        assert_eq!(environment.get("HOME"), None);
    }

    #[test]
    fn only_forwarded_setup_arguments_request_the_supervisor_environment() {
        for arguments in [
            vec!["--pro", "setup"],
            vec!["--data-root", "/tmp/setup", "setup", "--pro"],
            vec!["--", "setup", "--pro"],
        ] {
            let arguments = arguments
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>();
            assert!(forwarded_arguments_select_setup(&arguments));
        }
        for arguments in [
            vec!["--pro", "status"],
            vec!["--data-root", "setup", "status", "--pro"],
            vec!["help", "setup", "--pro"],
        ] {
            let arguments = arguments
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>();
            assert!(!forwarded_arguments_select_setup(&arguments));
        }
    }

    #[test]
    fn explicit_override_selects_only_the_protocol_compatible_pro_executable() {
        let temp = tempfile::tempdir().unwrap();
        let source_core = temp.path().join("source/target/debug/ctx");
        let pro = temp.path().join("installed/libexec/ctx-pro");
        let companion = installed_companion_from_parts(&source_core, Some(pro.clone()))
            .expect("source override");

        assert_eq!(companion.executable(), pro);
    }

    #[test]
    fn installed_core_defaults_to_its_sibling_pro_executable() {
        let temp = tempfile::tempdir().unwrap();
        let core =
            temp.path()
                .join("installed/bin")
                .join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
        let expected = temp
            .path()
            .join("installed/libexec")
            .join(if cfg!(windows) {
                "ctx-pro.exe"
            } else {
                "ctx-pro"
            });
        let companion = installed_companion_from_parts(&core, None).unwrap();

        assert_eq!(companion.executable(), expected);
    }

    #[test]
    fn missing_pro_is_a_distinct_typed_error() {
        let temp = tempfile::tempdir().unwrap();
        let core =
            temp.path()
                .join("installed/bin")
                .join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
        let companion = installed_companion_from_parts(&core, None).unwrap();
        let missing_path = companion.executable().to_path_buf();
        let error = CompanionBridge::default()
            .launch_mcp(
                &companion,
                McpRequest::new(Vec::new()),
                &CancellationToken::new(),
            )
            .unwrap_err();
        let error = classify_bridge_error(error);
        assert!(matches!(
            error,
            CompanionLaunchError::MissingExecutable { ref path } if path == &missing_path
        ));
        assert_eq!(error.code(), "companion_missing_executable");
    }

    #[cfg(unix)]
    #[test]
    fn protocol_v3_alone_launches_pro_without_any_context_environment() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let core = temp.path().join("installed/bin/ctx");
        let pro = temp.path().join("installed/libexec/ctx-pro");
        std::fs::create_dir_all(core.parent().unwrap()).unwrap();
        std::fs::create_dir_all(pro.parent().unwrap()).unwrap();
        std::fs::write(
            &pro,
            br##"#!/bin/sh
if [ "$1" = "--ctx-pro-protocol-v3" ] && [ "$2" = "handshake" ]; then
  printf '{"protocol_version":3}\n'
  exit 0
fi
if [ "$1" != "--ctx-pro-protocol-v3" ] || [ "$2" != "mcp-serve" ]; then
  exit 91
fi
for name in CTX_PRO_PATH CTX_PRO_INSTALL_CONTEXT CTX_DATA_ROOT CTX_PRO_DATA_ROOT CTX_MANAGED_PAIR_CHANNEL CTX_PRO_INSTALLATION_ID CTX_MANAGED_PAIR_INVOCATION_FINGERPRINT CTX_MANAGED_PAIR_CORE_CAPABILITY_FINGERPRINT CTX_RELEASE_BUILD_SOURCE_COMMIT; do
  eval "value=\${$name-}"
  [ -z "$value" ] || exit 92
done
printf '{"jsonrpc":"2.0"}\n'
"##,
        )
        .unwrap();
        std::fs::set_permissions(&pro, std::fs::Permissions::from_mode(0o700)).unwrap();
        let companion = installed_companion_from_parts(&core, None).unwrap();
        let response = CompanionBridge::default()
            .launch_mcp(
                &companion,
                McpRequest::new(Vec::new()),
                &CancellationToken::new(),
            )
            .unwrap();

        assert_eq!(response.exit_class(), ExitClass::Success);
        assert_eq!(response.stdout(), b"{\"jsonrpc\":\"2.0\"}\n");
        assert!(response.stderr().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn protocol_mismatch_is_a_distinct_typed_error() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let pro = temp.path().join("ctx-pro");
        std::fs::write(&pro, b"#!/bin/sh\nprintf '{\"protocol_version\":2}\\n'\n").unwrap();
        std::fs::set_permissions(&pro, std::fs::Permissions::from_mode(0o700)).unwrap();
        let error = CompanionBridge::default()
            .launch_mcp(
                &InstalledCompanion::new(&pro),
                McpRequest::new(Vec::new()),
                &CancellationToken::new(),
            )
            .unwrap_err();
        let error = classify_bridge_error(error);
        assert!(matches!(
            error,
            CompanionLaunchError::ProtocolMismatch {
                expected,
                observed,
            } if expected.get() == 3 && observed.get() == 2
        ));
        assert_eq!(error.code(), "companion_protocol_mismatch");
    }

    #[test]
    fn pre_handshake_exit_is_retryable_unavailable_not_protocol_mismatch() {
        let error = classify_bridge_error(BridgeError::HandshakeFailed {
            exit: ExitClass::Code(70),
            stderr: b"loader diagnostic".to_vec(),
            stderr_truncated: false,
        });
        assert!(matches!(
            error,
            CompanionLaunchError::LaunchFailed {
                ref stderr,
                stderr_truncated: false,
            } if stderr == b"loader diagnostic"
        ));
        assert_eq!(error.code(), "companion_unavailable");
        assert!(error.retryable());
        let document = cli_launch_error_document(&error);
        assert_eq!(document["details"]["stderr"], "loader diagnostic");
        assert_eq!(document["details"]["stderr_truncated"], false);
        assert!(document["details"]
            .get("observed_protocol_version")
            .is_none());
        assert_eq!(
            CompanionRouteError::from(error),
            CompanionRouteError::Unavailable
        );
    }

    #[test]
    fn global_help_and_version_never_enter_the_companion_gate() {
        for option in ["-h", "--help", "-V", "--version"] {
            for family in ["pro", "blame", "referral"] {
                let arguments = [
                    OsString::from("ctx"),
                    OsString::from(option),
                    OsString::from(family),
                ];
                assert!(
                    paid_family_arguments(&arguments).is_none(),
                    "{option} {family}"
                );
            }
        }
    }

    #[test]
    fn subcommand_help_and_help_alias_route_to_the_companion() {
        for arguments in [
            vec!["ctx", "pro", "--help"],
            vec!["ctx", "blame", "--help"],
            vec!["ctx", "referral", "--help"],
            vec!["ctx", "help", "pro"],
        ] {
            let arguments = arguments
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>();
            assert_eq!(
                paid_family_arguments(&arguments),
                Some(arguments[1..].to_vec())
            );
        }
    }

    #[test]
    fn paid_data_root_argument_is_forwarded_without_core_derivation() {
        let arguments = [
            OsString::from("ctx"),
            OsString::from("--data-root=relative-root"),
            OsString::from("pro"),
        ];
        assert_eq!(
            paid_family_arguments(&arguments),
            Some(arguments[1..].to_vec())
        );
    }

    #[test]
    fn data_root_options_after_delimiter_remain_opaque_pro_arguments() {
        for trailing in [
            vec!["--data-root", "/private/positional"],
            vec!["--data-root=/private/positional"],
        ] {
            let mut arguments = vec![
                OsString::from("ctx"),
                OsString::from("pro"),
                OsString::from("--"),
            ];
            arguments.extend(trailing.into_iter().map(OsString::from));
            assert_eq!(
                paid_family_arguments(&arguments),
                Some(arguments[1..].to_vec()),
                "{arguments:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn opaque_paid_arguments_are_preserved_byte_for_byte() {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let opaque = OsString::from_vec(vec![b'v', 0xff, b'x']);
        let arguments = [
            OsString::from("ctx"),
            OsString::from("referral"),
            opaque.clone(),
        ];
        let forwarded = paid_family_arguments(&arguments).unwrap();
        assert_eq!(
            forwarded[1].as_os_str().as_bytes(),
            opaque.as_os_str().as_bytes()
        );
    }

    #[test]
    fn mcp_response_must_be_one_opaque_framed_line() {
        assert!(is_one_framed_line(b"{\"jsonrpc\":\"2.0\"}\n"));
        assert!(!is_one_framed_line(b"{}"));
        assert!(!is_one_framed_line(b"{}\n{}\n"));
    }
}

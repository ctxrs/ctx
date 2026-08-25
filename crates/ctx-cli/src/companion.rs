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
        .is_ok_and(crate::config::resolved_analytics_consent);
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
#[path = "companion/tests.rs"]
mod tests;

//! Fixed, process-neutral Core capability endpoint used by a
//! protocol-compatible installed companion. This is deliberately not a
//! general command runner.

use std::{
    collections::BTreeSet,
    io::Read as _,
    path::{Component, Path, PathBuf},
    process::ExitCode,
};

use anyhow::{anyhow, Context as _, Result};
use ctx_companion_bridge::{
    verify_signed_managed_pair_envelope, SignedManagedPairIdentity, SignedManagedPairTarget,
    CORE_PRO_PROTOCOL_VERSION,
};
use ctx_history_cli::HistoryConfigPort;
use ctx_upgrade_engine::{
    ManagedPairComponentIdentity, ManagedPairEngine, ManagedPairTarget,
    ManagedPairTransactionStatus, ManagedPairVerifier, VerifiedManagedPairIdentity,
};
use serde_json::{json, Value};
#[cfg(test)]
use sha2::{Digest as _, Sha256};

mod failure;
mod hosted_pair_install;
mod progress_events;
mod setup_options;
mod setup_refresh;

use failure::produce_response;
use progress_events::{CapabilityEventSink, IgnoreEvents, ProtocolEventWriter};
use setup_options::{
    progress_mode_for_notice, setup_notice_lines, setup_progress_mode, SetupProgressMode,
};
use setup_refresh::core_setup_refresh;
#[cfg(test)]
use setup_refresh::{
    should_propagate_setup_refresh_failure, should_wait_for_fresh_empty_publication,
};

const INVOCATION: &str = "--ctx-core-capability-v1";
const HOSTED_PAIR_INSTALL_INVOCATION: &str = "--ctx-core-hosted-pair-install-v1";
const POST_EXIT_INVOCATION: &str = "--ctx-core-managed-pair-swap-v1";
const POST_EXIT_UNINSTALL_INVOCATION: &str = "--ctx-core-managed-pair-uninstall-v1";
const MAX_FRAME_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 48 * 1024;
#[cfg(test)]
const API_INVENTORY: &str = r#"{"event_frames":{"Refresh":{"current_source_progress_keys":["logical_certified_bytes","logical_rows_scanned","snapshot_bytes_completed","snapshot_bytes_total","snapshot_pages_completed","snapshot_pages_total","stage"],"frame_keys":["event","operation","protocol_version","refresh","schema_version","sequence","type"],"refresh_keys":["completed_bytes","completed_records","completed_sources","current_source","current_source_progress","elapsed_millis","estimated_remaining_millis","logical_phase","maintenance_wake","phase","physical_attempt_id","physical_attempt_state","processed_bytes","processed_messages","processed_sessions","processed_tool_calls","progress_owner_attempt_state","progress_owner_request_id","providers","request_id","request_state","terminal_state","total_sources","total_sources_known","whole_run_stage"],"terminal_state_details_keys":["affected_routes","blocked_routes","class","physical_attempt_id","published_generation","retained_generation","retry_advice","retryable_routes"],"terminal_state_keys":["details","error_code","retryable"]}},"operations":{"CoreDoctor":{"request_keys":[],"response_keys":["facts"]},"CoreSetup":{"request_keys":["catalog_only","defer_fresh_empty_wait","no_daemon","notice_lines","progress","semantic","wait"],"request_values":{"progress":["auto","events","json","none","plain"]},"response_keys":["facts","generation_id"]},"CoreStatus":{"request_keys":["usage"],"response_keys":["facts"]},"LocalUsageSummary":{"request_keys":[],"response_keys":["facts"]},"ManagedPairAbort":{"request_keys":["attempt_id"],"response_keys":["aborted"]},"ManagedPairBegin":{"request_keys":[],"response_keys":["attempt_id","candidate_root"]},"ManagedPairStage":{"request_keys":["attempt_id"],"response_keys":["attempt_id","release_name","rollback_generation","status"]},"ManagedPairStatus":{"request_keys":["attempt_id"],"response_keys":["status"]},"ManagedPairUninstall":{"request_keys":[],"response_keys":["attempt_id","cleanup_mode","status"]},"RefreshAndWait":{"optional_request_keys":["progress"],"request_keys":[],"request_values":{"progress":["events"]},"response_keys":["facts","generation_id"]},"WakeRefresh":{"request_keys":[],"response_keys":["accepted","analytics_enabled"]}},"protocol":"ctx-core-capability","schema_version":1,"terminal_failure":{"classes":["control_plane","corruption","coverage","incompatible","internal","mixed","resource_unavailable","source_changed","unavailable","unreadable"],"details_keys":["affected_routes","blocked_routes","class","physical_attempt_id","retained_generation","retry_advice","retryable_routes"],"error_codes":["all_provider_terminal_coverage_unavailable","index_corruption","index_incompatible","logical_source_failures","malformed_source","resource_unavailable","source_changed","source_failures","source_refresh_admission_failed","source_refresh_failed","source_refresh_internal","source_unavailable","source_unclaimed","unsupported_schema"],"response_keys":["details","error_code","ok","operation","protocol_version","retryable","schema_version"],"retry_advice":["inspect_sources","rebuild_index","retry_admission","retry_affected_routes","retry_finalization","retry_request","retry_retryable_routes_and_inspect_blocked","upgrade_or_reconfigure"]}}"#;
#[cfg(test)]
pub(crate) const API_FINGERPRINT: &str =
    "73e1caffd18462f24f16bfedf99581b4d3062a22e2ecfbd00110cd61fbd66352";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    CoreSetup,
    CoreStatus,
    CoreDoctor,
    LocalUsageSummary,
    RefreshAndWait,
    WakeRefresh,
    ManagedPairBegin,
    ManagedPairStage,
    ManagedPairAbort,
    ManagedPairStatus,
    ManagedPairUninstall,
}

impl Operation {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "CoreSetup" => Ok(Self::CoreSetup),
            "CoreStatus" => Ok(Self::CoreStatus),
            "CoreDoctor" => Ok(Self::CoreDoctor),
            "LocalUsageSummary" => Ok(Self::LocalUsageSummary),
            "RefreshAndWait" => Ok(Self::RefreshAndWait),
            "WakeRefresh" => Ok(Self::WakeRefresh),
            "ManagedPairBegin" => Ok(Self::ManagedPairBegin),
            "ManagedPairStage" => Ok(Self::ManagedPairStage),
            "ManagedPairAbort" => Ok(Self::ManagedPairAbort),
            "ManagedPairStatus" => Ok(Self::ManagedPairStatus),
            "ManagedPairUninstall" => Ok(Self::ManagedPairUninstall),
            _ => Err(anyhow!("unknown operation")),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::CoreSetup => "CoreSetup",
            Self::CoreStatus => "CoreStatus",
            Self::CoreDoctor => "CoreDoctor",
            Self::LocalUsageSummary => "LocalUsageSummary",
            Self::RefreshAndWait => "RefreshAndWait",
            Self::WakeRefresh => "WakeRefresh",
            Self::ManagedPairBegin => "ManagedPairBegin",
            Self::ManagedPairStage => "ManagedPairStage",
            Self::ManagedPairAbort => "ManagedPairAbort",
            Self::ManagedPairStatus => "ManagedPairStatus",
            Self::ManagedPairUninstall => "ManagedPairUninstall",
        }
    }
}

/// Intercepts only the fixed hidden invocation. Any spelling variation stays in
/// the ordinary public parser and receives no privileged transport.
pub(crate) fn intercept(arguments: &[std::ffi::OsString]) -> Option<ExitCode> {
    if arguments
        .get(1)
        .is_some_and(|value| value == HOSTED_PAIR_INSTALL_INVOCATION)
    {
        return Some(match hosted_pair_install::run(arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error:#}");
                ExitCode::FAILURE
            }
        });
    }
    if arguments
        .get(1)
        .is_some_and(|value| value == POST_EXIT_INVOCATION)
    {
        return Some(match run_post_exit(arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        });
    }
    if arguments
        .get(1)
        .is_some_and(|value| value == POST_EXIT_UNINSTALL_INVOCATION)
    {
        return Some(match run_post_exit_uninstall(arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        });
    }
    if arguments.len() != 2 || arguments.get(1).is_none_or(|value| value != INVOCATION) {
        return None;
    }
    Some(capability_exit_code(run()))
}

fn capability_exit_code(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn run() -> Result<()> {
    run_with_protocol_io(std::io::stdin().lock(), std::io::stdout().lock(), execute)
}

fn run_with_protocol_io(
    reader: impl std::io::Read,
    mut writer: impl std::io::Write,
    execute_request: impl FnOnce(Request, &mut dyn CapabilityEventSink) -> Result<Value>,
) -> Result<()> {
    let input = read_frame_from(reader)?;
    let (bytes, terminal_error) = produce_response(input, |request| {
        let mut events = ProtocolEventWriter::new(request.operation, &mut writer);
        execute_request(request, &mut events)
    })?;
    write_response_frame(&mut writer, &bytes)?;
    if let Some(error) = terminal_error {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
fn run_with_io(
    reader: impl std::io::Read,
    writer: impl std::io::Write,
    execute_request: impl FnOnce(Request) -> Result<Value>,
) -> Result<()> {
    run_with_protocol_io(reader, writer, |request, _events| execute_request(request))
}

fn write_response_frame(mut writer: impl std::io::Write, bytes: &[u8]) -> Result<()> {
    writer.write_all(bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

struct Request {
    data_root: PathBuf,
    operation: Operation,
    options: Options,
}

enum Options {
    Setup(CoreSetupOptions),
    Status { usage: Option<UsageAction> },
    Refresh { events: bool },
    PairAttempt { attempt_id: String },
    Empty,
}

struct CoreSetupOptions {
    catalog_only: bool,
    defer_fresh_empty_wait: bool,
    no_daemon: bool,
    notice_lines: Vec<String>,
    progress: SetupProgressMode,
    semantic: bool,
    wait: bool,
}

#[derive(Clone, Copy)]
enum UsageAction {
    Enable,
    Disable,
    Reset,
}

fn parse_frame(bytes: Vec<u8>) -> Result<Request> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES || bytes.contains(&0) {
        return Err(anyhow!("invalid frame bound"));
    }
    let text = std::str::from_utf8(&bytes).context("frame is not UTF-8")?;
    if text.contains('\n') || text.contains('\r') {
        return Err(anyhow!("frame is not one line"));
    }
    reject_duplicate_keys(text)?;
    let value: Value = serde_json::from_str(text).context("invalid JSON")?;
    if canonical(&value)? != bytes {
        return Err(anyhow!("frame is not canonical JSON"));
    }
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("request is not an object"))?;
    exact_keys(
        object.keys().map(String::as_str),
        [
            "data_root",
            "operation",
            "options",
            "protocol_version",
            "schema_version",
        ],
    )?;
    if object.get("schema_version") != Some(&json!(1))
        || object.get("protocol_version").and_then(Value::as_u64)
            != Some(u64::from(CORE_PRO_PROTOCOL_VERSION.get()))
    {
        return Err(anyhow!("Core↔Pro protocol version mismatch"));
    }
    let root = object
        .get("data_root")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing data root"))?;
    let data_root = normalized_absolute_root(root)?;
    let operation = Operation::parse(
        object
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing operation"))?,
    )?;
    let options = object
        .get("options")
        .ok_or_else(|| anyhow!("missing options"))?;
    let options = parse_options(operation, options)?;
    Ok(Request {
        data_root,
        operation,
        options,
    })
}

fn execute(request: Request, events: &mut dyn CapabilityEventSink) -> Result<Value> {
    if !matches!(
        request.operation,
        Operation::LocalUsageSummary
            | Operation::ManagedPairBegin
            | Operation::ManagedPairStage
            | Operation::ManagedPairAbort
            | Operation::ManagedPairStatus
            | Operation::ManagedPairUninstall
    ) {
        crate::semantic::initialize()?;
    }
    let facts = match (request.operation, request.options) {
        (Operation::CoreSetup, Options::Setup(options)) => {
            core_setup_facts(&request.data_root, options, events)?
        }
        (Operation::CoreStatus, Options::Status { usage }) => {
            core_status_facts(&request.data_root, usage)?
        }
        (Operation::CoreDoctor, Options::Empty) => {
            crate::commands::doctor::doctor_facts(&request.data_root)?
        }
        (Operation::LocalUsageSummary, Options::Empty) => {
            local_usage_summary_facts(&request.data_root)?
        }
        (Operation::RefreshAndWait, Options::Refresh { events: true }) => {
            refresh_and_facts(&request.data_root, events)?
        }
        (Operation::RefreshAndWait, Options::Refresh { events: false }) => {
            refresh_and_facts(&request.data_root, &mut IgnoreEvents)?
        }
        (Operation::WakeRefresh, Options::Empty) => wake_refresh_facts(&request.data_root),
        (Operation::ManagedPairBegin, Options::Empty) => {
            let verifier = CoreManagedPairVerifier::new()?;
            let attempt = managed_pair_engine()?.begin(&verifier)?;
            json!({
                "attempt_id": attempt.attempt_id(),
                "candidate_root": attempt.candidate_root(),
            })
        }
        (Operation::ManagedPairStage, Options::PairAttempt { attempt_id }) => {
            let verifier = CoreManagedPairVerifier::new()?;
            let prepared = managed_pair_engine()?.stage_attempt(&attempt_id, &verifier)?;
            json!({
                "attempt_id": prepared.attempt_id(),
                "release_name": prepared.identity().release_name(),
                "rollback_generation": prepared.identity().rollback_generation(),
                "status": "staged",
            })
        }
        (Operation::ManagedPairAbort, Options::PairAttempt { attempt_id }) => {
            json!({"aborted": managed_pair_engine()?.abort(&attempt_id)?})
        }
        (Operation::ManagedPairStatus, Options::PairAttempt { attempt_id }) => {
            let status = managed_pair_engine()?.status(&attempt_id)?;
            json!({"status": managed_pair_status_name(status)})
        }
        (Operation::ManagedPairUninstall, Options::Empty) => {
            let verifier = CoreManagedPairVerifier::new()?;
            let attempt = managed_pair_engine()?.prepare_uninstall(&verifier)?;
            json!({
                "attempt_id": attempt.attempt_id(),
                "cleanup_mode": if attempt.retry_or_reboot_may_be_required() {
                    "retry_or_reboot_required_if_running_core_is_locked"
                } else {
                    "post_exit"
                },
                "status": "armed",
            })
        }
        _ => return Err(anyhow!("operation options are inconsistent")),
    };
    Ok(json!({
        "facts": facts,
        "ok": true,
        "operation": request.operation.name(),
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "schema_version": 1,
    }))
}

fn wake_refresh_facts(data_root: &Path) -> Value {
    let config = ctx_app_config::AppConfig::load(data_root);
    let analytics_enabled = config
        .as_ref()
        .is_ok_and(crate::analytics::effective_analytics_enabled);
    if let Ok(config) = config {
        crate::semantic::maybe_autostart_daemon(
            data_root,
            &config,
            crate::DaemonTriggerCommandArg::Setup,
        );
    }
    json!({"accepted": true, "analytics_enabled": analytics_enabled})
}

fn status_facts(data_root: &Path) -> Result<Value> {
    let config = ctx_app_config::AppConfig::load(data_root)?;
    let storage = crate::observability_composition::local_usage_storage_authority(data_root);
    let control =
        crate::observability_composition::usage_control_snapshot(config.local_usage.enabled);
    bounded_value(
        crate::commands::status::status_read_model_authorized(
            data_root, &config, &storage, &control,
        )?
        .report,
    )
}

fn local_usage_summary_facts(data_root: &Path) -> Result<Value> {
    let storage = crate::observability_composition::local_usage_storage_authority(data_root);
    let mut control =
        crate::observability_composition::LocalUsageControlAuthority::new(data_root.to_path_buf());
    bounded_value(serde_json::to_value(
        crate::local_usage::read_report_authorized(&storage, &control.snapshot(), false),
    )?)
}

fn core_status_facts(data_root: &Path, usage: Option<UsageAction>) -> Result<Value> {
    let usage_action = match usage {
        Some(UsageAction::Enable | UsageAction::Disable) => {
            let enabled = matches!(usage, Some(UsageAction::Enable));
            ctx_app_config::set_local_usage_enabled(data_root, enabled)?;
            let control = ctx_app_config::read_local_usage_control(data_root)?;
            Some(json!({
                "action": if enabled { "enable" } else { "disable" },
                "effective_enabled": control.effective_enabled,
                "environment_override": control.environment_override.as_str(),
                "persisted_enabled": control.persisted_enabled,
            }))
        }
        Some(UsageAction::Reset) => {
            let storage =
                crate::observability_composition::local_usage_storage_authority(data_root);
            let cleared = crate::local_usage::reset_authorized(&storage)?;
            Some(
                json!({"action": "reset", "store_state": if cleared { "cleared" } else { "missing" }}),
            )
        }
        None => None,
    };
    bounded_value(json!({
        "status": status_facts(data_root)?,
        "usage_action": usage_action,
    }))
}

fn core_setup_facts(
    data_root: &Path,
    options: CoreSetupOptions,
    events: &mut dyn CapabilityEventSink,
) -> Result<Value> {
    let CoreSetupOptions {
        catalog_only,
        defer_fresh_empty_wait,
        no_daemon,
        notice_lines,
        progress: progress_mode,
        semantic,
        wait,
    } = options;
    let mut config = ctx_app_config::AppConfig::load(data_root)?;
    if semantic {
        ctx_app_config::set_semantic_search_enabled(data_root, true)?;
        config = ctx_app_config::AppConfig::load(data_root)?;
    }
    crate::history_config::CliHistoryConfigAdapter::new(data_root, &mut config)
        .write_default_config()?;

    let daemon_requested = config.automatic_indexing_enabled() && !no_daemon;
    if daemon_requested {
        let _ = crate::semantic::autostart_daemon_for_setup_and_wait(
            data_root,
            &config,
            crate::DaemonTriggerCommandArg::Setup,
        )?;
    }
    let (published_generation, refresh_request) = if daemon_requested {
        core_setup_refresh(
            data_root,
            wait,
            defer_fresh_empty_wait,
            &notice_lines,
            progress_mode,
            events,
        )?
    } else {
        (
            None,
            json!({
                "daemon_available": false,
                "mode": if wait { "wait" } else { "background" },
                "reason": if no_daemon { "explicit_opt_out" } else { "daemon_disabled" },
                "status": "unavailable",
            }),
        )
    };
    let source_epoch = crate::semantic::source_epoch_status_report(data_root, &config)?;
    bounded_setup_facts(
        json!({
            "deprecated_catalog_only_ignored": catalog_only,
            "daemon_requested": daemon_requested,
            "refresh_request": refresh_request,
            "semantic_enabled": config.semantic_search_enabled(),
            "wait": wait,
        }),
        published_generation,
        &source_epoch.report,
    )
}

fn bounded_setup_facts(
    mut facts: Value,
    published: Option<String>,
    source_epoch: &Value,
) -> Result<Value> {
    facts["generation_id"] = json!(setup_generation_id(published, source_epoch));
    bounded_value(facts)
}

fn setup_generation_id(published: Option<String>, source_epoch: &Value) -> Option<String> {
    published.or_else(|| {
        source_epoch["lexical"]["generation_id"]
            .as_str()
            .map(str::to_owned)
    })
}

fn refresh_and_facts(data_root: &Path, events: &mut dyn CapabilityEventSink) -> Result<Value> {
    let mut terminal_progress = None;
    let mut progress = |status: &crate::semantic::RefreshStatus| {
        if status.kind()?.request_state().is_terminal() {
            terminal_progress = Some(status.schema_v1_fields().clone());
            Ok(())
        } else {
            events.refresh(status)
        }
    };
    let result = crate::semantic::coordinate_source_backed_refresh_with_progress(
        data_root,
        crate::semantic::SourceBackedRefreshMode::Wait,
        &mut progress,
    );
    let observation = match result {
        Ok(observation) => observation,
        Err(error) => {
            if let Some(terminal) = terminal_progress.take() {
                let terminal = crate::semantic::RefreshStatus::parse_schema_v1(terminal)?;
                events.refresh(&terminal)?;
            }
            return Err(error);
        }
    };
    if let Some(terminal) = terminal_progress.take() {
        let terminal = crate::semantic::RefreshStatus::parse_schema_v1(terminal)?;
        events.refresh(&terminal)?;
    }
    let generation_id = observation.pin.generation_id().to_owned();
    bounded_value(json!({"generation_id": generation_id}))
}

fn parse_options(operation: Operation, value: &Value) -> Result<Options> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("options are not an object"))?;
    if operation == Operation::RefreshAndWait {
        return match object.len() {
            0 => Ok(Options::Refresh { events: false }),
            1 if object.get("progress").and_then(Value::as_str) == Some("events") => {
                Ok(Options::Refresh { events: true })
            }
            _ => Err(anyhow!("refresh progress option is invalid")),
        };
    }
    let expected: &[&str] = match operation {
        Operation::CoreSetup => &[
            "catalog_only",
            "defer_fresh_empty_wait",
            "no_daemon",
            "notice_lines",
            "progress",
            "semantic",
            "wait",
        ],
        Operation::CoreStatus => &["usage"],
        Operation::CoreDoctor
        | Operation::LocalUsageSummary
        | Operation::WakeRefresh
        | Operation::ManagedPairBegin
        | Operation::ManagedPairUninstall => &[],
        Operation::ManagedPairStage
        | Operation::ManagedPairAbort
        | Operation::ManagedPairStatus => &["attempt_id"],
        Operation::RefreshAndWait => unreachable!("refresh options returned above"),
    };
    exact_keys(object.keys().map(String::as_str), expected.iter().copied())?;
    match operation {
        Operation::CoreSetup => Ok(Options::Setup(CoreSetupOptions {
            catalog_only: required_bool(object, "catalog_only")?,
            defer_fresh_empty_wait: required_bool(object, "defer_fresh_empty_wait")?,
            no_daemon: required_bool(object, "no_daemon")?,
            notice_lines: setup_notice_lines(object)?,
            progress: setup_progress_mode(object)?,
            semantic: required_bool(object, "semantic")?,
            wait: required_bool(object, "wait")?,
        })),
        Operation::CoreStatus => Ok(Options::Status {
            usage: match object.get("usage") {
                Some(Value::Null) => None,
                Some(Value::String(value)) if value == "enable" => Some(UsageAction::Enable),
                Some(Value::String(value)) if value == "disable" => Some(UsageAction::Disable),
                Some(Value::String(value)) if value == "reset" => Some(UsageAction::Reset),
                _ => return Err(anyhow!("status usage option is invalid")),
            },
        }),
        Operation::ManagedPairStage
        | Operation::ManagedPairAbort
        | Operation::ManagedPairStatus => Ok(Options::PairAttempt {
            attempt_id: object
                .get("attempt_id")
                .and_then(Value::as_str)
                .filter(|value| {
                    value.len() == 32
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                })
                .ok_or_else(|| anyhow!("managed-pair attempt ID is invalid"))?
                .to_owned(),
        }),
        _ => Ok(Options::Empty),
    }
}

struct CoreManagedPairVerifier {
    expectations: ctx_companion_bridge::ManagedPairExpectations,
}

impl CoreManagedPairVerifier {
    fn new() -> Result<Self> {
        let expectations = crate::companion::managed_pair_expectations()
            .map_err(|error| anyhow!("{}", error.code()))?;
        Ok(Self { expectations })
    }
}

impl ManagedPairVerifier for CoreManagedPairVerifier {
    fn verify_signed_envelope(
        &self,
        signed_envelope: &[u8],
    ) -> Result<VerifiedManagedPairIdentity> {
        let identity = verify_signed_managed_pair_envelope(&self.expectations, signed_envelope)
            .map_err(|error| anyhow!(error.to_string()))?;
        engine_identity(&identity)
    }
}

fn engine_identity(identity: &SignedManagedPairIdentity) -> Result<VerifiedManagedPairIdentity> {
    let target = match identity.target() {
        SignedManagedPairTarget::LinuxArm64 => ManagedPairTarget::LinuxArm64,
        SignedManagedPairTarget::LinuxX64 => ManagedPairTarget::LinuxX64,
        SignedManagedPairTarget::MacosArm64 => ManagedPairTarget::MacosArm64,
        SignedManagedPairTarget::MacosX64 => ManagedPairTarget::MacosX64,
        SignedManagedPairTarget::WindowsX64 => ManagedPairTarget::WindowsX64,
    };
    VerifiedManagedPairIdentity::new(
        identity.release_name(),
        target,
        identity.rollback_generation(),
        identity.manifest_sha256().to_hex(),
        ManagedPairComponentIdentity::new(
            identity.core().sha256().to_hex(),
            identity.core().size_bytes(),
        )?,
        ManagedPairComponentIdentity::new(
            identity.companion().sha256().to_hex(),
            identity.companion().size_bytes(),
        )?,
    )
}

fn managed_pair_engine() -> Result<ManagedPairEngine> {
    let root = std::env::current_dir().context("resolve managed-pair install root")?;
    ManagedPairEngine::new(root)
}

fn managed_pair_status_name(status: ManagedPairTransactionStatus) -> &'static str {
    match status {
        ManagedPairTransactionStatus::Absent => "absent",
        ManagedPairTransactionStatus::Begun => "begun",
        ManagedPairTransactionStatus::Staging => "staging",
        ManagedPairTransactionStatus::Staged => "staged",
        ManagedPairTransactionStatus::Deferred => "deferred",
        ManagedPairTransactionStatus::Activating => "activating",
        ManagedPairTransactionStatus::Committed => "committed",
        ManagedPairTransactionStatus::Aborted => "aborted",
        ManagedPairTransactionStatus::Failed => "failed",
        ManagedPairTransactionStatus::RollingBack => "rolling_back",
    }
}

fn run_post_exit(arguments: &[std::ffi::OsString]) -> Result<()> {
    if arguments.len() != 5 {
        return Err(anyhow!("invalid managed-pair post-exit invocation"));
    }
    let attempt_id = arguments[2]
        .to_str()
        .filter(|value| value.len() == 32)
        .ok_or_else(|| anyhow!("invalid managed-pair attempt ID"))?;
    let parent_pid = arguments[3]
        .to_str()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 1)
        .ok_or_else(|| anyhow!("invalid managed-pair parent PID"))?;
    let parent_creation_time = match arguments[4].to_str() {
        Some("-") => None,
        Some(value) => Some(
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value != 0)
                .ok_or_else(|| anyhow!("invalid managed-pair parent identity"))?,
        ),
        None => return Err(anyhow!("invalid managed-pair parent identity")),
    };
    managed_pair_engine()?.run_post_exit_swapper_after_parent_exit(
        attempt_id,
        &CoreManagedPairVerifier::new()?,
        parent_pid,
        parent_creation_time,
    )
}

fn run_post_exit_uninstall(arguments: &[std::ffi::OsString]) -> Result<()> {
    let (attempt_id, parent_pid, parent_creation_time) = post_exit_arguments(arguments)?;
    managed_pair_engine()?.run_post_exit_uninstall_after_parent_exit(
        attempt_id,
        parent_pid,
        parent_creation_time,
    )?;
    Ok(())
}

fn post_exit_arguments(arguments: &[std::ffi::OsString]) -> Result<(&str, u32, Option<u64>)> {
    if arguments.len() != 5 {
        return Err(anyhow!("invalid managed-pair post-exit invocation"));
    }
    let attempt_id = arguments[2]
        .to_str()
        .filter(|value| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
        .ok_or_else(|| anyhow!("invalid managed-pair attempt ID"))?;
    let parent_pid = arguments[3]
        .to_str()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 1)
        .ok_or_else(|| anyhow!("invalid managed-pair parent PID"))?;
    let parent_creation_time = match arguments[4].to_str() {
        Some("-") => None,
        Some(value) => Some(
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value != 0)
                .ok_or_else(|| anyhow!("invalid managed-pair parent identity"))?,
        ),
        None => return Err(anyhow!("invalid managed-pair parent identity")),
    };
    Ok((attempt_id, parent_pid, parent_creation_time))
}

fn required_bool(object: &serde_json::Map<String, Value>, key: &str) -> Result<bool> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("setup option {key} must be boolean"))
}

fn normalized_absolute_root(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || value.len() > 16 * 1024 || value.contains('\0') {
        return Err(anyhow!("data root must be bounded and absolute"));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(anyhow!("data root must be lexically normalized"));
    }
    let normalized = std::fs::canonicalize(&path).unwrap_or(path.clone());
    if normalized != path {
        return Err(anyhow!("data root must already be normalized"));
    }
    Ok(path)
}

fn read_frame_from(reader: impl std::io::Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_FRAME_BYTES + 2) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.len() > MAX_FRAME_BYTES || bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        return Err(anyhow!("input has multiple frames or trailing data"));
    }
    Ok(bytes)
}

fn canonical(value: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(value).context("canonicalize JSON")
}

fn bounded_value(value: Value) -> Result<Value> {
    if canonical(&value)?.len() > MAX_RESPONSE_BYTES.saturating_sub(256) {
        return Err(anyhow!("Core facts exceed response bound"));
    }
    Ok(value)
}

fn exact_keys<'a>(
    actual: impl IntoIterator<Item = &'a str>,
    expected: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let actual = actual.into_iter().collect::<BTreeSet<_>>();
    let expected = expected.into_iter().collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(anyhow!("request keys are not exact"));
    }
    Ok(())
}

/// serde_json accepts duplicate object names, so reject them before decoding.
/// This compact scanner only recognizes JSON structure and decoded string keys;
/// all syntax remains owned by serde_json afterwards.
fn reject_duplicate_keys(input: &str) -> Result<()> {
    fn string(bytes: &[u8], index: &mut usize) -> Result<String> {
        let start = *index;
        *index += 1;
        let mut escaped = false;
        while *index < bytes.len() {
            let byte = bytes[*index];
            *index += 1;
            if escaped {
                escaped = false;
                continue;
            }
            if byte == b'\\' {
                escaped = true;
                continue;
            }
            if byte == b'"' {
                return serde_json::from_slice(&bytes[start..*index])
                    .map_err(|_| anyhow!("invalid JSON string"));
            }
            if byte < 0x20 {
                return Err(anyhow!("invalid JSON string"));
            }
        }
        Err(anyhow!("unterminated JSON string"))
    }
    fn value(bytes: &[u8], index: &mut usize) -> Result<()> {
        while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
            *index += 1;
        }
        match bytes.get(*index) {
            Some(b'{') => {
                *index += 1;
                let mut keys = BTreeSet::new();
                while bytes.get(*index) != Some(&b'}') {
                    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
                        *index += 1;
                    }
                    if bytes.get(*index) != Some(&b'"') {
                        return Err(anyhow!("object key expected"));
                    }
                    let key = string(bytes, index)?;
                    if !keys.insert(key) {
                        return Err(anyhow!("duplicate JSON key"));
                    }
                    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
                        *index += 1;
                    }
                    if bytes.get(*index) != Some(&b':') {
                        return Err(anyhow!("object colon expected"));
                    }
                    *index += 1;
                    value(bytes, index)?;
                    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
                        *index += 1;
                    }
                    match bytes.get(*index) {
                        Some(b',') => *index += 1,
                        Some(b'}') => (),
                        _ => return Err(anyhow!("object delimiter expected")),
                    }
                }
                *index += 1;
                Ok(())
            }
            Some(b'[') => {
                *index += 1;
                while bytes.get(*index) != Some(&b']') {
                    value(bytes, index)?;
                    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
                        *index += 1;
                    }
                    match bytes.get(*index) {
                        Some(b',') => *index += 1,
                        Some(b']') => (),
                        _ => return Err(anyhow!("array delimiter expected")),
                    }
                }
                *index += 1;
                Ok(())
            }
            Some(b'"') => string(bytes, index).map(drop),
            Some(_) => {
                while bytes.get(*index).is_some_and(|byte| {
                    !matches!(byte, b',' | b']' | b'}') && !byte.is_ascii_whitespace()
                }) {
                    *index += 1;
                }
                Ok(())
            }
            None => Err(anyhow!("unexpected end of JSON")),
        }
    }
    let bytes = input.as_bytes();
    let mut index = 0;
    value(bytes, &mut index)?;
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    if index != bytes.len() {
        return Err(anyhow!("trailing JSON data"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "core_capability/contract_tests.rs"]
mod tests;

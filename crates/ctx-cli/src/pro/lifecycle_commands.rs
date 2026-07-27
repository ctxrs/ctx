use std::{
    fs,
    io::{self, IsTerminal as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use ctx_history_core::platform_security::{restrict_private_file, verify_private_file};
use ctx_pro_host_protocol::ProFilesystemLayout;
#[cfg(test)]
use ctx_pro_host_protocol::{Capability, PROTOCOL_FINGERPRINT, PROTOCOL_VERSION};
use serde_json::json;

use super::{
    default_helper_path, install_marker_path, install_verified_bundle, previous_helper_path,
    previous_marker_path, publish_helper_path, publish_marker_path,
    reconcile_setup_installation_locked, replace_file, rollback_helper_stage_path,
    rollback_marker_stage_path, sync_parent_directory, transaction_helper_path,
    transaction_journal_next_path, transaction_journal_path, transaction_marker_path, Persistence,
    SetupInstallation,
};
use crate::pro::artifact_delivery::VerifiedArtifactBundle;
#[cfg(test)]
use crate::pro::client::HelperSmoke;
use crate::pro::client::{
    materialize, smoke_helper_at_path, status, ProSetupRepairability, ProStatus,
};
use crate::pro::commercial_lifecycle::CommercialLifecycleService;
use crate::pro::lifecycle::lifecycle_manifest::ReleaseTrust;
use crate::pro::local_deletion::{
    clear_local_pro_initialization_indicator, local_pro_graph_data_exists,
    local_pro_graph_key_cleanup_phase_exists, local_pro_initialization_indicator_exists,
    write_local_pro_initialization_indicator, LocalDeletionService,
};
use crate::pro::pending_materialization;
use crate::pro::setup_validation::{
    setup_artifact, validate_account_state, validate_staged_helper,
};
use crate::{
    analytics::{
        send_pro_operation, Outcome, ProAccessStateV1, ProHelperConnectionOutcomeV1,
        ProHostOperationV1, ProLifecycleOperationV1, ProLifecycleTelemetryV1,
        ProMaterializationTelemetryV1, ProReconcileOutcomeV1, ProUninstallDataDispositionV1,
    },
    pro::stable_error_code,
};

#[derive(Debug, Args)]
pub(crate) struct ProArgs {
    #[command(subcommand)]
    command: Option<ProCommand>,
    #[arg(long, help = "Print machine-readable JSON")]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum ProCommand {
    #[command(about = "Explicit synonym for `ctx pro`")]
    Setup(ProSetupArgs),
    #[command(about = "Open account and billing management")]
    Manage(ProManageArgs),
    #[command(about = "Remove Pro and choose whether to delete or preserve local Pro data")]
    Uninstall(ProUninstallArgs),
}

#[derive(Debug, Args)]
struct ProSetupArgs {
    #[arg(long)]
    json: bool,
    #[arg(long, hide = true)]
    defer_materialization: bool,
}

#[derive(Debug, Args)]
struct ProManageArgs {
    #[arg(long, help = "Print the portal URL without opening a browser")]
    no_open: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ProUninstallArgs {
    #[arg(long, conflicts_with = "keep_data", help = "Delete local Pro data")]
    delete_data: bool,
    #[arg(
        long,
        conflicts_with = "delete_data",
        help = "Preserve local Pro data for later setup"
    )]
    keep_data: bool,
    #[arg(long)]
    json: bool,
}

const UNINSTALL_DATA_PROMPT: &str =
    "Delete all local Pro data? It can be rebuilt if you set up Pro again. [Y/n]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UninstallDataDisposition {
    Delete,
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalProDataOutcome {
    Absent,
    Deleted,
    Preserved,
}

impl ProArgs {
    pub(crate) fn json_output(&self) -> bool {
        self.json
            || match &self.command {
                Some(ProCommand::Setup(args)) => args.json,
                Some(ProCommand::Manage(args)) => args.json,
                Some(ProCommand::Uninstall(args)) => args.json,
                None => false,
            }
    }

    fn telemetry_operation(&self) -> ProLifecycleOperationV1 {
        match &self.command {
            Some(ProCommand::Manage(_)) => ProLifecycleOperationV1::Manage,
            Some(ProCommand::Uninstall(_)) => ProLifecycleOperationV1::Uninstall,
            None | Some(ProCommand::Setup(_)) => ProLifecycleOperationV1::Setup,
        }
    }

    pub(crate) fn local_usage_operation(&self) -> &'static str {
        match &self.command {
            Some(ProCommand::Manage(_)) => "pro_manage",
            Some(ProCommand::Uninstall(_)) => "pro_uninstall",
            None | Some(ProCommand::Setup(_)) => "pro_setup",
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProSetupPlan {
    pub(crate) artifact: Option<VerifiedArtifactBundle>,
    pub(crate) account_state: String,
}

#[derive(Debug)]
pub(crate) struct ProManagePlan {
    pub(crate) portal_url: String,
    pub(crate) access_state: String,
    pub(crate) refresh_after_unix: Option<i64>,
    pub(crate) access_deadline_unix: Option<i64>,
    pub(crate) grace_deadline_unix: Option<i64>,
}

/// Narrow orchestration seam for the hosted commercial client. WorkOS/Stripe,
/// entitlement proof, and R2 delivery own their formats in separate modules.
pub(crate) trait ProLifecycleService {
    fn release_trust(&self) -> Result<ReleaseTrust>;
    fn setup(&mut self, data_root: &Path, installed_version: Option<&str>) -> Result<ProSetupPlan>;
    fn manage(&mut self, data_root: &Path) -> Result<ProManagePlan>;
}

/// Local-only destruction seam. Implementations may locate and delete native
/// vault records, but do not need commercial configuration or network access.
pub(crate) trait ProDeletionService {
    fn delete_graph_data(&mut self, data_root: &Path) -> Result<()>;
    fn delete_commercial_credentials(&mut self, data_root: &Path) -> Result<()>;
    fn finish_deletion(&mut self, data_root: &Path) -> Result<()>;
}

pub(crate) fn run_lifecycle(args: ProArgs, data_root: PathBuf) -> Result<()> {
    let started = Instant::now();
    let mut telemetry = ProLifecycleTelemetryV1::new(args.telemetry_operation());
    let result = run_lifecycle_inner(args, &data_root, &mut telemetry);
    if let Err(error) = &result {
        telemetry.fail(stable_error_code(error));
    }
    send_pro_operation(
        &data_root,
        ProHostOperationV1::Lifecycle(telemetry),
        if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Failure
        },
        started.elapsed(),
    );
    result
}

fn run_lifecycle_inner(
    args: ProArgs,
    data_root: &Path,
    telemetry: &mut ProLifecycleTelemetryV1,
) -> Result<()> {
    let json_output = args.json_output();
    let defer_materialization = matches!(
        &args.command,
        Some(ProCommand::Setup(ProSetupArgs {
            defer_materialization: true,
            ..
        }))
    );
    match args.command {
        None | Some(ProCommand::Setup(_)) => {
            crate::identity::installation_id(data_root)
                .context("key_store_unavailable: initialize local Pro installation identity")?;
            let mut service = CommercialLifecycleService::production(data_root)?;
            run_setup(
                data_root,
                &mut service,
                json_output,
                defer_materialization,
                telemetry,
            )
        }
        Some(ProCommand::Manage(args)) => {
            let mut service = CommercialLifecycleService::production(data_root)?;
            run_manage(
                data_root,
                &mut service,
                args.no_open,
                json_output,
                telemetry,
            )
        }
        Some(ProCommand::Uninstall(args)) => {
            let disposition = uninstall_data_disposition(&args, json_output)?;
            telemetry.uninstall_data = Some(match disposition {
                UninstallDataDisposition::Delete => ProUninstallDataDispositionV1::Delete,
                UninstallDataDisposition::Keep => ProUninstallDataDispositionV1::Preserve,
            });
            match disposition {
                UninstallDataDisposition::Delete => {
                    let mut service = LocalDeletionService::production();
                    run_uninstall(data_root, Some(&mut service), disposition, json_output)
                        .map(|_| ())
                }
                UninstallDataDisposition::Keep => {
                    run_uninstall(data_root, None, disposition, json_output).map(|_| ())
                }
            }
        }
    }
}

fn uninstall_data_disposition(
    args: &ProUninstallArgs,
    json_output: bool,
) -> Result<UninstallDataDisposition> {
    if args.delete_data {
        return Ok(UninstallDataDisposition::Delete);
    }
    if args.keep_data {
        return Ok(UninstallDataDisposition::Keep);
    }
    if json_output || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        bail!("invalid_request: noninteractive uninstall requires --delete-data or --keep-data");
    }
    prompt_uninstall_data_disposition(&mut io::stdin().lock(), &mut io::stderr().lock())
}

fn prompt_uninstall_data_disposition(
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
) -> Result<UninstallDataDisposition> {
    loop {
        write!(output, "{UNINSTALL_DATA_PROMPT} ")?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            bail!("cancelled: uninstall confirmation was not provided");
        }
        match answer.trim() {
            "" | "y" | "Y" | "yes" | "YES" => return Ok(UninstallDataDisposition::Delete),
            "n" | "N" | "no" | "NO" => return Ok(UninstallDataDisposition::Keep),
            _ => writeln!(output, "Please answer y or n.")?,
        }
    }
}

pub(crate) fn lifecycle_status_json(data_root: &Path) -> serde_json::Value {
    lifecycle_status_value(status(data_root), preserved_data_marker_is_set(data_root))
}

pub(super) fn lifecycle_status_value(helper: ProStatus, preserved_data: bool) -> serde_json::Value {
    let (state, next_command, next_reason) = if !helper.installed {
        match helper.setup_repairability {
            ProSetupRepairability::Automated => (
                "repair_required",
                Some("ctx pro"),
                "helper_artifacts_invalid",
            ),
            ProSetupRepairability::ManualDiagnosis => {
                ("unavailable", None, "manual_diagnosis_required")
            }
            ProSetupRepairability::NotNeeded if preserved_data => (
                "uninstalled_data_preserved",
                Some("ctx pro"),
                "restore_preserved_pro_data",
            ),
            ProSetupRepairability::NotNeeded => ("not_setup", Some("ctx pro"), "helper_missing"),
        }
    } else {
        match helper.error_code.as_deref() {
            None if helper.ready => ("ready", Some("ctx pro manage"), "billing_and_account"),
            Some("entitlement_expired") => (
                "locked",
                Some("ctx pro manage"),
                "subscription_or_trial_ended",
            ),
            Some("not_materialized" | "needs_rebuild" | "partial" | "needs_resume") => {
                ("catch_up_required", Some("ctx pro"), "graph_not_current")
            }
            Some("helper_upgrade_required" | "protocol_mismatch") => {
                ("repair_required", Some("ctx pro"), "helper_incompatible")
            }
            Some("key_store_unavailable" | "key_store_locked" | "corrupt_graph") => {
                ("repair_required", Some("ctx pro"), "local_pro_unavailable")
            }
            Some(_) | None => ("unavailable", Some("ctx pro"), "helper_unavailable"),
        }
    };
    json!({
        "schema_version": 2,
        "payload_type": "pro_status",
        "state": state,
        "installed": helper.installed,
        "ready": helper.ready,
        "materialized": helper.materialized,
        "helper_version": helper.helper_version,
        "protocol_version": helper.protocol_version,
        "capabilities": helper.capabilities,
        "error_code": helper.error_code,
        "access_state": helper.access_state,
        "refresh_after_unix": helper.refresh_after_unix,
        "access_deadline_unix": helper.access_deadline_unix,
        "grace_deadline_unix": helper.grace_deadline_unix,
        "next_action": {
            "command": next_command,
            "reason": next_reason,
        },
    })
}

fn run_setup(
    data_root: &Path,
    service: &mut dyn ProLifecycleService,
    json_output: bool,
    defer_materialization: bool,
    telemetry: &mut ProLifecycleTelemetryV1,
) -> Result<()> {
    let trust = service.release_trust()?;
    let target = default_helper_path(data_root);
    telemetry.reconcile = ProReconcileOutcomeV1::Failed;
    let (installation, plan) = with_pro_initialization(data_root, || {
        let installation = reconcile_setup_installation_locked(
            &target,
            trust.public_key_pem,
            &mut Persistence::default(),
        )?;
        let plan = service.setup(data_root, installation.installed_version())?;
        Ok((installation, plan))
    })?;
    let replacing_existing = matches!(
        &installation,
        SetupInstallation::Current(_) | SetupInstallation::RepairRequired
    );
    telemetry.reconcile = match &installation {
        SetupInstallation::Current(_) => ProReconcileOutcomeV1::Current,
        SetupInstallation::Missing => ProReconcileOutcomeV1::Missing,
        SetupInstallation::RepairRequired => ProReconcileOutcomeV1::Failed,
    };
    validate_account_state(&plan.account_state)?;
    telemetry.access_state = ProAccessStateV1::from_safe_name(&plan.account_state);
    let artifact = setup_artifact(&installation, plan.artifact)?;
    let helper_updated = if let Some(bundle) = artifact {
        let smoke = match smoke_helper_at_path(data_root, &bundle.artifact) {
            Ok(smoke) => {
                telemetry.helper_connection = ProHelperConnectionOutcomeV1::Connected;
                smoke
            }
            Err(error) => {
                telemetry.helper_connection =
                    crate::analytics::pro_helper_connection_outcome(stable_error_code(&error));
                return Err(error);
            }
        };
        validate_staged_helper(&smoke)?;
        install_verified_bundle(&bundle, data_root, trust)?;
        telemetry.reconcile = if replacing_existing {
            ProReconcileOutcomeV1::Updated
        } else {
            ProReconcileOutcomeV1::Installed
        };
        true
    } else {
        false
    };
    if defer_materialization {
        return pending_materialization::defer_setup(
            data_root,
            &plan.account_state,
            helper_updated,
            json_output,
        );
    }
    // An empty repository-root set still materializes canonical transcript evidence.
    let mut materialization = ProMaterializationTelemetryV1::started();
    let report = materialize(data_root, &mut materialization);
    if materialization.helper_connection != ProHelperConnectionOutcomeV1::NotAttempted {
        telemetry.helper_connection = materialization.helper_connection;
    }
    telemetry.materialization = Some(materialization);
    let report = report.map_err(crate::pro::actionable_error)?;
    let value = json!({
        "schema_version": 1,
        "payload_type": "pro_setup",
        "ok": true,
        "account_state": plan.account_state,
        "helper_updated": helper_updated,
        "graph": report,
        "status": lifecycle_status_json(data_root),
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("ctx Pro is ready.");
        println!("materialized observations: {}", report.observations);
    }
    Ok(())
}

fn run_manage(
    data_root: &Path,
    service: &mut dyn ProLifecycleService,
    no_open: bool,
    json_output: bool,
    telemetry: &mut ProLifecycleTelemetryV1,
) -> Result<()> {
    run_manage_with_opener(
        data_root,
        service,
        no_open,
        json_output,
        telemetry,
        &open_browser,
    )
}

fn run_manage_with_opener(
    data_root: &Path,
    service: &mut dyn ProLifecycleService,
    no_open: bool,
    json_output: bool,
    telemetry: &mut ProLifecycleTelemetryV1,
    opener: &dyn Fn(&str) -> Result<()>,
) -> Result<()> {
    let plan = with_pro_initialization(data_root, || service.manage(data_root))?;
    validate_portal_url(&plan.portal_url)?;
    validate_access_status(
        &plan.access_state,
        plan.refresh_after_unix,
        plan.access_deadline_unix,
        plan.grace_deadline_unix,
    )?;
    telemetry.access_state = ProAccessStateV1::from_safe_name(&plan.access_state);
    let browser_opened = !json_output && !no_open && opener(&plan.portal_url).is_ok();
    let mut value = manage_payload(&plan, browser_opened);
    let usage_report = crate::config::AppConfig::load(data_root)
        .map(|config| crate::local_usage::read_report(data_root, config.local_usage.enabled, false))
        .unwrap_or_else(|_| crate::local_usage::UsageReport::config_error());
    let conversion_action =
        crate::local_usage::pro_conversion_action(Some(plan.access_state.as_str()));
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "local_usage".to_owned(),
            serde_json::to_value(&usage_report)?,
        );
        object.insert(
            "conversion_action".to_owned(),
            conversion_action.clone().unwrap_or(serde_json::Value::Null),
        );
    }
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "Manage ctx Pro: {}",
            value["portal_url"].as_str().unwrap_or("")
        );
        if !no_open && !browser_opened {
            println!("A browser could not be opened; use the URL above.");
        }
        crate::local_usage::render_human_summary(&usage_report, false);
        if let Some(action) = conversion_action {
            if action["kind"] == "pro_restore_access" {
                println!(
                    "Restore ctx Pro access; the local graph is preserved: {}",
                    action["command"].as_str().unwrap_or("ctx pro manage")
                );
            } else {
                println!(
                    "Continue with ctx Pro for {}: {}",
                    action["price"].as_str().unwrap_or("$15/month"),
                    action["command"].as_str().unwrap_or("ctx pro manage")
                );
            }
        }
    }
    Ok(())
}

fn with_pro_initialization<T>(
    data_root: &Path,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let target = default_helper_path(data_root);
    let _lifecycle_lock = super::lifecycle_lock::LifecycleLock::acquire(&target, true)?
        .ok_or_else(|| anyhow::anyhow!("invalid_request: failed to create Pro lifecycle lock"))?;
    if local_pro_graph_key_cleanup_phase_exists(data_root)? {
        bail!(
            "key_store_unavailable: interrupted Pro deletion must be completed with `ctx pro uninstall --delete-data`"
        );
    }
    write_local_pro_initialization_indicator(data_root)?;
    operation()
}

fn manage_payload(plan: &ProManagePlan, browser_opened: bool) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "payload_type": "pro_manage",
        "portal_url": plan.portal_url,
        "browser_opened": browser_opened,
        "access_state": plan.access_state,
        "refresh_after_unix": plan.refresh_after_unix,
        "access_deadline_unix": plan.access_deadline_unix,
        "grace_deadline_unix": plan.grace_deadline_unix,
    })
}

fn validate_portal_url(value: &str) -> Result<()> {
    if value.len() > 4096 {
        bail!("invalid_response: Pro management URL exceeds the maximum length");
    }
    let parsed = url::Url::parse(value).context("invalid_response: invalid Pro management URL")?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() || parsed.username() != "" {
        bail!("invalid_response: Pro management URL must be an HTTPS origin");
    }
    Ok(())
}

fn validate_access_status(
    state: &str,
    refresh_after_unix: Option<i64>,
    access_deadline_unix: Option<i64>,
    grace_deadline_unix: Option<i64>,
) -> Result<()> {
    if !matches!(
        state,
        "trial" | "active" | "canceling_paid" | "offline_grace" | "locked"
    ) {
        bail!("invalid_response: Pro access state is invalid");
    }
    if [
        refresh_after_unix,
        access_deadline_unix,
        grace_deadline_unix,
    ]
    .into_iter()
    .flatten()
    .any(|value| value <= 0)
    {
        bail!("invalid_response: Pro access deadline is invalid");
    }
    if matches!((access_deadline_unix, grace_deadline_unix), (Some(access), Some(grace)) if access > grace)
    {
        bail!("invalid_response: Pro access deadlines are inconsistent");
    }
    if matches!(state, "trial" | "active" | "canceling_paid") && access_deadline_unix.is_none() {
        bail!("invalid_response: Pro access deadline is missing");
    }
    if state == "offline_grace" && (access_deadline_unix.is_none() || grace_deadline_unix.is_none())
    {
        bail!("invalid_response: Pro offline-grace deadlines are missing");
    }
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(url);
        command
    } else if cfg!(windows) {
        let mut command = Command::new("rundll32.exe");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("open Pro management URL")?;
    Ok(())
}

fn run_uninstall(
    data_root: &Path,
    service: Option<&mut dyn ProDeletionService>,
    disposition: UninstallDataDisposition,
    json_output: bool,
) -> Result<serde_json::Value> {
    let delete_data = disposition == UninstallDataDisposition::Delete;
    let target = default_helper_path(data_root);
    let initial_state = inspect_local_pro_uninstall_state(data_root)?;
    if !initial_state.any_artifact() {
        return emit_uninstall_result(false, LocalProDataOutcome::Absent, json_output);
    }
    let Some(_lifecycle_lock) = super::lifecycle_lock::LifecycleLock::acquire(&target, false)?
    else {
        return emit_uninstall_result(false, LocalProDataOutcome::Absent, json_output);
    };
    let state = inspect_local_pro_uninstall_state(data_root)?;
    pending_materialization::clear(data_root)?;
    if !delete_data && state.cleanup_phase {
        bail!(
            "key_store_unavailable: interrupted Pro deletion must be completed with `ctx pro uninstall --delete-data`"
        );
    }
    let helper_removed = if delete_data {
        let helper_removed =
            if state.initialized || state.graph_data || state.helper_files || state.cleanup_phase {
                let service = service.ok_or_else(|| {
                    anyhow::anyhow!("key_store_unavailable: local deletion service is unavailable")
                })?;
                // The public delete-only adapter destroys the exact native key record
                // and then removes all graph-family files. It does not launch or retain
                // the private helper and remains available after an ordinary uninstall.
                service.delete_graph_data(data_root)?;
                service.delete_commercial_credentials(data_root)?;
                let helper_removed = delete_helper_files(data_root)?;
                service.finish_deletion(data_root)?;
                helper_removed
            } else {
                delete_helper_files(data_root)?
            };
        clear_local_pro_initialization_indicator(data_root)?;
        clear_preserved_data_marker(data_root)?;
        helper_removed
    } else if state.graph_data {
        write_preserved_data_marker(data_root)?;
        delete_helper_files(data_root)?
    } else {
        clear_preserved_data_marker(data_root)?;
        delete_helper_files(data_root)?
    };
    let data_outcome = if state.graph_data {
        if delete_data {
            LocalProDataOutcome::Deleted
        } else {
            LocalProDataOutcome::Preserved
        }
    } else {
        LocalProDataOutcome::Absent
    };
    emit_uninstall_result(helper_removed, data_outcome, json_output)
}

fn emit_uninstall_result(
    helper_removed: bool,
    data_outcome: LocalProDataOutcome,
    json_output: bool,
) -> Result<serde_json::Value> {
    let value = uninstall_payload(helper_removed, data_outcome);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        match data_outcome {
            LocalProDataOutcome::Deleted => {
                println!("Local Pro data was deleted. Canonical ctx history was preserved.");
                println!("next: ctx pro (rebuild Pro data)");
            }
            LocalProDataOutcome::Preserved => {
                println!("ctx Pro was removed. Local Pro data was preserved.");
                println!("next: ctx pro (restore preserved Pro data)");
            }
            LocalProDataOutcome::Absent if helper_removed => {
                println!(
                    "ctx Pro was removed. No local Pro data was found. Canonical ctx history was preserved."
                );
            }
            LocalProDataOutcome::Absent => {
                println!(
                    "No ctx Pro installation or local Pro data was found. Canonical ctx history was preserved."
                );
            }
        }
    }
    Ok(value)
}

fn uninstall_payload(helper_removed: bool, data_outcome: LocalProDataOutcome) -> serde_json::Value {
    let next_action = match data_outcome {
        LocalProDataOutcome::Deleted => Some(json!({
            "command": "ctx pro",
            "reason": "rebuild_pro_data",
        })),
        LocalProDataOutcome::Preserved => Some(json!({
            "command": "ctx pro",
            "reason": "restore_preserved_pro_data",
        })),
        LocalProDataOutcome::Absent => None,
    };
    json!({
        "schema_version": 1,
        "payload_type": "pro_uninstall",
        "uninstalled": true,
        "helper_removed": helper_removed,
        "local_pro_data": match data_outcome {
            LocalProDataOutcome::Absent => "absent",
            LocalProDataOutcome::Deleted => "deleted",
            LocalProDataOutcome::Preserved => "preserved",
        },
        "canonical_history_preserved": true,
        "next_action": next_action,
    })
}

const PRESERVED_DATA_MARKER_CONTENT: &[u8] = b"ctx-local-pro-data-preserved-v1\n";

fn preserved_data_marker_is_set(data_root: &Path) -> bool {
    let path = ProFilesystemLayout::new(data_root).preserved_data_marker_path();
    path.symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        && fs::read(path).is_ok_and(|content| content == PRESERVED_DATA_MARKER_CONTENT)
        && local_pro_graph_data_exists(data_root).unwrap_or(false)
}

fn write_preserved_data_marker(data_root: &Path) -> Result<()> {
    if !local_pro_graph_data_exists(data_root)? {
        bail!("invalid_request: cannot mark absent local Pro data as preserved");
    }
    let path = ProFilesystemLayout::new(data_root).preserved_data_marker_path();
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            verify_private_file(&path).context("verify local Pro data marker")?;
            if fs::read(&path).context("read local Pro data marker")?
                != PRESERVED_DATA_MARKER_CONTENT
            {
                bail!("invalid_request: local Pro data marker has invalid content");
            }
            return Ok(());
        }
        Ok(_) => bail!("invalid_request: local Pro data marker is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect local Pro data marker"),
    }
    let staged = path.with_extension("data-preserved.next");
    delete_one_file(&staged)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    let mut file = options
        .open(&staged)
        .context("create local Pro data marker")?;
    file.write_all(PRESERVED_DATA_MARKER_CONTENT)
        .context("write local Pro data marker")?;
    file.sync_all().context("sync local Pro data marker")?;
    restrict_private_file(&staged).context("protect local Pro data marker")?;
    verify_private_file(&staged).context("verify local Pro data marker")?;
    replace_file(&staged, &path).context("publish local Pro data marker")?;
    sync_parent_directory(&path)?;
    Ok(())
}

fn clear_preserved_data_marker(data_root: &Path) -> Result<()> {
    let path = ProFilesystemLayout::new(data_root).preserved_data_marker_path();
    let removed = delete_one_file(&path)?;
    let staged = path.with_extension("data-preserved.next");
    let removed_staged = delete_one_file(&staged)?;
    if removed || removed_staged {
        sync_parent_directory(&path)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct LocalProUninstallState {
    initialized: bool,
    cleanup_phase: bool,
    graph_data: bool,
    helper_files: bool,
    preserved_marker: bool,
    pending_materialization: bool,
    lifecycle_lock: bool,
}

impl LocalProUninstallState {
    const fn any_artifact(self) -> bool {
        self.initialized
            || self.cleanup_phase
            || self.graph_data
            || self.helper_files
            || self.preserved_marker
            || self.pending_materialization
            || self.lifecycle_lock
    }
}

fn inspect_local_pro_uninstall_state(data_root: &Path) -> Result<LocalProUninstallState> {
    let layout = ProFilesystemLayout::new(data_root);
    let helper_files =
        helper_file_candidates(data_root)?
            .iter()
            .try_fold(false, |present, path| {
                let exists = regular_file_exists(path, "local Pro helper file")?;
                Ok::<_, anyhow::Error>(present || exists)
            })?;
    Ok(LocalProUninstallState {
        initialized: local_pro_initialization_indicator_exists(data_root)?,
        cleanup_phase: local_pro_graph_key_cleanup_phase_exists(data_root)?,
        graph_data: local_pro_graph_data_exists(data_root)?,
        helper_files,
        preserved_marker: regular_file_exists(
            &layout.preserved_data_marker_path(),
            "local Pro data marker",
        )?,
        pending_materialization: pending_materialization::pending(data_root)?,
        lifecycle_lock: regular_file_exists(&layout.lifecycle_lock_path(), "Pro lifecycle lock")?,
    })
}

fn regular_file_exists(path: &Path, label: &str) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => bail!("invalid_request: {label} is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {label}")),
    }
}

fn delete_helper_files(data_root: &Path) -> Result<bool> {
    let target = default_helper_path(data_root);
    let candidates = helper_file_candidates(data_root)?;
    let mut removed = false;
    for candidate in &candidates {
        removed |= delete_one_file(candidate)?;
    }
    for candidate in &candidates {
        match candidate.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => bail!("invalid_request: failed to verify local Pro helper removal"),
            Err(error) => return Err(error).context("verify local Pro helper removal"),
        }
    }
    if let Some(bin) = target.parent() {
        let _ = fs::remove_dir(bin);
        if let Some(pro) = bin.parent() {
            let _ = fs::remove_dir(pro);
        }
    }
    Ok(removed)
}

fn helper_file_candidates(data_root: &Path) -> Result<[PathBuf; 12]> {
    let target = default_helper_path(data_root);
    Ok([
        target.clone(),
        install_marker_path(&target)?,
        previous_helper_path(&target)?,
        previous_marker_path(&target)?,
        transaction_journal_path(&target)?,
        transaction_journal_next_path(&target)?,
        transaction_helper_path(&target)?,
        transaction_marker_path(&target)?,
        publish_helper_path(&target)?,
        publish_marker_path(&target)?,
        rollback_helper_stage_path(&target)?,
        rollback_marker_stage_path(&target)?,
    ])
}

fn delete_one_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path).context("remove local Pro file")?;
            Ok(true)
        }
        Ok(_) => bail!("invalid_request: a local Pro file path is not a file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("inspect local Pro file"),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[derive(Default)]
    struct RecordingDeletion {
        calls: Vec<&'static str>,
        fail_graph_key_deletion: bool,
    }

    impl ProDeletionService for RecordingDeletion {
        fn delete_commercial_credentials(&mut self, _data_root: &Path) -> Result<()> {
            self.calls.push("delete_commercial_credentials");
            Ok(())
        }

        fn delete_graph_data(&mut self, data_root: &Path) -> Result<()> {
            self.calls.push("delete_graph_data");
            if self.fail_graph_key_deletion {
                bail!("key_store_unavailable: simulated deletion failure");
            }
            let graph = ctx_pro_host_protocol::ProFilesystemLayout::new(data_root).graph_path();
            if graph.exists() {
                fs::remove_file(graph)?;
            }
            Ok(())
        }

        fn finish_deletion(&mut self, data_root: &Path) -> Result<()> {
            if default_helper_path(data_root).exists() {
                bail!("invalid_request: cleanup phase finished before helper deletion");
            }
            self.calls.push("finish_deletion");
            Ok(())
        }
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let helper = default_helper_path(root.path());
        fs::create_dir_all(helper.parent().unwrap()).unwrap();
        ctx_history_core::platform_security::restrict_private_directory(
            &ProFilesystemLayout::new(root.path()).pro_root(),
        )
        .unwrap();
        write_local_pro_initialization_indicator(root.path()).unwrap();
        fs::write(&helper, b"helper").unwrap();
        let graph = ctx_pro_host_protocol::ProFilesystemLayout::new(root.path()).graph_path();
        let canonical = root.path().join("work.sqlite");
        fs::write(&graph, b"encrypted graph").unwrap();
        fs::write(&canonical, b"canonical history").unwrap();
        (root, helper, graph, canonical)
    }

    #[test]
    fn repair_required_setup_rejects_a_missing_artifact() {
        let error = setup_artifact(&SetupInstallation::RepairRequired, None)
            .err()
            .unwrap()
            .to_string();
        assert_eq!(
            error,
            "invalid_response: Pro setup returned no helper artifact for an install or repair"
        );
    }

    #[test]
    fn staged_activation_requires_exact_protocol_authorization_and_status() {
        let mut smoke = HelperSmoke {
            protocol_version: PROTOCOL_VERSION,
            protocol_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            helper_version: "0.1.0".to_owned(),
            capabilities: [Capability::EntitlementAuthorization, Capability::Status]
                .into_iter()
                .collect(),
        };
        validate_staged_helper(&smoke).unwrap();

        smoke
            .capabilities
            .remove(&Capability::EntitlementAuthorization);
        assert_eq!(
            validate_staged_helper(&smoke).unwrap_err().to_string(),
            "protocol_mismatch: staged Pro helper failed the activation smoke contract"
        );
    }

    #[test]
    fn ordinary_uninstall_preserves_local_pro_data_and_history() {
        let (root, helper, graph, canonical) = fixture();
        run_uninstall(root.path(), None, UninstallDataDisposition::Keep, true).unwrap();
        assert!(!helper.exists());
        assert_eq!(fs::read(graph).unwrap(), b"encrypted graph");
        assert_eq!(fs::read(canonical).unwrap(), b"canonical history");
        assert!(preserved_data_marker_is_set(root.path()));
        let status = lifecycle_status_json(root.path());
        assert_eq!(status["state"], "uninstalled_data_preserved");
        assert_eq!(
            status["next_action"]["reason"],
            "restore_preserved_pro_data"
        );
    }

    #[test]
    fn delete_data_confirms_key_before_removing_graph_and_credentials() {
        let (root, helper, graph, canonical) = fixture();
        let mut service = RecordingDeletion::default();
        run_uninstall(
            root.path(),
            Some(&mut service),
            UninstallDataDisposition::Delete,
            true,
        )
        .unwrap();
        assert_eq!(
            service.calls,
            [
                "delete_graph_data",
                "delete_commercial_credentials",
                "finish_deletion",
            ]
        );
        assert!(!helper.exists());
        assert!(!graph.exists());
        assert!(canonical.exists());
        assert_eq!(
            uninstall_payload(true, LocalProDataOutcome::Deleted),
            json!({
                "schema_version": 1,
                "payload_type": "pro_uninstall",
                "uninstalled": true,
                "helper_removed": true,
                "local_pro_data": "deleted",
                "canonical_history_preserved": true,
                "next_action": {
                    "command": "ctx pro",
                    "reason": "rebuild_pro_data",
                },
            })
        );
    }

    #[test]
    fn never_pro_missing_and_empty_roots_are_truthful_idempotent_noops() {
        for disposition in [
            UninstallDataDisposition::Delete,
            UninstallDataDisposition::Keep,
        ] {
            let parent = tempfile::tempdir().unwrap();
            let missing = parent.path().join("missing");
            let mut missing_service = RecordingDeletion::default();
            let value = run_uninstall(
                &missing,
                (disposition == UninstallDataDisposition::Delete)
                    .then_some(&mut missing_service as &mut dyn ProDeletionService),
                disposition,
                true,
            )
            .unwrap();
            assert_eq!(value["local_pro_data"], "absent");
            assert_eq!(value["helper_removed"], false);
            assert_eq!(value["next_action"], serde_json::Value::Null);
            assert!(missing_service.calls.is_empty());
            assert!(!missing.exists());

            let empty = tempfile::tempdir().unwrap();
            crate::identity::installation_id(empty.path()).unwrap();
            fs::write(empty.path().join("work.sqlite"), b"canonical history").unwrap();
            let mut empty_service = RecordingDeletion::default();
            let value = run_uninstall(
                empty.path(),
                (disposition == UninstallDataDisposition::Delete)
                    .then_some(&mut empty_service as &mut dyn ProDeletionService),
                disposition,
                true,
            )
            .unwrap();
            assert_eq!(value["local_pro_data"], "absent");
            assert_eq!(value["helper_removed"], false);
            assert_eq!(value["next_action"], serde_json::Value::Null);
            assert!(empty_service.calls.is_empty());
            assert!(!ProFilesystemLayout::new(empty.path()).pro_root().exists());
            assert_eq!(
                fs::read(empty.path().join("work.sqlite")).unwrap(),
                b"canonical history"
            );
        }
    }

    #[test]
    fn interrupted_artifact_fetch_retains_cleanup_evidence_until_verified_uninstall() {
        let root = tempfile::tempdir().unwrap();
        crate::identity::installation_id(root.path()).unwrap();
        let result = with_pro_initialization(root.path(), || -> Result<()> {
            assert!(local_pro_initialization_indicator_exists(root.path())?);
            bail!("artifact_download_failed: simulated interrupted fetch");
        });
        assert!(result
            .unwrap_err()
            .to_string()
            .starts_with("artifact_download_failed:"));
        assert!(local_pro_initialization_indicator_exists(root.path()).unwrap());

        let mut deletion = RecordingDeletion::default();
        let value = run_uninstall(
            root.path(),
            Some(&mut deletion),
            UninstallDataDisposition::Delete,
            true,
        )
        .unwrap();
        assert_eq!(
            deletion.calls,
            [
                "delete_graph_data",
                "delete_commercial_credentials",
                "finish_deletion",
            ]
        );
        assert_eq!(value["local_pro_data"], "absent");
        assert_eq!(value["next_action"], serde_json::Value::Null);
        assert!(!local_pro_initialization_indicator_exists(root.path()).unwrap());

        let mut repeated = RecordingDeletion::default();
        let value = run_uninstall(
            root.path(),
            Some(&mut repeated),
            UninstallDataDisposition::Delete,
            true,
        )
        .unwrap();
        assert!(repeated.calls.is_empty());
        assert_eq!(value["local_pro_data"], "absent");
    }

    #[test]
    fn interrupted_deletion_blocks_setup_and_keep_until_delete_retry() {
        let root = tempfile::tempdir().unwrap();
        let installation_id = crate::identity::installation_id(root.path()).unwrap();
        let layout = ProFilesystemLayout::new(root.path());
        fs::create_dir(layout.pro_root()).unwrap();
        ctx_history_core::platform_security::restrict_private_directory(&layout.pro_root())
            .unwrap();
        let phase = layout.pro_root().join(".ctx-pro.graph-key-cleanup.json");
        crate::pro::local_deletion::write_empty_graph_key_cleanup_phase_for_test(
            root.path(),
            &installation_id,
        )
        .unwrap();

        let mut operation_called = false;
        let error = with_pro_initialization(root.path(), || -> Result<()> {
            operation_called = true;
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().starts_with("key_store_unavailable:"));
        assert!(!operation_called);

        let error =
            run_uninstall(root.path(), None, UninstallDataDisposition::Keep, true).unwrap_err();
        assert!(error.to_string().starts_with("key_store_unavailable:"));
        assert!(phase.exists());
    }

    #[test]
    fn helper_without_graph_still_triggers_vault_cleanup_but_reports_absent() {
        let root = tempfile::tempdir().unwrap();
        crate::identity::installation_id(root.path()).unwrap();
        let layout = ProFilesystemLayout::new(root.path());
        fs::create_dir(layout.pro_root()).unwrap();
        ctx_history_core::platform_security::restrict_private_directory(&layout.pro_root())
            .unwrap();
        fs::create_dir(layout.bin_dir()).unwrap();
        fs::write(layout.helper_path(), b"helper").unwrap();

        let mut deletion = RecordingDeletion::default();
        let value = run_uninstall(
            root.path(),
            Some(&mut deletion),
            UninstallDataDisposition::Delete,
            true,
        )
        .unwrap();
        assert_eq!(
            deletion.calls,
            [
                "delete_graph_data",
                "delete_commercial_credentials",
                "finish_deletion",
            ]
        );
        assert_eq!(value["local_pro_data"], "absent");
        assert_eq!(value["helper_removed"], true);
        assert_eq!(value["next_action"], serde_json::Value::Null);
    }

    #[test]
    fn keep_data_marks_only_real_graph_data() {
        let root = tempfile::tempdir().unwrap();
        let pro_root = ProFilesystemLayout::new(root.path()).pro_root();
        fs::create_dir(&pro_root).unwrap();
        let stale_marker = ProFilesystemLayout::new(root.path()).preserved_data_marker_path();
        fs::write(&stale_marker, PRESERVED_DATA_MARKER_CONTENT).unwrap();

        assert!(!preserved_data_marker_is_set(root.path()));
        let value = run_uninstall(root.path(), None, UninstallDataDisposition::Keep, true).unwrap();
        assert_eq!(value["local_pro_data"], "absent");
        assert_eq!(value["next_action"], serde_json::Value::Null);
        assert!(!stale_marker.exists());
        assert!(!preserved_data_marker_is_set(root.path()));
    }

    #[test]
    fn failed_key_deletion_preserves_helper_graph_and_credentials() {
        let (root, helper, graph, canonical) = fixture();
        let mut service = RecordingDeletion {
            fail_graph_key_deletion: true,
            ..RecordingDeletion::default()
        };
        let error = run_uninstall(
            root.path(),
            Some(&mut service),
            UninstallDataDisposition::Delete,
            true,
        )
        .unwrap_err();
        assert!(error.to_string().starts_with("key_store_unavailable:"));
        assert_eq!(service.calls, ["delete_graph_data"]);
        assert!(helper.exists());
        assert!(graph.exists());
        assert!(canonical.exists());
    }

    fn pro_status(access_state: &str) -> ProStatus {
        let locked = access_state == "locked";
        ProStatus {
            schema_version: 1,
            installed: true,
            ready: !locked,
            materialized: true,
            helper_path: PathBuf::from("/redacted"),
            helper_version: Some("0.26.0".to_owned()),
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec!["status".to_owned()],
            error_code: locked.then(|| "entitlement_expired".to_owned()),
            access_state: Some(access_state.to_owned()),
            refresh_after_unix: Some(100),
            access_deadline_unix: Some(200),
            grace_deadline_unix: Some(300),
            setup_repairability: ProSetupRepairability::NotNeeded,
        }
    }

    #[test]
    fn lifecycle_status_keeps_readiness_separate_from_access_transitions() {
        for access_state in [
            "trial",
            "active",
            "canceling_paid",
            "offline_grace",
            "locked",
        ] {
            let value = lifecycle_status_value(pro_status(access_state), false);
            assert_eq!(value["access_state"], access_state);
            assert_eq!(value["refresh_after_unix"], 100);
            assert_eq!(value["access_deadline_unix"], 200);
            assert_eq!(value["grace_deadline_unix"], 300);
            assert_eq!(
                value["state"],
                if access_state == "locked" {
                    "locked"
                } else {
                    "ready"
                }
            );
        }
    }

    #[test]
    fn lifecycle_status_distinguishes_invalid_artifacts_from_never_installed() {
        let mut invalid = pro_status("active");
        invalid.installed = false;
        invalid.ready = false;
        invalid.materialized = false;
        invalid.helper_version = None;
        invalid.capabilities.clear();
        invalid.error_code = Some("invalid_response".to_owned());
        invalid.access_state = None;
        invalid.refresh_after_unix = None;
        invalid.access_deadline_unix = None;
        invalid.grace_deadline_unix = None;
        invalid.setup_repairability = ProSetupRepairability::Automated;

        let value = lifecycle_status_value(invalid.clone(), false);
        assert_eq!(value["state"], "repair_required");
        assert_eq!(value["installed"], false);
        assert_eq!(value["error_code"], "invalid_response");
        assert_eq!(value["next_action"]["command"], "ctx pro");
        assert_eq!(value["next_action"]["reason"], "helper_artifacts_invalid");

        invalid.setup_repairability = ProSetupRepairability::ManualDiagnosis;
        let value = lifecycle_status_value(invalid.clone(), false);
        assert_eq!(value["state"], "unavailable");
        assert_eq!(value["next_action"]["command"], serde_json::Value::Null);
        assert_eq!(value["next_action"]["reason"], "manual_diagnosis_required");

        invalid.error_code = Some("pro_not_installed".to_owned());
        invalid.setup_repairability = ProSetupRepairability::NotNeeded;
        let value = lifecycle_status_value(invalid, false);
        assert_eq!(value["state"], "not_setup");
        assert_eq!(value["next_action"]["reason"], "helper_missing");
    }

    #[test]
    fn manage_json_has_one_exact_nonsecret_access_shape() {
        let plan = ProManagePlan {
            portal_url: "https://billing.example.test/session".to_owned(),
            access_state: "canceling_paid".to_owned(),
            refresh_after_unix: Some(100),
            access_deadline_unix: Some(200),
            grace_deadline_unix: Some(300),
        };
        assert_eq!(
            manage_payload(&plan, false),
            json!({
                "schema_version": 1,
                "payload_type": "pro_manage",
                "portal_url": "https://billing.example.test/session",
                "browser_opened": false,
                "access_state": "canceling_paid",
                "refresh_after_unix": 100,
                "access_deadline_unix": 200,
                "grace_deadline_unix": 300,
            })
        );
        for access_state in ["trial", "active", "canceling_paid"] {
            validate_access_status(access_state, None, Some(200), None).unwrap();
        }
        validate_access_status("offline_grace", None, Some(200), Some(300)).unwrap();
        validate_access_status("locked", None, None, None).unwrap();
        assert!(validate_access_status("none", None, None, None).is_err());
    }

    #[test]
    fn manage_json_never_invokes_the_browser_opener() {
        struct ManageService;

        impl ProLifecycleService for ManageService {
            fn release_trust(&self) -> Result<ReleaseTrust> {
                bail!("unused")
            }

            fn setup(
                &mut self,
                _data_root: &Path,
                _installed_version: Option<&str>,
            ) -> Result<ProSetupPlan> {
                bail!("unused")
            }

            fn manage(&mut self, _data_root: &Path) -> Result<ProManagePlan> {
                Ok(ProManagePlan {
                    portal_url: "https://billing.example.test/session".to_owned(),
                    access_state: "active".to_owned(),
                    refresh_after_unix: Some(100),
                    access_deadline_unix: Some(200),
                    grace_deadline_unix: None,
                })
            }
        }

        let root = tempfile::tempdir().unwrap();
        let calls = Cell::new(0);
        let opener = |_: &str| {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let mut telemetry = ProLifecycleTelemetryV1::new(ProLifecycleOperationV1::Manage);
        run_manage_with_opener(
            root.path(),
            &mut ManageService,
            false,
            true,
            &mut telemetry,
            &opener,
        )
        .unwrap();
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn ordinary_uninstall_then_delete_and_repeated_delete_are_idempotent() {
        let (root, helper, graph, canonical) = fixture();
        run_uninstall(root.path(), None, UninstallDataDisposition::Keep, true).unwrap();
        assert!(!helper.exists());
        assert!(graph.exists());

        let mut first = RecordingDeletion::default();
        run_uninstall(
            root.path(),
            Some(&mut first),
            UninstallDataDisposition::Delete,
            true,
        )
        .unwrap();
        assert!(!graph.exists());
        assert!(!preserved_data_marker_is_set(root.path()));
        assert!(canonical.exists());

        let mut repeated = RecordingDeletion::default();
        let value = run_uninstall(
            root.path(),
            Some(&mut repeated),
            UninstallDataDisposition::Delete,
            true,
        )
        .unwrap();
        assert!(repeated.calls.is_empty());
        assert_eq!(value["local_pro_data"], "absent");
        assert_eq!(value["next_action"], serde_json::Value::Null);
        assert!(canonical.exists());
    }

    #[test]
    fn tty_uninstall_prompt_is_exact_and_defaults_to_delete() {
        let mut input = std::io::Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        assert_eq!(
            prompt_uninstall_data_disposition(&mut input, &mut output).unwrap(),
            UninstallDataDisposition::Delete
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("{UNINSTALL_DATA_PROMPT} ")
        );
    }

    #[test]
    fn tty_uninstall_prompt_can_preserve_data_and_reprompts_invalid_input() {
        let mut input = std::io::Cursor::new(b"maybe\nn\n".to_vec());
        let mut output = Vec::new();
        assert_eq!(
            prompt_uninstall_data_disposition(&mut input, &mut output).unwrap(),
            UninstallDataDisposition::Keep
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("{UNINSTALL_DATA_PROMPT} Please answer y or n.\n{UNINSTALL_DATA_PROMPT} ")
        );
    }
}

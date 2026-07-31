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

#[path = "lifecycle_commands/render.rs"]
mod render;
#[path = "lifecycle_commands/setup_replay.rs"]
mod setup_replay;

use super::{
    default_helper_path, install_marker_path, install_verified_bundle, previous_helper_path,
    previous_marker_path, publish_helper_path, publish_marker_path,
    reconcile_setup_installation_locked, replace_file, rollback_helper_stage_path,
    rollback_marker_stage_path, sync_parent_directory, transaction_helper_path,
    transaction_journal_next_path, transaction_journal_path, transaction_marker_path, Persistence,
    SetupInstallation,
};
use crate::pro::artifact_delivery::SetupArtifactBundle;
#[cfg(ctx_pro_qualification)]
use crate::pro::client::smoke_qualification_helper;
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
use crate::pro::referral::{parse_referral_codename_unchecked, ReferralCodename};
use crate::pro::setup_validation::{
    setup_artifact, validate_account_state, validate_staged_helper,
};
use crate::{
    analytics::{
        send_pro_operation, Outcome, ProAccessStateV1, ProHelperConnectionOutcomeV1,
        ProHostOperationV1, ProLifecycleOperationV1, ProLifecycleTelemetryV1,
        ProMaterializationTelemetryV1, ProReconcileOutcomeV1, ProUninstallDataDispositionV1,
    },
    output::JsonOutputFormat,
    pro::stable_error_code,
    ui::Ui,
};
use render::{
    browser_notice as render_browser_notice, manage as render_manage_human,
    setup as render_setup_human, uninstall as render_uninstall_human,
};
#[cfg(test)]
use setup_replay::reusable_setup_access_state;
use setup_replay::{record_helper_smoke, replay_completed_setup, write_setup_result};

#[derive(Debug, Args)]
pub(crate) struct ProArgs {
    #[command(subcommand)]
    command: Option<ProCommand>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
    #[arg(
        long,
        value_parser = parse_referral_codename_unchecked,
        help = "Apply a referral codename when starting a new anonymous trial"
    )]
    referral: Option<ReferralCodename>,
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
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
    #[arg(long, hide = true)]
    defer_materialization: bool,
    #[arg(long, hide = true)]
    trial_only: bool,
}

#[derive(Debug, Args)]
struct ProManageArgs {
    #[arg(long, help = "Print the portal URL without opening a browser")]
    no_open: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
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
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
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
    pub(crate) fn validate_invocation(&self) -> Result<()> {
        if let Some(referral) = &self.referral {
            referral.validate()?;
        }
        if self.referral.is_some() && self.command.is_some() {
            bail!("invalid_request: --referral is accepted only by bare `ctx pro`");
        }
        Ok(())
    }

    pub(crate) fn json_output(&self) -> bool {
        self.format.is_json()
            || match &self.command {
                Some(ProCommand::Setup(args)) => args.format.is_json(),
                Some(ProCommand::Manage(args)) => args.format.is_json(),
                Some(ProCommand::Uninstall(args)) => args.format.is_json(),
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
    pub(crate) artifact: Option<SetupArtifactBundle>,
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
    #[allow(clippy::too_many_arguments)]
    fn setup(
        &mut self,
        data_root: &Path,
        installed_version: Option<&str>,
        trial_only: bool,
        referral_codename: Option<&str>,
        ui: &mut Ui,
        human_output: bool,
        browser_enabled: bool,
    ) -> Result<ProSetupPlan>;
    fn manage(
        &mut self,
        data_root: &Path,
        ui: &mut Ui,
        human_output: bool,
        browser_enabled: bool,
    ) -> Result<ProManagePlan>;
}

/// Local-only destruction seam. Implementations may locate and delete native
/// vault records, but do not need commercial configuration or network access.
pub(crate) trait ProDeletionService {
    fn delete_graph_data(&mut self, data_root: &Path) -> Result<()>;
    fn delete_commercial_credentials(&mut self, data_root: &Path) -> Result<()>;
    fn finish_deletion(&mut self, data_root: &Path) -> Result<()>;
}

pub(crate) fn run_lifecycle(args: ProArgs, data_root: PathBuf, ui: &mut Ui) -> Result<()> {
    let started = Instant::now();
    let mut telemetry = ProLifecycleTelemetryV1::new(args.telemetry_operation());
    let result = run_lifecycle_inner(args, &data_root, &mut telemetry, ui);
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
    ui: &mut Ui,
) -> Result<()> {
    args.validate_invocation()?;
    let json_output = args.json_output();
    let defer_materialization = matches!(
        &args.command,
        Some(ProCommand::Setup(ProSetupArgs {
            defer_materialization: true,
            ..
        }))
    );
    let trial_only = matches!(
        &args.command,
        Some(ProCommand::Setup(ProSetupArgs {
            trial_only: true,
            ..
        }))
    );
    let ProArgs {
        command, referral, ..
    } = args;
    match command {
        None | Some(ProCommand::Setup(_)) => {
            crate::identity::installation_id(data_root)
                .context("key_store_unavailable: initialize local Pro installation identity")?;
            if replay_completed_setup(
                data_root,
                json_output,
                trial_only,
                referral.as_ref().map(ReferralCodename::as_str),
                telemetry,
                ui,
            )? {
                return Ok(());
            }
            let mut service = CommercialLifecycleService::production(data_root)?;
            run_setup(
                data_root,
                &mut service,
                json_output,
                defer_materialization,
                trial_only,
                referral.as_ref().map(ReferralCodename::as_str),
                telemetry,
                ui,
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
                ui,
            )
        }
        Some(ProCommand::Uninstall(args)) => {
            let disposition = uninstall_data_disposition(&args, json_output, ui)?;
            telemetry.uninstall_data = Some(match disposition {
                UninstallDataDisposition::Delete => ProUninstallDataDispositionV1::Delete,
                UninstallDataDisposition::Keep => ProUninstallDataDispositionV1::Preserve,
            });
            match disposition {
                UninstallDataDisposition::Delete => {
                    let mut service = LocalDeletionService::production();
                    run_uninstall(data_root, Some(&mut service), disposition, json_output, ui)
                        .map(|_| ())
                }
                UninstallDataDisposition::Keep => {
                    run_uninstall(data_root, None, disposition, json_output, ui).map(|_| ())
                }
            }
        }
    }
}

fn uninstall_data_disposition(
    args: &ProUninstallArgs,
    json_output: bool,
    ui: &mut Ui,
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
    prompt_uninstall_data_disposition(&mut io::stdin().lock(), ui)
}

fn prompt_uninstall_data_disposition(
    input: &mut impl io::BufRead,
    ui: &mut Ui,
) -> Result<UninstallDataDisposition> {
    render::prompt_uninstall_data_disposition(input, ui)
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

#[allow(clippy::too_many_arguments)]
fn run_setup(
    data_root: &Path,
    service: &mut dyn ProLifecycleService,
    json_output: bool,
    defer_materialization: bool,
    trial_only: bool,
    referral_codename: Option<&str>,
    telemetry: &mut ProLifecycleTelemetryV1,
    ui: &mut Ui,
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
        let plan = service.setup(
            data_root,
            installation.installed_version(),
            trial_only,
            referral_codename,
            ui,
            !json_output,
            !json_output,
        )?;
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
        match bundle {
            SetupArtifactBundle::Release(bundle) => {
                let smoke = record_helper_smoke(
                    smoke_helper_at_path(data_root, &bundle.artifact),
                    telemetry,
                )?;
                validate_staged_helper(&smoke)?;
                install_verified_bundle(&bundle, data_root, trust)?;
                telemetry.reconcile = if replacing_existing {
                    ProReconcileOutcomeV1::Updated
                } else {
                    ProReconcileOutcomeV1::Installed
                };
                true
            }
            #[cfg(ctx_pro_qualification)]
            SetupArtifactBundle::Qualification(bundle) => {
                let executable =
                    crate::pro::verified_executable::VerifiedHelperExecutable::open_qualification(
                        bundle,
                    )?;
                let smoke = record_helper_smoke(
                    smoke_qualification_helper(data_root, executable),
                    telemetry,
                )?;
                validate_staged_helper(&smoke)?;
                false
            }
            #[cfg(all(test, not(ctx_pro_qualification)))]
            SetupArtifactBundle::Qualification(bundle) => {
                let smoke = record_helper_smoke(
                    smoke_helper_at_path(data_root, bundle.verified_path()?),
                    telemetry,
                )?;
                validate_staged_helper(&smoke)?;
                false
            }
        }
    } else {
        false
    };
    if defer_materialization {
        return pending_materialization::defer_setup(
            data_root,
            &plan.account_state,
            helper_updated,
            json_output,
            ui,
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
    write_setup_result(
        data_root,
        &plan.account_state,
        helper_updated,
        report,
        json_output,
        ui,
    )
}

fn run_manage(
    data_root: &Path,
    service: &mut dyn ProLifecycleService,
    no_open: bool,
    json_output: bool,
    telemetry: &mut ProLifecycleTelemetryV1,
    ui: &mut Ui,
) -> Result<()> {
    run_manage_with_opener(
        data_root,
        service,
        no_open,
        json_output,
        telemetry,
        ui,
        &open_browser,
    )
}

fn run_manage_with_opener(
    data_root: &Path,
    service: &mut dyn ProLifecycleService,
    no_open: bool,
    json_output: bool,
    telemetry: &mut ProLifecycleTelemetryV1,
    ui: &mut Ui,
    opener: &dyn Fn(&str) -> Result<()>,
) -> Result<()> {
    let plan = with_pro_initialization(data_root, || {
        service.manage(data_root, ui, !json_output, !json_output && !no_open)
    })?;
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
        let document = render_manage_human(
            ui.stdout_context(),
            &plan,
            &usage_report,
            conversion_action.as_ref(),
            browser_opened,
        );
        ui.write_stdout(&document)?;
        if !no_open {
            let notice = render_browser_notice(
                ui.stderr_context(),
                browser_opened,
                "ctx Pro account management",
            );
            ui.write_stderr(&notice)?;
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
    ui: &mut Ui,
) -> Result<serde_json::Value> {
    let delete_data = disposition == UninstallDataDisposition::Delete;
    let target = default_helper_path(data_root);
    let initial_state = inspect_local_pro_uninstall_state(data_root)?;
    if !initial_state.data_artifact() && !initial_state.lifecycle_lock {
        return emit_uninstall_result(false, LocalProDataOutcome::Absent, json_output, ui);
    }
    let Some(_lifecycle_lock) = super::lifecycle_lock::LifecycleLock::acquire(&target, false)?
    else {
        return emit_uninstall_result(false, LocalProDataOutcome::Absent, json_output, ui);
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
        if inspect_local_pro_uninstall_state(data_root)?.data_artifact() {
            bail!("key_store_unavailable: local Pro data deletion could not be verified");
        }
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
    emit_uninstall_result(helper_removed, data_outcome, json_output, ui)
}

fn emit_uninstall_result(
    helper_removed: bool,
    data_outcome: LocalProDataOutcome,
    json_output: bool,
    ui: &mut Ui,
) -> Result<serde_json::Value> {
    let value = uninstall_payload(helper_removed, data_outcome);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        let document = render_uninstall_human(ui.stdout_context(), helper_removed, data_outcome);
        ui.write_stdout(&document)?;
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
    const fn data_artifact(self) -> bool {
        self.initialized
            || self.cleanup_phase
            || self.graph_data
            || self.helper_files
            || self.preserved_marker
            || self.pending_materialization
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
#[path = "lifecycle_commands/tests.rs"]
mod tests;

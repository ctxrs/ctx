use std::{
    io::{self, IsTerminal as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
#[cfg(test)]
use ctx_pro_host_protocol::ProFilesystemLayout;
#[cfg(test)]
use ctx_pro_host_protocol::{
    Capability, CoreProjectionCurrentness, MaterializedCoverage, ProOperation, RepositoryCoverage,
    PROTOCOL_FINGERPRINT, PROTOCOL_VERSION,
};
use serde_json::json;

#[path = "lifecycle_commands/render.rs"]
mod render;
#[path = "lifecycle_commands/setup_replay.rs"]
mod setup_replay;
#[path = "lifecycle_commands/uninstall.rs"]
mod uninstall;
#[path = "lifecycle_commands/validation.rs"]
mod validation;

use super::{
    default_helper_path, install_verified_bundle, reconcile_setup_installation_locked, Persistence,
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
#[cfg(test)]
use crate::pro::local_deletion::local_pro_initialization_indicator_exists;
use crate::pro::local_deletion::{
    local_pro_graph_key_cleanup_phase_exists, write_local_pro_initialization_indicator,
    LocalDeletionService,
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
    setup as render_setup_human,
};
#[cfg(test)]
use setup_replay::reusable_setup_access_state;
use setup_replay::{record_helper_smoke, replay_completed_setup, write_setup_result};
#[cfg(test)]
use std::fs;
use uninstall::{preserved_data_marker_is_set, run_uninstall};
#[cfg(test)]
use uninstall::{uninstall_payload, PRESERVED_DATA_MARKER_CONTENT};
use validation::{validate_access_status, validate_portal_url};

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
    let human_output = !args.json_output();
    let retry_command = render::human_retry_command(&args);
    let mut telemetry = ProLifecycleTelemetryV1::new(args.telemetry_operation());
    let result = run_lifecycle_inner(args, &data_root, &mut telemetry, ui);
    #[cfg(ctx_pro_test_helper)]
    let result = crate::pro::test_control::finish(result);
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
    crate::pro::human_result(result, human_output, retry_command, ui)
}

fn run_lifecycle_inner(
    args: ProArgs,
    data_root: &Path,
    telemetry: &mut ProLifecycleTelemetryV1,
    ui: &mut Ui,
) -> Result<()> {
    crate::pro::commercial_config::reject_test_control_outside_test_host()?;
    #[cfg(ctx_pro_test_helper)]
    crate::pro::test_control::prepare()?;
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
            #[cfg(ctx_pro_test_helper)]
            if let Some(mut service) = crate::pro::test_control::lifecycle_service()? {
                return run_setup(
                    data_root,
                    &mut service,
                    json_output,
                    defer_materialization,
                    trial_only,
                    referral.as_ref().map(ReferralCodename::as_str),
                    telemetry,
                    ui,
                );
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
            #[cfg(ctx_pro_test_helper)]
            if let Some(mut service) = crate::pro::test_control::lifecycle_service()? {
                return run_manage(
                    data_root,
                    &mut service,
                    args.no_open,
                    json_output,
                    telemetry,
                    ui,
                );
            }
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
    let helper_response_valid = !matches!(
        helper.error_code.as_deref(),
        Some("invalid_response" | "protocol_mismatch")
    );
    let ready = helper_response_valid && helper.ready;
    let materialized = helper_response_valid && helper.materialized;
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
            None if ready => ("ready", Some("ctx pro manage"), "billing_and_account"),
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
            None if materialized => ("not_blame_ready", None, "no_available_blame_operations"),
            Some(_) | None => ("unavailable", Some("ctx pro"), "helper_unavailable"),
        }
    };
    json!({
        "schema_version": 2,
        "payload_type": "pro_status",
        "state": state,
        "installed": helper.installed,
        "ready": ready,
        "materialized": materialized,
        "helper_version": helper.helper_version,
        "protocol_version": helper.protocol_version,
        "capabilities": helper.capabilities,
        "error_code": helper.error_code,
        "projection_currentness": helper.projection_currentness,
        "materialized_coverage": helper.materialized_coverage,
        "repository_coverage": helper.repository_coverage,
        "supported_operations": helper.supported_operations,
        "available_operations": helper.available_operations,
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
            #[cfg(ctx_pro_test_helper)]
            SetupArtifactBundle::TestControl(bundle) => {
                let helper = bundle.verified_path()?;
                let smoke =
                    record_helper_smoke(smoke_helper_at_path(data_root, &helper), telemetry)?;
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

fn open_browser(url: &str) -> Result<()> {
    #[cfg(ctx_pro_test_helper)]
    if let Some(result) = crate::pro::test_control::browser_result_if_active(url) {
        return result;
    }

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

#[cfg(test)]
#[path = "lifecycle_commands/tests.rs"]
mod tests;

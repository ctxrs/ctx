use std::path::PathBuf;

use anyhow::{anyhow, Result};
use ctx_cli_presentation::upgrade::{
    render_auto_mode, render_error, render_outcome, UpgradeArgs, UpgradeCommand,
};
use ctx_upgrade_engine::{
    managed_install_marker_for_current_exe, run_hosted_transaction,
    unmanaged_install_conversion_guidance, HostedTransactionArgs, ManagedInstallMarker,
    UpgradeOutcome, UpgradePolicy,
};

use crate::{
    analytics::{
        count_bucket, UpgradeChannel, UpgradeFailureKind, UpgradeStatus, UpgradeTelemetry,
    },
    config::AppConfig,
    output::JsonOutputFormat,
    ui::Ui,
};

use super::{config::set_auto_mode, ports};

mod status;
use status::render_status;

pub fn run(
    args: UpgradeArgs,
    data_root: PathBuf,
    config: AppConfig,
    telemetry: &mut UpgradeTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    validate_hidden_upgrade_protocol(&args)?;
    if let Some(action) = args.hosted_transaction {
        telemetry.suppress_event = true;
        return run_hosted_transaction(HostedTransactionArgs {
            action: action.into(),
            install_path: args
                .install_path
                .ok_or_else(|| anyhow!("hosted transaction missing --install-path"))?,
            attempt_id: args.attempt_id,
            marker_source: args.marker_source,
            ownership_source: args.ownership_source,
            binary_sha256: args.binary_sha256,
        });
    }
    #[cfg(windows)]
    if args.replacement_helper {
        let install_path = args
            .install_path
            .as_deref()
            .ok_or_else(|| anyhow!("replacement helper missing --install-path"))?;
        let attempt_id = args
            .attempt_id
            .as_deref()
            .ok_or_else(|| anyhow!("replacement helper missing --attempt-id"))?;
        telemetry.suppress_event = true;
        return ports::engine().run_replacement_helper(
            install_path,
            attempt_id,
            args.parent_pid.unwrap_or(0),
        );
    }
    if args.automatic_worker {
        telemetry.suppress_event = true;
        super::wait_for_invoking_parent(args.parent_pid, args.startup_receipt.as_deref())?;
        #[cfg(ctx_release_qualification)]
        if let Some(receipt) = std::env::var_os("CTX_AUTOMATIC_UPGRADE_WORKER_RECEIPT_FOR_TESTS") {
            if std::env::vars_os().any(|(key, _)| {
                key.to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with("CTX_RELEASE_")
            }) {
                return Err(anyhow!(
                    "automatic worker inherited release-authority environment"
                ));
            }
            std::fs::write(receipt, b"started\n")?;
            return Ok(());
        }
        let current = AppConfig::load(&data_root)?;
        if !super::effective_auto_upgrade_enabled(&current)
            || current.persistent_automatic_upgrade_driver_enabled()
        {
            return Ok(());
        }
        let engine = ports::engine();
        engine.prepare_data_root(&data_root)?;
        let startup_policy = crate::semantic::daemon_config_snapshot(&current);
        return engine.run_automatic(
            &ports::AUTOMATIC_POLICY,
            &ports::UPGRADE_OBSERVER,
            &data_root,
            &startup_policy,
        );
    }
    let engine = ports::engine();
    if let Err(error) = engine.prepare_data_root(&data_root) {
        // Analytics identity creation writes beneath the data root and would
        // otherwise repair an insecure pre-existing root after any upgrade
        // operation, including the read-only status command, rejected it.
        telemetry.suppress_event = true;
        return Err(error);
    }
    let policy = UpgradePolicy {
        channel: &config.upgrade.channel,
        interval: config.upgrade.interval,
        semantic_enabled: config.semantic_search_enabled(),
    };
    let result = (|| -> Result<()> {
        match &args.command {
            Some(UpgradeCommand::Check(check)) => {
                let channel = check.channel.as_deref().or(args.channel.as_deref());
                let outcome = engine.check(&data_root, policy, channel)?;
                insert_upgrade_outcome_analytics(telemetry, &outcome);
                render_outcome(
                    &outcome,
                    check.format.is_json() || args.format.is_json(),
                    ui,
                )
            }
            Some(UpgradeCommand::Status(status)) => {
                insert_upgrade_simple_analytics(telemetry, UpgradeStatus::StatusChecked);
                render_status(
                    &data_root,
                    &config,
                    status.format.is_json() || args.format.is_json(),
                    ui,
                )
            }
            Some(UpgradeCommand::Enable) => {
                require_managed_install_for_auto_upgrade()?;
                insert_upgrade_simple_analytics(telemetry, UpgradeStatus::AutoEnabled);
                set_auto_mode(&data_root, "apply")?;
                render_auto_mode(true, args.format.is_json(), ui)
            }
            Some(UpgradeCommand::Disable) => {
                insert_upgrade_simple_analytics(telemetry, UpgradeStatus::AutoDisabled);
                set_auto_mode(&data_root, "off")?;
                render_auto_mode(false, args.format.is_json(), ui)
            }
            None => {
                let outcome =
                    engine.apply(&data_root, policy, args.channel.as_deref(), args.dry_run)?;
                insert_upgrade_outcome_analytics(telemetry, &outcome);
                render_outcome(&outcome, args.format.is_json(), ui)
            }
        }
    })();
    if let Err(error) = &result {
        insert_upgrade_error_analytics(telemetry, error);
    }
    render_error(result, !args.json_output(), ui)
}

fn require_managed_install_for_auto_upgrade() -> Result<()> {
    match managed_install_marker_for_current_exe()? {
        ManagedInstallMarker::Valid(_) => Ok(()),
        ManagedInstallMarker::Absent => Err(anyhow!(
            "ctx is not installed by the hosted installer; {}",
            unmanaged_install_conversion_guidance()
        )),
        ManagedInstallMarker::Invalid { reason } => Err(anyhow!(reason)),
    }
}

fn validate_hidden_upgrade_protocol(args: &UpgradeArgs) -> Result<()> {
    let normal_options = args.command.is_some()
        || args.channel.is_some()
        || args.dry_run
        || args.format != JsonOutputFormat::Text;

    if args.hosted_transaction.is_some() {
        if normal_options
            || args.replacement_helper
            || args.automatic_worker
            || args.parent_pid.is_some()
            || args.startup_receipt.is_some()
        {
            return Err(anyhow!(
                "hosted transaction cannot be combined with upgrade options"
            ));
        }
        return Ok(());
    }

    if args.replacement_helper {
        #[cfg(not(windows))]
        return Err(anyhow!("replacement helper is available only on Windows"));

        #[cfg(windows)]
        {
            let valid_identity = args.install_path.is_some()
                && args.attempt_id.is_some()
                && args.parent_pid.is_some_and(|pid| pid != 0);
            if normal_options
                || !valid_identity
                || args.automatic_worker
                || args.startup_receipt.is_some()
                || args.marker_source.is_some()
                || args.ownership_source.is_some()
                || args.binary_sha256.is_some()
            {
                return Err(anyhow!("invalid replacement-helper process protocol"));
            }
            return Ok(());
        }
    }

    if args.automatic_worker {
        if normal_options
            || args.install_path.is_some()
            || args.attempt_id.is_some()
            || args.marker_source.is_some()
            || args.ownership_source.is_some()
            || args.binary_sha256.is_some()
        {
            return Err(anyhow!("invalid automatic-worker process protocol"));
        }
        #[cfg(windows)]
        if !args.parent_pid.is_some_and(|pid| pid != 0)
            || args.startup_receipt.as_deref().is_none_or(str::is_empty)
        {
            return Err(anyhow!("invalid automatic-worker process protocol"));
        }
        #[cfg(not(windows))]
        if args.parent_pid.is_some() || args.startup_receipt.is_some() {
            return Err(anyhow!("invalid automatic-worker process protocol"));
        }
        return Ok(());
    }

    if args.install_path.is_some()
        || args.attempt_id.is_some()
        || args.parent_pid.is_some()
        || args.startup_receipt.is_some()
        || args.marker_source.is_some()
        || args.ownership_source.is_some()
        || args.binary_sha256.is_some()
    {
        return Err(anyhow!(
            "hidden process options require a matching upgrade process role"
        ));
    }
    Ok(())
}

fn insert_upgrade_outcome_analytics(telemetry: &mut UpgradeTelemetry, outcome: &UpgradeOutcome) {
    telemetry.status = Some(UpgradeStatus::from_safe_summary(outcome.status()));
    telemetry.applied = Some(outcome.applied());
    telemetry.scheduled = Some(outcome.status() == "scheduled");
    telemetry.update_available = Some(false);
    telemetry.update_was_available = Some(false);
    telemetry.upgrade_attempt_id = outcome.attempt_id().map(str::to_owned);
    telemetry.managed_install = Some(false);
    telemetry.self_upgrade_allowed = Some(false);
    telemetry.auto_upgrade_allowed = Some(false);
    telemetry.warning_count = Some(count_bucket(outcome.warnings().len() as u64));
    if let Some(plan) = outcome.plan() {
        telemetry.channel = Some(UpgradeChannel::from_config(plan.channel()));
        telemetry.update_available = Some(if outcome.applied() {
            false
        } else {
            plan.update_available()
        });
        telemetry.update_was_available = Some(plan.update_available());
        telemetry.managed_install = Some(plan.managed());
        telemetry.self_upgrade_allowed = Some(plan.self_upgrade_allowed());
        telemetry.auto_upgrade_allowed = Some(plan.automatic_upgrade_allowed());
    }
}

fn insert_upgrade_simple_analytics(telemetry: &mut UpgradeTelemetry, status: UpgradeStatus) {
    telemetry.status = Some(status);
    telemetry.applied = Some(false);
    telemetry.scheduled = Some(false);
    telemetry.update_available = Some(false);
}

fn insert_upgrade_error_analytics(telemetry: &mut UpgradeTelemetry, error: &anyhow::Error) {
    telemetry.status = Some(UpgradeStatus::Failed);
    telemetry.applied = Some(false);
    telemetry.scheduled = Some(false);
    telemetry.failure_kind = Some(upgrade_failure_kind(error));
}

fn upgrade_failure_kind(error: &anyhow::Error) -> UpgradeFailureKind {
    let text = format!("{error:#}").to_ascii_lowercase();
    if text.contains("upgrade lock") {
        UpgradeFailureKind::LockFailed
    } else if text.contains("not installed by the hosted installer")
        || text.contains("install marker")
        || text.contains("unmanaged")
    {
        UpgradeFailureKind::UnmanagedInstall
    } else if text.contains("metadata") && text.contains("download") {
        UpgradeFailureKind::MetadataFetch
    } else if text.contains("signature") {
        UpgradeFailureKind::SignatureVerify
    } else if text.contains("metadata") {
        UpgradeFailureKind::MetadataInvalid
    } else if text.contains("checksum") || text.contains("sha") {
        UpgradeFailureKind::ArtifactVerify
    } else if text.contains("download") {
        UpgradeFailureKind::ArtifactDownload
    } else if text.contains("does not allow") {
        UpgradeFailureKind::PolicyDisallowed
    } else {
        UpgradeFailureKind::ApplyFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse_upgrade(args: &[&str]) -> UpgradeArgs {
        let cli =
            crate::cli::Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
                .unwrap();
        let crate::cli::CommandRoot::Upgrade(args) = cli.command else {
            panic!("expected upgrade command");
        };
        args
    }

    #[test]
    fn normal_upgrade_rejects_orphaned_hidden_process_fields() {
        let args = parse_upgrade(&["upgrade", "--parent-pid", "42"]);
        assert!(validate_hidden_upgrade_protocol(&args).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_rejects_windows_worker_protocol_fields_and_replacement_role() {
        let worker = parse_upgrade(&[
            "upgrade",
            "--automatic-worker",
            "--parent-pid",
            "42",
            "--startup-receipt",
            "receipt",
        ]);
        assert!(validate_hidden_upgrade_protocol(&worker).is_err());

        let replacement = parse_upgrade(&[
            "upgrade",
            "--replacement-helper",
            "--install-path",
            "/tmp/ctx",
            "--attempt-id",
            "ua_test",
            "--parent-pid",
            "42",
        ]);
        assert!(validate_hidden_upgrade_protocol(&replacement).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_worker_requires_a_complete_parent_receipt_protocol() {
        let incomplete = parse_upgrade(&["upgrade", "--automatic-worker"]);
        assert!(validate_hidden_upgrade_protocol(&incomplete).is_err());

        let complete = parse_upgrade(&[
            "upgrade",
            "--automatic-worker",
            "--parent-pid",
            "42",
            "--startup-receipt",
            "receipt",
        ]);
        assert!(validate_hidden_upgrade_protocol(&complete).is_ok());
    }
}

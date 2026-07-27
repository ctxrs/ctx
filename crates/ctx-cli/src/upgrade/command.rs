use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use crate::{
    analytics::{
        count_bucket, AutoUpgradeSpawnStatus, AutoUpgradeTelemetry, UpgradeChannel,
        UpgradeFailureKind, UpgradeStatus, UpgradeTelemetry,
    },
    config::AppConfig,
    net,
};

use super::install::{
    apply_artifact, current_install_path, install_marker_for_plan,
    read_verified_install_marker_for_current_exe, recover_interrupted_install, ApplyResult,
};
use super::metadata::{
    metadata_signature_url, metadata_url, parse_release_metadata, validate_artifact_url,
    verify_artifact_sha, verify_metadata_signature,
};
use super::path::{path_diagnostics, PathDiagnostics};
use super::state::{
    append_upgrade_log, atomic_write_json, now_unix_s, read_json_file, set_auto_mode,
    should_check_now, write_state_checked, write_state_error, UpgradeLock, STATE_FILE,
};
use super::{env_flag, platform_key, version_gt, UpgradePlan};

const RELEASE_METADATA_MAX_BYTES: usize = 1024 * 1024;
const RELEASE_METADATA_SIGNATURE_MAX_BYTES: usize = 64 * 1024;
const RELEASE_ARTIFACT_MAX_BYTES: usize = 128 * 1024 * 1024;
const RELEASE_ONNXRUNTIME_ARTIFACT_MAX_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Debug, Args)]
pub struct UpgradeArgs {
    #[command(subcommand)]
    pub command: Option<UpgradeCommand>,
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long, hide = true)]
    pub background: bool,
}

#[derive(Debug, Subcommand)]
pub enum UpgradeCommand {
    #[command(about = "Check whether a newer ctx release is available")]
    Check(UpgradeCheckArgs),
    #[command(about = "Show local upgrade state")]
    Status(UpgradeStatusArgs),
    #[command(about = "Enable managed background auto-upgrades")]
    Enable,
    #[command(about = "Disable background auto-upgrades")]
    Disable,
}

#[derive(Debug, Args)]
pub struct UpgradeCheckArgs {
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UpgradeStatusArgs {
    #[arg(long)]
    pub json: bool,
}

impl UpgradeArgs {
    pub fn json_output(&self) -> bool {
        self.json
            || matches!(
                &self.command,
                Some(UpgradeCommand::Check(args)) if args.json
            )
            || matches!(
                &self.command,
                Some(UpgradeCommand::Status(args)) if args.json
            )
            || self.background
    }

    pub fn background(&self) -> bool {
        self.background
    }

    pub fn operation(&self) -> &'static str {
        match &self.command {
            Some(UpgradeCommand::Check(_)) => "check",
            Some(UpgradeCommand::Status(_)) => "status",
            Some(UpgradeCommand::Enable) => "enable",
            Some(UpgradeCommand::Disable) => "disable",
            None => "apply",
        }
    }
}

#[derive(Debug, Clone)]
struct UpgradeOutcome {
    command: &'static str,
    status: &'static str,
    message: String,
    plan: Option<UpgradePlan>,
    applied: bool,
    dry_run: bool,
    warnings: Vec<String>,
}

impl UpgradeOutcome {
    fn json(&self) -> Value {
        let plan = self.plan.as_ref();
        json!({
            "schema_version": 1,
            "command": self.command,
            "ok": true,
            "status": self.status,
            "message": self.message,
            "current_version": plan.map(|plan| plan.current_version.as_str()),
            "latest_version": plan.map(|plan| plan.latest_version.as_str()),
            "update_available": plan.map(|plan| plan.update_available).unwrap_or(false),
            "channel": plan.map(|plan| plan.channel.as_str()),
            "platform": plan.map(|plan| plan.platform.as_str()),
            "metadata_url": plan.map(|plan| plan.metadata_url.as_str()),
            "artifact_url": plan.map(|plan| plan.artifact_url.as_str()),
            "install_path": plan.map(|plan| plan.install_path.display().to_string()),
            "managed": plan.map(|plan| plan.managed).unwrap_or(false),
            "path": plan.map(|plan| plan.path.json()),
            "applied": self.applied,
            "dry_run": self.dry_run,
            "warnings": self.warnings,
        })
    }
}

pub fn run(
    args: UpgradeArgs,
    data_root: PathBuf,
    config: AppConfig,
    telemetry: &mut UpgradeTelemetry,
) -> Result<()> {
    if args.background {
        return run_background_apply(&data_root, &config, telemetry);
    }
    let result = (|| -> Result<()> {
        match &args.command {
            Some(UpgradeCommand::Check(check)) => {
                let channel = check.channel.as_deref().or(args.channel.as_deref());
                let outcome = check_upgrade(&data_root, &config, channel, "upgrade_check")?;
                insert_upgrade_outcome_analytics(telemetry, &outcome);
                render_outcome(&outcome, check.json || args.json)
            }
            Some(UpgradeCommand::Status(status)) => {
                insert_upgrade_simple_analytics(telemetry, UpgradeStatus::StatusChecked);
                render_status(&data_root, status.json || args.json)
            }
            Some(UpgradeCommand::Enable) => {
                insert_upgrade_simple_analytics(telemetry, UpgradeStatus::AutoEnabled);
                set_auto_mode(&data_root, "apply")
            }
            Some(UpgradeCommand::Disable) => {
                insert_upgrade_simple_analytics(telemetry, UpgradeStatus::AutoDisabled);
                set_auto_mode(&data_root, "off")
            }
            None => {
                let outcome = apply_upgrade(
                    &data_root,
                    &config,
                    args.channel.as_deref(),
                    args.dry_run,
                    false,
                )?;
                insert_upgrade_outcome_analytics(telemetry, &outcome);
                render_outcome(&outcome, args.json)
            }
        }
    })();
    if let Err(error) = &result {
        insert_upgrade_error_analytics(telemetry, error);
    }
    result
}

pub fn maybe_spawn_auto_upgrade(data_root: &Path, config: &AppConfig) -> AutoUpgradeTelemetry {
    let channel = UpgradeChannel::from_config(&config.upgrade.channel);
    if !auto_mode_is_apply(config) {
        return auto_upgrade_telemetry(channel, AutoUpgradeSpawnStatus::AutoDisabled, false, false);
    }
    if env_flag("CI") {
        return auto_upgrade_telemetry(channel, AutoUpgradeSpawnStatus::Ci, false, false);
    }
    if env_flag("CTX_UPGRADE_BACKGROUND_CHILD") {
        return auto_upgrade_telemetry(
            channel,
            AutoUpgradeSpawnStatus::BackgroundChild,
            false,
            false,
        );
    }
    if !should_check_now(data_root, config.upgrade.interval) {
        return auto_upgrade_telemetry(channel, AutoUpgradeSpawnStatus::NotDue, false, false);
    }
    if read_verified_install_marker_for_current_exe().is_err() {
        return auto_upgrade_telemetry(channel, AutoUpgradeSpawnStatus::MarkerInvalid, true, false);
    }
    let Ok(current_exe) = current_install_path() else {
        return auto_upgrade_telemetry(
            channel,
            AutoUpgradeSpawnStatus::CurrentExeError,
            true,
            false,
        );
    };
    let mut command = Command::new(current_exe);
    command.arg("--data-root").arg(data_root);
    let spawn_result = command
        .args(["upgrade", "--background"])
        .env("CTX_UPGRADE_BACKGROUND_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if spawn_result.is_ok() {
        auto_upgrade_telemetry(channel, AutoUpgradeSpawnStatus::Spawned, true, true)
    } else {
        auto_upgrade_telemetry(channel, AutoUpgradeSpawnStatus::SpawnFailed, true, false)
    }
}

fn auto_upgrade_telemetry(
    channel: UpgradeChannel,
    status: AutoUpgradeSpawnStatus,
    due: bool,
    spawned: bool,
) -> AutoUpgradeTelemetry {
    AutoUpgradeTelemetry {
        due,
        spawned,
        status,
        channel,
    }
}

fn run_background_apply(
    data_root: &Path,
    config: &AppConfig,
    telemetry: &mut UpgradeTelemetry,
) -> Result<()> {
    if !auto_mode_is_apply(config) || env_flag("CI") {
        insert_upgrade_simple_analytics(telemetry, UpgradeStatus::Skipped);
        return Ok(());
    }
    match apply_upgrade(data_root, config, None, false, true) {
        Ok(outcome) => {
            insert_upgrade_outcome_analytics(telemetry, &outcome);
            append_upgrade_log(data_root, &outcome.message);
            Ok(())
        }
        Err(error) => {
            let message = format!("{error:#}");
            let _ = write_state_error(data_root, &message);
            append_upgrade_log(data_root, &format!("background upgrade failed: {message}"));
            insert_upgrade_error_analytics(telemetry, &error);
            Err(error)
        }
    }
}

fn insert_upgrade_outcome_analytics(telemetry: &mut UpgradeTelemetry, outcome: &UpgradeOutcome) {
    telemetry.status = Some(UpgradeStatus::from_safe_summary(outcome.status));
    telemetry.applied = Some(outcome.applied);
    telemetry.scheduled = Some(outcome.status == "scheduled");
    telemetry.update_available = Some(false);
    telemetry.managed_install = Some(false);
    telemetry.self_upgrade_allowed = Some(false);
    telemetry.auto_upgrade_allowed = Some(false);
    telemetry.warning_count = Some(count_bucket(outcome.warnings.len() as u64));
    if let Some(plan) = &outcome.plan {
        telemetry.channel = Some(UpgradeChannel::from_config(&plan.channel));
        telemetry.update_available = Some(plan.update_available);
        telemetry.managed_install = Some(plan.managed);
        telemetry.self_upgrade_allowed = Some(plan.metadata.self_upgrade_allowed);
        telemetry.auto_upgrade_allowed = Some(plan.metadata.auto_upgrade_allowed);
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

fn auto_mode_is_apply(config: &AppConfig) -> bool {
    config.upgrade.auto.eq_ignore_ascii_case("apply")
}

fn check_upgrade(
    data_root: &Path,
    config: &AppConfig,
    channel_override: Option<&str>,
    command: &'static str,
) -> Result<UpgradeOutcome> {
    let plan = build_upgrade_plan(config, channel_override, false)?;
    write_state_checked(data_root, &plan, "checked")?;
    let status = if plan.update_available {
        "available"
    } else {
        "up_to_date"
    };
    let message = if plan.update_available {
        format!(
            "ctx {} is available (current {}, channel {}).",
            plan.latest_version, plan.current_version, plan.channel
        )
    } else {
        format!("ctx {} is up to date.", plan.current_version)
    };
    let warnings = plan.warnings.clone();
    Ok(UpgradeOutcome {
        command,
        status,
        message,
        plan: Some(plan),
        applied: false,
        dry_run: false,
        warnings,
    })
}

fn apply_upgrade(
    data_root: &Path,
    config: &AppConfig,
    channel_override: Option<&str>,
    dry_run: bool,
    background: bool,
) -> Result<UpgradeOutcome> {
    fs::create_dir_all(data_root)?;
    #[allow(unused_mut)]
    let mut upgrade_lock = match UpgradeLock::acquire(data_root) {
        Ok(lock) => lock,
        Err(error) if background => {
            append_upgrade_log(data_root, &format!("background upgrade skipped: {error}"));
            let _ = write_background_skip_state(data_root, "locked");
            return Ok(UpgradeOutcome {
                command: "upgrade",
                status: "locked",
                message: "another ctx upgrade is already running".to_owned(),
                plan: None,
                applied: false,
                dry_run,
                warnings: vec!["another ctx upgrade is already running".to_owned()],
            });
        }
        Err(error) => return Err(error),
    };
    if recover_interrupted_install(data_root)? {
        append_upgrade_log(data_root, "recovered interrupted install transaction");
        if cfg!(debug_assertions) && env_flag("CTX_UPGRADE_STOP_AFTER_RECOVERY_FOR_TESTS") {
            return Err(anyhow!("stopped after interrupted install recovery"));
        }
    }
    let plan = build_upgrade_plan(config, channel_override, true)?;
    if !plan.update_available {
        write_state_checked(data_root, &plan, "up_to_date")?;
        let warnings = plan.warnings.clone();
        return Ok(UpgradeOutcome {
            command: "upgrade",
            status: "up_to_date",
            message: format!("ctx {} is already installed.", plan.current_version),
            plan: Some(plan),
            applied: false,
            dry_run,
            warnings,
        });
    }
    if !plan.metadata.self_upgrade_allowed {
        return Err(anyhow!(
            "release {} does not allow self-upgrade",
            plan.latest_version
        ));
    }
    if plan.metadata.onnxruntime.is_none() {
        return Err(anyhow!(
            "release {} is newer than this ctx build but has no complete ONNX Runtime sidecar metadata; refusing a downgrade-compatible legacy update",
            plan.latest_version
        ));
    }
    if background && !plan.metadata.auto_upgrade_allowed {
        return Err(anyhow!(
            "release {} does not allow background auto-upgrade",
            plan.latest_version
        ));
    }
    if dry_run {
        write_state_checked(data_root, &plan, "dry_run")?;
        let warnings = plan.warnings.clone();
        return Ok(UpgradeOutcome {
            command: "upgrade",
            status: "dry_run",
            message: format!(
                "ctx {} would upgrade to {}.",
                plan.current_version, plan.latest_version
            ),
            plan: Some(plan),
            applied: false,
            dry_run: true,
            warnings,
        });
    }
    let bytes = net::get_bytes_limited(&plan.artifact_url, RELEASE_ARTIFACT_MAX_BYTES)
        .with_context(|| format!("download {}", plan.artifact_url))?;
    verify_artifact_sha(&bytes, &plan.artifact_sha256)?;
    let runtime_bytes = match (
        plan.metadata.onnxruntime.as_ref(),
        plan.onnxruntime_artifact_url(),
    ) {
        (Some(runtime), Some(runtime_url)) => {
            let bytes =
                net::get_bytes_limited(&runtime_url, RELEASE_ONNXRUNTIME_ARTIFACT_MAX_BYTES)
                    .with_context(|| format!("download {runtime_url}"))?;
            verify_artifact_sha(&bytes, &runtime.sha256)?;
            Some(bytes)
        }
        (None, None) => None,
        _ => return Err(anyhow!("incomplete ONNX Runtime upgrade plan")),
    };
    let apply_result = apply_artifact(
        &plan,
        &bytes,
        runtime_bytes.as_deref(),
        data_root,
        upgrade_lock.path(),
    )?;
    let mut warnings = plan.warnings.clone();
    if let ApplyResult::Scheduled { helper_pid } = apply_result {
        #[cfg(windows)]
        upgrade_lock.transfer_to(helper_pid)?;
        #[cfg(not(windows))]
        let _ = helper_pid;
        record_post_apply_state(data_root, &plan, "scheduled", &mut warnings);
        return Ok(UpgradeOutcome {
            command: "upgrade",
            status: "scheduled",
            message: format!(
                "scheduled ctx {} -> {} at {}; replacement will finish after this process exits",
                plan.current_version,
                plan.latest_version,
                plan.install_path.display()
            ),
            plan: Some(plan),
            applied: false,
            dry_run: false,
            warnings,
        });
    }
    record_post_apply_state(data_root, &plan, "applied", &mut warnings);
    Ok(UpgradeOutcome {
        command: "upgrade",
        status: "applied",
        message: format!(
            "upgraded ctx {} -> {} at {}",
            plan.current_version,
            plan.latest_version,
            plan.install_path.display()
        ),
        plan: Some(plan),
        applied: true,
        dry_run: false,
        warnings,
    })
}

fn record_post_apply_state(
    data_root: &Path,
    plan: &UpgradePlan,
    status: &str,
    warnings: &mut Vec<String>,
) {
    if let Err(error) = write_state_checked(data_root, plan, status) {
        warnings.push(format!(
            "upgrade {status}, but local upgrade state could not be written: {error:#}"
        ));
    }
}

fn write_background_skip_state(data_root: &Path, status: &str) -> Result<()> {
    let body = json!({
        "schema_version": 1,
        "status": status,
        "checked_at": utc_now(),
        "last_checked_unix_s": now_unix_s(),
        "update_available": false,
    });
    atomic_write_json(&data_root.join(STATE_FILE), &body)
}

fn build_upgrade_plan(
    config: &AppConfig,
    channel_override: Option<&str>,
    require_managed: bool,
) -> Result<UpgradePlan> {
    let current_version = env!("CARGO_PKG_VERSION").to_owned();
    let platform = platform_key()?.to_owned();
    let channel = channel_override
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(config.upgrade.channel.as_str())
        .to_owned();
    let mut warnings = Vec::new();
    let marker = install_marker_for_plan(
        require_managed,
        &platform,
        &channel,
        &current_version,
        &mut warnings,
    )?;
    let managed = warnings.is_empty();
    let path = path_diagnostics(&marker.install_path, &current_version);
    warnings.extend(path.warnings.clone());
    let metadata_url = metadata_url(config, &channel);
    let signature_url = metadata_signature_url(&metadata_url);
    let metadata_bytes = net::get_bytes_limited(&metadata_url, RELEASE_METADATA_MAX_BYTES)
        .with_context(|| format!("download release metadata {metadata_url}"))?;
    let signature_bytes =
        net::get_bytes_limited(&signature_url, RELEASE_METADATA_SIGNATURE_MAX_BYTES)
            .with_context(|| format!("download release metadata signature {signature_url}"))?;
    verify_metadata_signature(&metadata_bytes, &signature_bytes)?;
    let metadata = parse_release_metadata(&metadata_bytes, &platform, &channel)?;
    let artifact_url = format!(
        "{}/{}",
        metadata.base_url.trim_end_matches('/'),
        metadata.artifact
    );
    validate_artifact_url(&metadata.base_url, &metadata.artifact)?;
    if let Some(runtime) = &metadata.onnxruntime {
        validate_artifact_url(&metadata.base_url, &runtime.artifact)?;
    }
    let update_available = version_gt(&metadata.version, &current_version);
    Ok(UpgradePlan {
        current_version,
        latest_version: metadata.version.clone(),
        channel,
        platform,
        metadata_url,
        artifact_url,
        artifact_sha256: metadata.sha256.clone(),
        install_path: marker.install_path.clone(),
        update_available,
        managed,
        warnings,
        path,
        metadata,
    })
}

fn render_outcome(outcome: &UpgradeOutcome, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(&outcome.json())?);
    } else {
        println!("{}", outcome.message);
        for warning in &outcome.warnings {
            eprintln!("warning: {warning}");
        }
    }
    Ok(())
}

fn render_status(data_root: &Path, json_output: bool) -> Result<()> {
    let state = read_json_file(&data_root.join(STATE_FILE)).unwrap_or_else(|| {
        json!({
            "schema_version": 1,
            "status": "never_checked"
        })
    });
    let current_version = env!("CARGO_PKG_VERSION");
    let current_exe = current_install_path().ok();
    let path_diagnostics = current_exe
        .as_ref()
        .map(|path| path_diagnostics(path, current_version));
    let marker_result = read_verified_install_marker_for_current_exe();
    let state = reconcile_scheduled_state(state, marker_result.as_ref().ok());
    let marker = marker_result
        .map(|marker| {
            json!({
                "managed": true,
                "install_path": marker.install_path,
                "platform": marker.platform,
                "channel": marker.channel,
                "version": marker.version,
                "sha256": marker.sha256,
            })
        })
        .unwrap_or_else(|error| {
            json!({
                "managed": false,
                "reason": error.to_string()
            })
        });
    let pro = crate::pro::lifecycle_status_json(data_root);
    let value = json!({
        "schema_version": 1,
        "command": "upgrade_status",
        "current_version": current_version,
        "state": state,
        "install": marker,
        "path": path_diagnostics.as_ref().map(PathDiagnostics::json),
        "warnings": path_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.warnings.clone())
            .unwrap_or_default(),
        "pro": pro,
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if marker.get("managed").and_then(Value::as_bool) == Some(true) {
        let status = state
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        println!("ctx upgrade status: {status}");
        if status == "error" {
            if let Some(error) = state.get("error").and_then(Value::as_str) {
                println!("{error}");
            }
        }
        if let Some(path) = marker.get("install_path").and_then(Value::as_str) {
            println!("install: {path}");
        }
        if let Some(diagnostics) = &path_diagnostics {
            println!("current_exe: {}", diagnostics.current_exe.display());
            if let Some(first) = diagnostics.entries.first() {
                println!("path_ctx: {}", first.path.display());
            }
            for warning in &diagnostics.warnings {
                eprintln!("warning: {warning}");
            }
        }
        if pro["installed"].as_bool() == Some(true) {
            println!(
                "pro: {} (helper updates through `ctx pro`)",
                pro["state"].as_str().unwrap_or("unavailable")
            );
        }
    } else {
        println!("ctx upgrade status: unmanaged install");
        if let Some(reason) = marker.get("reason").and_then(Value::as_str) {
            println!("{reason}");
        }
        if let Some(diagnostics) = &path_diagnostics {
            println!("current_exe: {}", diagnostics.current_exe.display());
            if let Some(first) = diagnostics.entries.first() {
                println!("path_ctx: {}", first.path.display());
            }
            for warning in &diagnostics.warnings {
                eprintln!("warning: {warning}");
            }
        }
    }
    Ok(())
}

fn reconcile_scheduled_state(
    mut state: Value,
    marker: Option<&super::install::InstallMarker>,
) -> Value {
    if state.get("status").and_then(Value::as_str) != Some("scheduled") {
        return state;
    }
    let Some(marker) = marker else {
        return state;
    };
    let Some(latest_version) = state.get("latest_version").and_then(Value::as_str) else {
        return state;
    };
    let Some(install_path) = state.get("install_path").and_then(Value::as_str) else {
        return state;
    };
    if Path::new(install_path) != marker.install_path {
        return state;
    }
    if marker.version == latest_version {
        if let Some(object) = state.as_object_mut() {
            object.insert("status".to_owned(), Value::String("applied".to_owned()));
            object.insert("applied".to_owned(), Value::Bool(true));
            object.insert(
                "reconciled_from".to_owned(),
                Value::String("scheduled".to_owned()),
            );
        }
    }
    state
}

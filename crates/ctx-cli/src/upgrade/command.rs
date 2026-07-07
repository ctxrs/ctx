use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use crate::{analytics, analytics::AnalyticsProperties, config::AppConfig, net};

use super::install::{
    apply_artifact, current_install_path, install_marker_for_plan,
    read_verified_install_marker_for_current_exe, write_install_marker_after_upgrade, ApplyResult,
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

    pub fn mode(&self) -> &'static str {
        if self.background {
            "auto"
        } else {
            "manual"
        }
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
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    if args.background {
        return run_background_apply(&data_root, &config, analytics_properties);
    }
    let result = (|| -> Result<()> {
        match &args.command {
            Some(UpgradeCommand::Check(check)) => {
                let channel = check.channel.as_deref().or(args.channel.as_deref());
                let outcome = check_upgrade(&data_root, &config, channel, "upgrade_check")?;
                insert_upgrade_outcome_analytics(analytics_properties, &outcome);
                render_outcome(&outcome, check.json || args.json)
            }
            Some(UpgradeCommand::Status(status)) => {
                insert_upgrade_simple_analytics(analytics_properties, "status_checked");
                render_status(&data_root, status.json || args.json)
            }
            Some(UpgradeCommand::Enable) => {
                insert_upgrade_simple_analytics(analytics_properties, "auto_enabled");
                set_auto_mode(&data_root, "apply")
            }
            Some(UpgradeCommand::Disable) => {
                insert_upgrade_simple_analytics(analytics_properties, "auto_disabled");
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
                insert_upgrade_outcome_analytics(analytics_properties, &outcome);
                render_outcome(&outcome, args.json)
            }
        }
    })();
    if let Err(error) = &result {
        insert_upgrade_error_analytics(analytics_properties, error);
    }
    result
}

pub fn maybe_spawn_auto_upgrade(
    data_root: &Path,
    config: &AppConfig,
    json_output: bool,
    analytics_properties: &mut AnalyticsProperties,
) {
    analytics::insert_bool(analytics_properties, "auto_upgrade_probe", true);
    analytics::insert_bool(analytics_properties, "auto_upgrade_due", false);
    analytics::insert_bool(analytics_properties, "auto_upgrade_spawned", false);
    analytics::insert_str(
        analytics_properties,
        "upgrade_channel",
        upgrade_channel_bucket(&config.upgrade.channel),
    );
    if json_output {
        analytics::insert_str(
            analytics_properties,
            "auto_upgrade_spawn_status",
            "json_output",
        );
        return;
    }
    if !auto_mode_is_apply(config) {
        analytics::insert_str(
            analytics_properties,
            "auto_upgrade_spawn_status",
            "auto_disabled",
        );
        return;
    }
    if env_flag("CI") {
        analytics::insert_str(analytics_properties, "auto_upgrade_spawn_status", "ci");
        return;
    }
    if env_flag("CTX_UPGRADE_OFF") || env_flag("CTX_DISABLE_AUTO_UPGRADE") {
        analytics::insert_str(
            analytics_properties,
            "auto_upgrade_spawn_status",
            "env_disabled",
        );
        return;
    }
    if env_flag("CTX_UPGRADE_BACKGROUND_CHILD") {
        analytics::insert_str(
            analytics_properties,
            "auto_upgrade_spawn_status",
            "background_child",
        );
        return;
    }
    if !should_check_now(data_root, config.upgrade.interval) {
        analytics::insert_str(analytics_properties, "auto_upgrade_spawn_status", "not_due");
        return;
    }
    analytics::insert_bool(analytics_properties, "auto_upgrade_due", true);
    if read_verified_install_marker_for_current_exe().is_err() {
        analytics::insert_str(
            analytics_properties,
            "auto_upgrade_spawn_status",
            "marker_invalid",
        );
        return;
    }
    let Ok(current_exe) = current_install_path() else {
        analytics::insert_str(
            analytics_properties,
            "auto_upgrade_spawn_status",
            "current_exe_error",
        );
        return;
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
        analytics::insert_bool(analytics_properties, "auto_upgrade_spawned", true);
        analytics::insert_str(analytics_properties, "auto_upgrade_spawn_status", "spawned");
    } else {
        analytics::insert_str(
            analytics_properties,
            "auto_upgrade_spawn_status",
            "spawn_failed",
        );
    }
}

fn run_background_apply(
    data_root: &Path,
    config: &AppConfig,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    if !auto_mode_is_apply(config) || env_flag("CI") {
        insert_upgrade_simple_analytics(analytics_properties, "skipped");
        return Ok(());
    }
    match apply_upgrade(data_root, config, None, false, true) {
        Ok(outcome) => {
            insert_upgrade_outcome_analytics(analytics_properties, &outcome);
            append_upgrade_log(data_root, &outcome.message);
            Ok(())
        }
        Err(error) => {
            let message = format!("{error:#}");
            let _ = write_state_error(data_root, &message);
            append_upgrade_log(data_root, &format!("background upgrade failed: {message}"));
            insert_upgrade_error_analytics(analytics_properties, &error);
            Err(error)
        }
    }
}

fn insert_upgrade_outcome_analytics(
    analytics_properties: &mut AnalyticsProperties,
    outcome: &UpgradeOutcome,
) {
    analytics::insert_str(analytics_properties, "upgrade_status", outcome.status);
    analytics::insert_bool(analytics_properties, "upgrade_applied", outcome.applied);
    analytics::insert_bool(
        analytics_properties,
        "upgrade_scheduled",
        outcome.status == "scheduled",
    );
    analytics::insert_bool(analytics_properties, "update_available", false);
    analytics::insert_bool(analytics_properties, "managed_install", false);
    analytics::insert_bool(analytics_properties, "self_upgrade_allowed", false);
    analytics::insert_bool(analytics_properties, "auto_upgrade_allowed", false);
    analytics::insert_count_bucket(
        analytics_properties,
        "upgrade_warning_count_bucket",
        outcome.warnings.len() as u64,
    );
    if let Some(plan) = &outcome.plan {
        analytics::insert_str(
            analytics_properties,
            "upgrade_channel",
            upgrade_channel_bucket(&plan.channel),
        );
        analytics::insert_bool(
            analytics_properties,
            "update_available",
            plan.update_available,
        );
        analytics::insert_bool(analytics_properties, "managed_install", plan.managed);
        analytics::insert_bool(
            analytics_properties,
            "self_upgrade_allowed",
            plan.metadata.self_upgrade_allowed,
        );
        analytics::insert_bool(
            analytics_properties,
            "auto_upgrade_allowed",
            plan.metadata.auto_upgrade_allowed,
        );
    }
}

fn insert_upgrade_simple_analytics(
    analytics_properties: &mut AnalyticsProperties,
    status: &'static str,
) {
    analytics::insert_str(analytics_properties, "upgrade_status", status);
    analytics::insert_bool(analytics_properties, "upgrade_applied", false);
    analytics::insert_bool(analytics_properties, "upgrade_scheduled", false);
    analytics::insert_bool(analytics_properties, "update_available", false);
}

fn insert_upgrade_error_analytics(
    analytics_properties: &mut AnalyticsProperties,
    error: &anyhow::Error,
) {
    analytics::insert_str(analytics_properties, "upgrade_status", "failed");
    analytics::insert_bool(analytics_properties, "upgrade_applied", false);
    analytics::insert_bool(analytics_properties, "upgrade_scheduled", false);
    analytics::insert_str(
        analytics_properties,
        "upgrade_failure_kind",
        upgrade_failure_kind(error),
    );
}

fn upgrade_failure_kind(error: &anyhow::Error) -> &'static str {
    let text = format!("{error:#}").to_ascii_lowercase();
    if text.contains("upgrade lock") {
        "lock_failed"
    } else if text.contains("not installed by the hosted installer")
        || text.contains("install marker")
        || text.contains("unmanaged")
    {
        "unmanaged_install"
    } else if text.contains("metadata") && text.contains("download") {
        "metadata_fetch"
    } else if text.contains("signature") {
        "signature_verify"
    } else if text.contains("metadata") {
        "metadata_invalid"
    } else if text.contains("checksum") || text.contains("sha") {
        "artifact_verify"
    } else if text.contains("download") {
        "artifact_download"
    } else if text.contains("does not allow") {
        "policy_disallowed"
    } else {
        "apply_failed"
    }
}

fn auto_mode_is_apply(config: &AppConfig) -> bool {
    config.upgrade.auto.eq_ignore_ascii_case("apply")
}

fn upgrade_channel_bucket(channel: &str) -> &'static str {
    match channel.trim().to_ascii_lowercase().as_str() {
        "stable" => "stable",
        "beta" => "beta",
        "canary" => "canary",
        "dev" => "dev",
        _ => "other",
    }
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
    let _lock = match UpgradeLock::acquire(data_root) {
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
    let apply_result = apply_artifact(&plan, &bytes)?;
    let warnings = plan.warnings.clone();
    if apply_result == ApplyResult::Scheduled {
        write_state_checked(data_root, &plan, "scheduled")?;
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
    write_install_marker_after_upgrade(&plan)?;
    write_state_checked(data_root, &plan, "applied")?;
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
    let marker = read_verified_install_marker_for_current_exe()
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
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if marker.get("managed").and_then(Value::as_bool) == Some(true) {
        let status = state
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        println!("ctx upgrade status: {status}");
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

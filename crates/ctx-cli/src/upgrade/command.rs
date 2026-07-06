use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::{config::AppConfig, net};

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
    append_upgrade_log, read_json_file, set_auto_mode, should_check_now, write_state_checked,
    write_state_error, UpgradeLock, STATE_FILE,
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

pub fn run(args: UpgradeArgs, data_root: PathBuf, config: AppConfig) -> Result<()> {
    if args.background {
        return run_background_apply(&data_root, &config);
    }
    match &args.command {
        Some(UpgradeCommand::Check(check)) => {
            let channel = check.channel.as_deref().or(args.channel.as_deref());
            let outcome = check_upgrade(&data_root, &config, channel, "upgrade_check")?;
            render_outcome(&outcome, check.json || args.json)
        }
        Some(UpgradeCommand::Status(status)) => render_status(&data_root, status.json || args.json),
        Some(UpgradeCommand::Enable) => set_auto_mode(&data_root, "apply"),
        Some(UpgradeCommand::Disable) => set_auto_mode(&data_root, "off"),
        None => {
            let outcome = apply_upgrade(
                &data_root,
                &config,
                args.channel.as_deref(),
                args.dry_run,
                false,
            )?;
            render_outcome(&outcome, args.json)
        }
    }
}

pub fn maybe_spawn_auto_upgrade(data_root: &Path, config: &AppConfig, json_output: bool) {
    if json_output || !auto_mode_is_apply(config) || env_flag("CI") || env_flag("CTX_UPGRADE_OFF") {
        return;
    }
    if env_flag("CTX_DISABLE_AUTO_UPGRADE") || env_flag("CTX_UPGRADE_BACKGROUND_CHILD") {
        return;
    }
    if !should_check_now(data_root, config.upgrade.interval) {
        return;
    }
    if read_verified_install_marker_for_current_exe().is_err() {
        return;
    }
    let Ok(current_exe) = current_install_path() else {
        return;
    };
    let mut command = Command::new(current_exe);
    command.arg("--data-root").arg(data_root);
    let _ = command
        .args(["upgrade", "--background"])
        .env("CTX_UPGRADE_BACKGROUND_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn run_background_apply(data_root: &Path, config: &AppConfig) -> Result<()> {
    if !auto_mode_is_apply(config) || env_flag("CI") {
        return Ok(());
    }
    match apply_upgrade(data_root, config, None, false, true) {
        Ok(outcome) => {
            append_upgrade_log(data_root, &outcome.message);
            Ok(())
        }
        Err(error) => {
            let message = format!("{error:#}");
            let _ = write_state_error(data_root, &message);
            append_upgrade_log(data_root, &format!("background upgrade failed: {message}"));
            Ok(())
        }
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
    let _lock = match UpgradeLock::acquire(data_root) {
        Ok(lock) => lock,
        Err(error) if background => {
            append_upgrade_log(data_root, &format!("background upgrade skipped: {error}"));
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

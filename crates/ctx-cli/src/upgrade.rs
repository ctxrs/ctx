use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clap::{Args, Subcommand};
use ctx_history_core::utc_now;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{config::AppConfig, net};

const STATE_FILE: &str = "upgrade-state.json";
const LOCK_FILE: &str = "upgrade.lock";
const LOG_FILE: &str = "logs/upgrade.log";
const DEFAULT_METADATA_PUBLIC_KEY_PEM: &str = r#"-----BEGIN RSA PUBLIC KEY-----
MIIBigKCAYEAyBPNIx3H/NwWlN9CPHY5kOEe9kQEshOJEMpv3Atq086H1FWqliTm
3BCWiO4s/89wNMn11Pla2JetCWNiWsbxm3BIxCd1o6cq8y9ur6Zk1RGOQBLQgqhF
m5BpcTTavhtlc3FdV2KSm2UU1IEJAiFXJyMlbgmf3tXfO8Cji/3mG11rWCXfnEzX
Jmig5/WWA21ZgsafPJGH9ow7FsLok5G1kvOeVDXcv0gzmxWH+2O40kCGWo7BK7P/
2DPD2GbXc81Mf6S7vWi7CeFiBeGH8EGZ6MgBM0UnAFEqtx/WvY47O+LHzFrGlJTp
ss3xlxsSQOTmXDJdOzmQVi04GkbOtBEl+dIyYsxZGusLBMGDqkZekO4Z5LvqA8zH
t4JAElZCs8SGTlV70MSlnyZb5/rkKx9kMvb7YjuYbY6vnN5Pp3P7gMhOKehP+62U
80cgyj1m6Sk5bByrs54ne2mM+cwNXXgKp5UntmkefDcfKP7MmISy93U/kg3fWojE
/a+X6TNV/k5fAgMBAAE=
-----END RSA PUBLIC KEY-----"#;
const RELEASE_BASE_PREFIX: &str = "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/";

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
struct InstallMarker {
    install_path: PathBuf,
    platform: String,
    channel: String,
    version: String,
    sha256: String,
}

#[derive(Debug, Clone)]
struct ReleaseMetadata {
    version: String,
    base_url: String,
    artifact: String,
    sha256: String,
    source_commit: Option<String>,
    published_at: Option<String>,
    self_upgrade_allowed: bool,
    auto_upgrade_allowed: bool,
    store_schema_version: Option<String>,
}

#[derive(Debug, Clone)]
struct UpgradePlan {
    current_version: String,
    latest_version: String,
    channel: String,
    platform: String,
    metadata_url: String,
    artifact_url: String,
    artifact_sha256: String,
    install_path: PathBuf,
    update_available: bool,
    managed: bool,
    warnings: Vec<String>,
    path: PathDiagnostics,
    metadata: ReleaseMetadata,
}

#[derive(Debug, Clone)]
struct PathDiagnostics {
    current_exe: PathBuf,
    entries: Vec<PathDiagnosticEntry>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct PathDiagnosticEntry {
    path: PathBuf,
    version: Option<String>,
    current: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyResult {
    Applied,
    Scheduled,
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

impl PathDiagnostics {
    fn json(&self) -> Value {
        json!({
            "current_exe": self.current_exe.display().to_string(),
            "first_ctx": self.entries.first().map(|entry| entry.path.display().to_string()),
            "entries": self.entries.iter().map(|entry| {
                json!({
                    "path": entry.path.display().to_string(),
                    "version": entry.version.as_deref(),
                    "current": entry.current,
                })
            }).collect::<Vec<_>>(),
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
    let bytes = net::get_bytes(&plan.artifact_url)
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
    let metadata_bytes = net::get_bytes(&metadata_url)
        .with_context(|| format!("download release metadata {metadata_url}"))?;
    let signature_bytes = net::get_bytes(&signature_url)
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

fn set_auto_mode(data_root: &Path, mode: &str) -> Result<()> {
    fs::create_dir_all(data_root)?;
    let config_path = data_root.join(crate::config::CONFIG_FILE);
    let existing = fs::read_to_string(&config_path).unwrap_or_default();
    let next = set_toml_section_value(&existing, "upgrade", "auto", &format!("\"{mode}\""));
    fs::write(&config_path, next).with_context(|| format!("write {}", config_path.display()))?;
    println!("ctx background auto-upgrade {mode}");
    Ok(())
}

fn set_toml_section_value(input: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines = Vec::new();
    let mut in_section = false;
    let mut saw_section = false;
    let mut wrote_key = false;
    for raw in input.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_section && !wrote_key {
                lines.push(format!("{key} = {value}"));
                wrote_key = true;
            }
            in_section = trimmed == format!("[{section}]");
            saw_section |= in_section;
            lines.push(raw.to_owned());
            continue;
        }
        if in_section
            && (trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")))
        {
            lines.push(format!("{key} = {value}"));
            wrote_key = true;
        } else {
            lines.push(raw.to_owned());
        }
    }
    if saw_section {
        if in_section && !wrote_key {
            lines.push(format!("{key} = {value}"));
        }
    } else {
        if !lines.is_empty() && lines.last().is_some_and(|line| !line.is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("[{section}]"));
        lines.push(format!("{key} = {value}"));
    }
    lines.join("\n") + "\n"
}

fn metadata_url(config: &AppConfig, channel: &str) -> String {
    if let Ok(url) = env::var("CTX_RELEASE_METADATA_URL") {
        if !url.trim().is_empty() {
            return url;
        }
    }
    format!(
        "{}/releases/{channel}/ctx-release-metadata.env",
        config.upgrade.functions_base.trim_end_matches('/')
    )
}

fn metadata_signature_url(metadata_url: &str) -> String {
    env::var("CTX_RELEASE_METADATA_SIGNATURE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{metadata_url}.sig"))
}

fn parse_release_metadata(
    bytes: &[u8],
    platform: &str,
    expected_channel: &str,
) -> Result<ReleaseMetadata> {
    let text = std::str::from_utf8(bytes).context("release metadata is not UTF-8")?;
    let metadata = parse_metadata_map(text)?;
    let value = |key: &str| metadata_value(&metadata, key);
    let schema = value("CTX_RELEASE_SCHEMA_VERSION")
        .ok_or_else(|| anyhow!("metadata missing CTX_RELEASE_SCHEMA_VERSION"))?;
    if schema != "1" {
        return Err(anyhow!("unsupported release metadata schema: {schema}"));
    }
    let channel = value("CTX_RELEASE_CHANNEL").unwrap_or_else(|| expected_channel.to_owned());
    if channel != expected_channel {
        return Err(anyhow!(
            "metadata channel {channel} does not match requested channel {expected_channel}"
        ));
    }
    let version = value("CTX_RELEASE_VERSION")
        .ok_or_else(|| anyhow!("metadata missing CTX_RELEASE_VERSION"))?;
    let base_url = value("CTX_RELEASE_BASE_URL")
        .ok_or_else(|| anyhow!("metadata missing CTX_RELEASE_BASE_URL"))?;
    let platform_key = platform.replace('-', "_");
    let artifact = value(&format!("CTX_RELEASE_ARTIFACT_{platform_key}"))
        .ok_or_else(|| anyhow!("metadata missing artifact for {platform}"))?;
    let sha256 = value(&format!("CTX_RELEASE_SHA256_{platform_key}"))
        .ok_or_else(|| anyhow!("metadata missing checksum for {platform}"))?;
    validate_sha256(&sha256)?;
    Ok(ReleaseMetadata {
        version,
        base_url,
        artifact,
        sha256,
        source_commit: value("CTX_RELEASE_SOURCE_COMMIT"),
        published_at: value("CTX_RELEASE_PUBLISHED_AT"),
        self_upgrade_allowed: metadata_bool(&metadata, "CTX_RELEASE_SELF_UPGRADE_ALLOWED", false)?,
        auto_upgrade_allowed: metadata_bool(&metadata, "CTX_RELEASE_AUTO_UPGRADE_ALLOWED", false)?,
        store_schema_version: value("CTX_RELEASE_STORE_SCHEMA_VERSION"),
    })
}

fn parse_metadata_map(text: &str) -> Result<BTreeMap<String, String>> {
    let mut metadata = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if metadata
            .insert(key.to_owned(), value.trim_end_matches('\r').to_owned())
            .is_some()
        {
            return Err(anyhow!("metadata contains duplicate key {key}"));
        }
    }
    Ok(metadata)
}

fn metadata_value(metadata: &BTreeMap<String, String>, key: &str) -> Option<String> {
    metadata.get(key).cloned()
}

fn metadata_bool(metadata: &BTreeMap<String, String>, key: &str, default: bool) -> Result<bool> {
    let Some(value) = metadata_value(metadata, key) else {
        return Ok(default);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        _ => Err(anyhow!("metadata {key} must be a boolean")),
    }
}

fn verify_metadata_signature(metadata: &[u8], signature: &[u8]) -> Result<()> {
    if cfg!(debug_assertions) && env_flag("CTX_RELEASE_SKIP_SIGNATURE_VERIFY_FOR_TESTS") {
        return Ok(());
    }
    let der = public_key_der()?;
    let signature_text = std::str::from_utf8(signature)
        .context("metadata signature is not UTF-8 base64")?
        .trim();
    let signature_bytes = BASE64
        .decode(signature_text)
        .context("metadata signature is not base64")?;
    let key =
        ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, der);
    key.verify(metadata, &signature_bytes)
        .map_err(|_| anyhow!("metadata signature verification failed"))
}

fn public_key_der() -> Result<Vec<u8>> {
    let pem = env::var("CTX_RELEASE_METADATA_PUBLIC_KEY_PEM")
        .unwrap_or_else(|_| DEFAULT_METADATA_PUBLIC_KEY_PEM.to_owned());
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .map(str::trim)
        .collect();
    BASE64
        .decode(body)
        .context("decode release metadata public key")
}

fn validate_artifact_url(base_url: &str, artifact: &str) -> Result<()> {
    if !base_url.starts_with("https://") && !base_url.starts_with("file://") {
        return Err(anyhow!("metadata base URL must be HTTPS"));
    }
    if !base_url.starts_with(RELEASE_BASE_PREFIX)
        && !base_url.starts_with("file://")
        && !env_flag("CTX_ALLOW_CUSTOM_RELEASE_BASE_URL")
    {
        return Err(anyhow!(
            "metadata base URL must be under {RELEASE_BASE_PREFIX}"
        ));
    }
    if artifact.contains('/') || artifact.contains('\\') || artifact.contains("..") {
        return Err(anyhow!("unsafe artifact name: {artifact}"));
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!("checksum is not a SHA-256 hex digest"));
    }
    if value == "0000000000000000000000000000000000000000000000000000000000000000" {
        return Err(anyhow!("checksum is a placeholder"));
    }
    Ok(())
}

fn verify_artifact_sha(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = sha256_hex(bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(anyhow!(
            "artifact checksum mismatch: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn apply_artifact(plan: &UpgradePlan, bytes: &[u8]) -> Result<ApplyResult> {
    let parent = plan.install_path.parent().ok_or_else(|| {
        anyhow!(
            "install path has no parent: {}",
            plan.install_path.display()
        )
    })?;
    fs::create_dir_all(parent)?;
    let unique = format!("{}.{}", std::process::id(), now_unix_s());
    let staged = parent.join(format!(".ctx-upgrade-{unique}.new"));
    {
        let mut file = fs::File::create(&staged)
            .with_context(|| format!("create staged artifact {}", staged.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    make_executable(&staged, &plan.install_path)?;
    verify_staged_version(&staged, &plan.latest_version)?;
    let result = replace_binary(&staged, plan)?;
    sync_parent(parent);
    Ok(result)
}

fn verify_staged_version(staged: &Path, expected_version: &str) -> Result<()> {
    let output = Command::new(staged)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("run staged ctx {}", staged.display()))?;
    if !output.status.success() {
        return Err(anyhow!("staged ctx --version failed"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains(expected_version) {
        return Err(anyhow!(
            "staged ctx version mismatch: expected {expected_version}, got {}",
            stdout.trim()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(staged: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(target)
        .map(|metadata| metadata.permissions().mode())
        .unwrap_or(0o755)
        | 0o111;
    fs::set_permissions(staged, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_staged: &Path, _target: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn replace_binary(staged: &Path, plan: &UpgradePlan) -> Result<ApplyResult> {
    let target = &plan.install_path;
    let backup = backup_path(target);
    if target.exists() {
        fs::copy(target, &backup)
            .with_context(|| format!("backup ctx binary to {}", backup.display()))?;
    }
    fs::rename(staged, target)?;
    Ok(ApplyResult::Applied)
}

#[cfg(windows)]
fn replace_binary(staged: &Path, plan: &UpgradePlan) -> Result<ApplyResult> {
    let target = &plan.install_path;
    let backup = backup_path(target);
    let script = staged.with_extension("ps1");
    let marker_tmp = staged.with_extension("install.json.tmp");
    let marker_path = install_marker_path(target);
    write_install_marker_to(&marker_tmp, plan)?;
    let parent = std::process::id();
    let body = format!(
        r#"$ErrorActionPreference = 'Stop'
$parent = {parent}
$staged = {staged}
$target = {target}
$backup = {backup}
$markerTmp = {marker_tmp}
$markerPath = {marker_path}
for ($i = 0; $i -lt 80; $i++) {{
  $p = Get-Process -Id $parent -ErrorAction SilentlyContinue
  if ($null -eq $p) {{ break }}
  Start-Sleep -Milliseconds 250
}}
if (Test-Path -LiteralPath $target) {{
  [System.IO.File]::Replace($staged, $target, $backup, $true)
}} else {{
  Move-Item -LiteralPath $staged -Destination $target -Force
}}
if (Test-Path -LiteralPath $markerTmp) {{
  Move-Item -LiteralPath $markerTmp -Destination $markerPath -Force
}}
Remove-Item -LiteralPath $MyInvocation.MyCommand.Path -Force
"#,
        staged = ps_single_quote(staged),
        target = ps_single_quote(target),
        backup = ps_single_quote(&backup),
        marker_tmp = ps_single_quote(&marker_tmp),
        marker_path = ps_single_quote(&marker_path),
    );
    fs::write(&script, body)?;
    Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn Windows ctx replacement helper")?;
    Ok(ApplyResult::Scheduled)
}

#[cfg(not(any(unix, windows)))]
fn replace_binary(_staged: &Path, _plan: &UpgradePlan) -> Result<ApplyResult> {
    Err(anyhow!(
        "self-upgrade replacement is unsupported on this platform"
    ))
}

#[cfg(windows)]
fn ps_single_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn backup_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("ctx");
    target.with_file_name(format!("{name}.previous"))
}

#[cfg(unix)]
fn sync_parent(parent: &Path) {
    let _ = fs::File::open(parent).and_then(|file| file.sync_all());
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) {}

fn read_install_marker_for_current_exe() -> Result<InstallMarker> {
    let path = current_install_path()?;
    let marker_path = install_marker_path(&path);
    let value = read_json_file(&marker_path)
        .ok_or_else(|| anyhow!("ctx is not installed by the hosted installer; reinstall with curl -fsSL https://ctx.rs/install | sh to enable managed upgrades"))?;
    let manager = value
        .get("manager")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if manager != "ctx-hosted-installer" {
        return Err(anyhow!(
            "ctx install marker has unsupported manager: {manager}"
        ));
    }
    let install_path = value
        .get("install_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("ctx install marker missing install_path"))?;
    if install_path != path {
        return Err(anyhow!(
            "ctx install marker path mismatch: marker {}, running {}",
            install_path.display(),
            path.display()
        ));
    }
    Ok(InstallMarker {
        install_path,
        platform: string_field(&value, "platform")?,
        channel: string_field(&value, "channel")?,
        version: string_field(&value, "version")?,
        sha256: string_field(&value, "sha256")?,
    })
}

fn read_verified_install_marker_for_current_exe() -> Result<InstallMarker> {
    let marker = read_install_marker_for_current_exe()?;
    verify_install_marker(&marker, platform_key()?)?;
    Ok(marker)
}

fn install_marker_for_plan(
    require_managed: bool,
    platform: &str,
    channel: &str,
    current_version: &str,
    warnings: &mut Vec<String>,
) -> Result<InstallMarker> {
    match read_install_marker_for_current_exe() {
        Ok(marker) => match verify_install_marker(&marker, platform) {
            Ok(()) => Ok(marker),
            Err(error) if require_managed => Err(error),
            Err(error) => {
                warnings.push(error.to_string());
                fallback_install_marker(platform, channel, current_version)
            }
        },
        Err(error) if require_managed => Err(error),
        Err(error) => {
            warnings.push(error.to_string());
            fallback_install_marker(platform, channel, current_version)
        }
    }
}

fn fallback_install_marker(
    platform: &str,
    channel: &str,
    current_version: &str,
) -> Result<InstallMarker> {
    Ok(InstallMarker {
        install_path: current_install_path()?,
        platform: platform.to_owned(),
        channel: channel.to_owned(),
        version: current_version.to_owned(),
        sha256: current_binary_sha().unwrap_or_default(),
    })
}

fn verify_install_marker(marker: &InstallMarker, platform: &str) -> Result<()> {
    if marker.platform != platform {
        return Err(anyhow!(
            "ctx install marker platform mismatch: marker {}, current {platform}",
            marker.platform
        ));
    }
    let actual = current_binary_sha()?;
    if !marker.sha256.eq_ignore_ascii_case(&actual) {
        return Err(anyhow!(
            "ctx install marker hash mismatch; reinstall with curl -fsSL https://ctx.rs/install | sh"
        ));
    }
    Ok(())
}

fn write_install_marker_after_upgrade(plan: &UpgradePlan) -> Result<()> {
    let marker_path = install_marker_path(&plan.install_path);
    write_install_marker_to(&marker_path, plan)
}

fn write_install_marker_to(marker_path: &Path, plan: &UpgradePlan) -> Result<()> {
    let body = json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "install_path": plan.install_path,
        "platform": plan.platform,
        "channel": plan.channel,
        "version": plan.latest_version,
        "sha256": plan.artifact_sha256,
        "metadata_url": plan.metadata_url,
        "artifact_url": plan.artifact_url,
        "source_commit": plan.metadata.source_commit,
        "published_at": plan.metadata.published_at,
        "store_schema_version": plan.metadata.store_schema_version,
        "installed_at": utc_now(),
    });
    atomic_write_json(marker_path, &body)
}

fn string_field(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("ctx install marker missing {key}"))
}

fn install_marker_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("ctx");
    path.with_file_name(format!("{file_name}.install.json"))
}

fn current_install_path() -> Result<PathBuf> {
    env::var_os("CTX_UPGRADE_TARGET")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(env::current_exe)
        .context("resolve current ctx executable")
}

fn current_binary_sha() -> Result<String> {
    let path = current_install_path()?;
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(sha256_hex(&bytes))
}

fn path_diagnostics(current_exe: &Path, current_version: &str) -> PathDiagnostics {
    let current_identity = path_identity(current_exe);
    let current_display = current_exe.display().to_string();
    let binary_name = if cfg!(windows) { "ctx.exe" } else { "ctx" };
    let mut entries = Vec::new();
    for dir in env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        let candidate = dir.join(binary_name);
        if !candidate.is_file() {
            continue;
        }
        if entries
            .iter()
            .any(|entry: &PathDiagnosticEntry| same_path(&entry.path, &candidate))
        {
            continue;
        }
        let current = path_identity(&candidate) == current_identity;
        entries.push(PathDiagnosticEntry {
            version: ctx_binary_version(&candidate).ok(),
            path: candidate,
            current,
        });
    }

    let mut warnings = Vec::new();
    match entries.first() {
        Some(first) if !first.current => warnings.push(format!(
            "PATH resolves ctx to {} before the current executable {}; your shell may keep using the earlier binary after upgrade",
            first.path.display(),
            current_display
        )),
        None => warnings.push(format!(
            "current ctx executable {current_display} is not discoverable on PATH"
        )),
        _ => {}
    }
    if entries.len() > 1 {
        warnings.push(format!(
            "multiple ctx binaries are on PATH; first is {}",
            entries[0].path.display()
        ));
    }
    let expected = format!("ctx {current_version}");
    for entry in &entries {
        if let Some(version) = &entry.version {
            if version != &expected {
                warnings.push(format!(
                    "ctx on PATH at {} reports {version}; current binary reports {expected}",
                    entry.path.display()
                ));
            }
        }
    }

    PathDiagnostics {
        current_exe: current_exe.to_path_buf(),
        entries,
        warnings,
    }
}

fn path_identity(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn same_path(left: &Path, right: &Path) -> bool {
    path_identity(left) == path_identity(right)
}

fn ctx_binary_version(path: &Path) -> Result<String> {
    let output = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("run {} --version", path.display()))?;
    if !output.status.success() {
        return Err(anyhow!("{} --version failed", path.display()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().next().unwrap_or_default().trim().to_owned())
}

fn write_state_checked(data_root: &Path, plan: &UpgradePlan, status: &str) -> Result<()> {
    let body = json!({
        "schema_version": 1,
        "status": status,
        "checked_at": utc_now(),
        "last_checked_unix_s": now_unix_s(),
        "current_version": plan.current_version,
        "latest_version": plan.latest_version,
        "update_available": plan.update_available,
        "channel": plan.channel,
        "platform": plan.platform,
        "metadata_url": plan.metadata_url,
        "artifact_url": plan.artifact_url,
        "install_path": plan.install_path,
        "managed": plan.managed,
    });
    atomic_write_json(&data_root.join(STATE_FILE), &body)
}

fn write_state_error(data_root: &Path, error: &str) -> Result<()> {
    let body = json!({
        "schema_version": 1,
        "status": "error",
        "checked_at": utc_now(),
        "last_checked_unix_s": now_unix_s(),
        "error": error,
    });
    atomic_write_json(&data_root.join(STATE_FILE), &body)
}

fn should_check_now(data_root: &Path, interval: Duration) -> bool {
    if interval.is_zero() {
        return true;
    }
    let Some(value) = read_json_file(&data_root.join(STATE_FILE)) else {
        return true;
    };
    let Some(last) = value.get("last_checked_unix_s").and_then(Value::as_u64) else {
        return true;
    };
    now_unix_s().saturating_sub(last) >= interval.as_secs()
}

fn read_json_file(path: &Path) -> Option<Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let body = serde_json::to_vec_pretty(value)?;
    fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))
}

struct UpgradeLock {
    path: PathBuf,
}

impl UpgradeLock {
    fn acquire(data_root: &Path) -> Result<Self> {
        fs::create_dir_all(data_root)?;
        let path = data_root.join(LOCK_FILE);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                writeln!(file, "{} {}", std::process::id(), now_unix_s())?;
                Ok(Self { path })
            }
            Err(error) => Err(anyhow!(
                "ctx upgrade lock is held at {}: {error}",
                path.display()
            )),
        }
    }
}

impl Drop for UpgradeLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn append_upgrade_log(data_root: &Path, message: &str) {
    let path = data_root.join(LOG_FILE);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {}", utc_now().to_rfc3339(), message);
    }
}

fn platform_key() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x64"),
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-x64"),
        ("windows", "x86_64") => Ok("windows-x64"),
        ("freebsd", "x86_64") => Ok("freebsd-x64"),
        (os, arch) => Err(anyhow!("unsupported ctx upgrade platform: {os}-{arch}")),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn version_gt(left: &str, right: &str) -> bool {
    let left = version_parts(left);
    let right = version_parts(right);
    left > right
}

fn version_parts(value: &str) -> Vec<u64> {
    value
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .take(4)
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn auto_mode_is_apply(config: &AppConfig) -> bool {
    config.upgrade.auto.eq_ignore_ascii_case("apply")
}

fn env_flag(key: &str) -> bool {
    env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

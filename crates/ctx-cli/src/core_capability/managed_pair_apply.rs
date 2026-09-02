use std::{
    ffi::OsStr,
    fs::{self, File},
    io::Read as _,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, bail, Context as _, Result};
use ctx_companion_bridge::ReleaseChannel;
use ctx_upgrade_engine::{
    apply_or_resume_managed_pair_under_installation_lock,
    ensure_hosted_transaction_inactive_under_installation_lock,
    inspect_managed_pair_under_installation_lock, managed_install_path_identity_matches,
    try_acquire_managed_installation_mutation_at_root, InstallMarker, ManagedPairApplyInput,
    ManagedPairInstallationStatus, ManagedPairTarget, ManagedPairVerifier,
    VerifiedManagedPairIdentity, MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH,
};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{write_response_frame, CoreManagedPairVerifier};

const ARGUMENT_COUNT: usize = 8;
pub(super) const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_ENVELOPE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MARKER_BYTES: u64 = 64 * 1024;
const SUCCESS_RECEIPT: &[u8] =
    br#"{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApplyRequest {
    install_root: PathBuf,
    signed_envelope: PathBuf,
    core: PathBuf,
    companion: PathBuf,
    install_marker: PathBuf,
}

impl ApplyRequest {
    pub(super) fn parse(arguments: &[std::ffi::OsString]) -> Result<Self> {
        if arguments.len() != ARGUMENT_COUNT {
            bail!("invalid managed-pair apply invocation");
        }
        if arguments[3] != OsStr::new("-") {
            bail!("managed-pair apply V1 requires data root '-'");
        }
        let install_root = normalized_absolute_path(&arguments[2], "install root")?;
        require_directory(&install_root, "install root")?;
        let signed_envelope = normalized_absolute_path(&arguments[4], "signed envelope")?;
        let core = normalized_absolute_path(&arguments[5], "Core")?;
        let companion = normalized_absolute_path(&arguments[6], "companion")?;
        let install_marker = normalized_absolute_path(&arguments[7], "install marker")?;
        for (path, label) in [
            (&signed_envelope, "signed envelope"),
            (&core, "Core"),
            (&companion, "companion"),
            (&install_marker, "install marker"),
        ] {
            require_regular_file(path, label)?;
        }
        Ok(Self {
            install_root,
            signed_envelope,
            core,
            companion,
            install_marker,
        })
    }

    pub(super) fn require_running_core(&self, running_core: &Path) -> Result<()> {
        let running_core = fs::canonicalize(running_core)
            .context("canonicalize running managed-pair candidate Core")?;
        if !managed_install_path_identity_matches(&running_core, &self.core) {
            bail!("managed-pair apply must run from the supplied candidate Core");
        }
        Ok(())
    }

    fn destination_core(&self) -> PathBuf {
        managed_core_destination(&self.install_root)
    }

    fn kernel_input(&self) -> ManagedPairApplyInput {
        ManagedPairApplyInput::new(
            self.signed_envelope.clone(),
            self.core.clone(),
            self.companion.clone(),
            self.install_marker.clone(),
        )
    }
}

pub(super) fn managed_core_destination(install_root: &Path) -> PathBuf {
    let executable = MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH
        .strip_suffix(".install.json")
        .expect("managed Core marker slot must suffix the executable path");
    install_root.join(executable)
}

pub(super) fn run(arguments: &[std::ffi::OsString]) -> Result<()> {
    let request = ApplyRequest::parse(arguments)?;
    request.require_running_core(
        &std::env::current_exe().context("resolve running managed-pair candidate Core")?,
    )?;
    apply(&request)?;
    write_response_frame(std::io::stdout().lock(), SUCCESS_RECEIPT)
}

fn apply(request: &ApplyRequest) -> Result<()> {
    let marker = read_install_marker(&request.install_marker)?;
    let channel = marker_channel(&marker)?;
    let verifier = CoreManagedPairVerifier::for_channel(channel);
    let _guard = try_acquire_managed_installation_mutation_at_root(&request.install_root)?
        .ok_or_else(|| anyhow!("managed-pair installation is busy"))?;
    ensure_hosted_transaction_inactive_under_installation_lock(&request.destination_core())?;
    let envelope = read_bounded_regular_file(
        &request.signed_envelope,
        MAX_ENVELOPE_BYTES,
        "signed envelope",
    )?;
    let envelope_sha256 = format!("{:x}", Sha256::digest(&envelope));
    let identity = verifier.verify_signed_envelope(&envelope)?;
    verify_component(&request.core, identity.core(), "Core")?;
    verify_component(&request.companion, identity.companion(), "companion")?;
    validate_install_marker(request, &marker, channel, &identity)?;

    apply_or_resume_managed_pair_under_installation_lock(
        &request.install_root,
        &request.kernel_input(),
        &verifier,
    )?;
    match inspect_managed_pair_under_installation_lock(&request.install_root, &verifier)? {
        ManagedPairInstallationStatus::Healthy {
            identity: active,
            envelope_sha256: active_envelope,
        } if active == identity && active_envelope.eq_ignore_ascii_case(&envelope_sha256) => Ok(()),
        _ => bail!("published managed pair does not match the requested signed candidate"),
    }
}

pub(super) fn read_install_marker(path: &Path) -> Result<InstallMarker> {
    let bytes = read_bounded_regular_file(path, MAX_MARKER_BYTES, "install marker")?;
    let value: Value =
        serde_json::from_slice(&bytes).context("parse managed Core install marker")?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || value.get("manager").and_then(Value::as_str) != Some("ctx-hosted-installer")
    {
        bail!("managed Core install marker schema is invalid");
    }
    Ok(InstallMarker {
        install_path: PathBuf::from(marker_string(&value, "install_path")?),
        platform: marker_string(&value, "platform")?.to_owned(),
        channel: marker_string(&value, "channel")?.to_owned(),
        version: marker_string(&value, "version")?.to_owned(),
        sha256: marker_string(&value, "sha256")?.to_owned(),
        staging_dogfood: value.get("staging_dogfood").and_then(Value::as_bool) == Some(true),
    })
}

fn marker_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("managed Core install marker is missing {key}"))
}

pub(super) fn marker_channel(marker: &InstallMarker) -> Result<ReleaseChannel> {
    match (marker.channel.as_str(), marker.staging_dogfood) {
        ("stable", false) => Ok(ReleaseChannel::Stable),
        ("staging", true) => Ok(ReleaseChannel::Staging),
        _ => bail!("managed Core install marker channel is inconsistent"),
    }
}

pub(super) fn validate_install_marker(
    request: &ApplyRequest,
    marker: &InstallMarker,
    channel: ReleaseChannel,
    identity: &VerifiedManagedPairIdentity,
) -> Result<()> {
    if !managed_install_path_identity_matches(&request.destination_core(), &marker.install_path)
        || marker_platform(identity.target()) != marker.platform
        || marker_channel(marker)? != channel
        || !marker.sha256.eq_ignore_ascii_case(identity.core().sha256())
        || marker.version != env!("CARGO_PKG_VERSION")
    {
        bail!("managed Core install marker does not match the signed candidate")
    }
    Ok(())
}

const fn marker_platform(target: ManagedPairTarget) -> &'static str {
    match target {
        ManagedPairTarget::LinuxArm64 => "linux-aarch64",
        ManagedPairTarget::LinuxX64 => "linux-x64",
        ManagedPairTarget::MacosArm64 => "macos-arm64",
        ManagedPairTarget::MacosX64 => "macos-x64",
        ManagedPairTarget::WindowsX64 => "windows-x64",
    }
}

fn verify_component(
    path: &Path,
    expected: &ctx_upgrade_engine::ManagedPairComponentIdentity,
    label: &str,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != expected.size_bytes()
        || sha256_file(path)? != expected.sha256()
    {
        bail!("managed-pair {label} does not match its signed identity");
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn read_bounded_regular_file(path: &Path, maximum: u64, label: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > maximum
    {
        bail!("managed-pair {label} is not a bounded regular file");
    }
    fs::read(path).with_context(|| format!("read {label}"))
}

fn normalized_absolute_path(value: &OsStr, label: &str) -> Result<PathBuf> {
    if value.as_encoded_bytes().len() > MAX_PATH_BYTES || value.as_encoded_bytes().contains(&0) {
        bail!("managed-pair {label} path exceeds its bound");
    }
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("managed-pair {label} path must be normalized and absolute");
    }
    let canonical = fs::canonicalize(&path)
        .with_context(|| format!("canonicalize managed-pair {label} path"))?;
    if !managed_install_path_identity_matches(&canonical, &path) {
        bail!("managed-pair {label} path must already be normalized");
    }
    Ok(path)
}

fn require_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("managed-pair {label} must be a directory");
    }
    Ok(())
}

fn require_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("managed-pair {label} must be a regular file");
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn success_receipt() -> &'static [u8] {
    SUCCESS_RECEIPT
}

use std::{
    cmp::Ordering,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_core::platform_security::{
    restrict_private_directory, restrict_private_executable, restrict_private_file,
    verify_private_directory,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::artifact_delivery::VerifiedArtifactBundle;
use super::client::default_helper_path;
use super::commercial_config::CommercialConfig;
use super::verified_executable::VerifiedHelperExecutable;
use lifecycle_manifest::{
    parse_release_version, validate_manifest_release_trust, verified_manifest,
    verified_manifest_for_trust, ProInstallMarker, ProManifest, MAX_ARTIFACT_BYTES,
    MAX_INSTALL_MARKER_BYTES, MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES,
};
#[cfg(test)]
use lifecycle_manifest::{
    platform_target, verify_signature_with_key, PRO_RELEASE_STAGING_PUBLIC_KEY_PEM,
};

const MAX_TRANSACTION_JOURNAL_BYTES: u64 = 16 * 1024;

#[path = "lifecycle_lock.rs"]
mod lifecycle_lock;
use lifecycle_lock::{
    install_marker_path, layout_for_target, previous_helper_path, previous_marker_path,
    publish_helper_path, publish_marker_path, rollback_helper_stage_path,
    rollback_marker_stage_path, transaction_helper_path, transaction_journal_next_path,
    transaction_journal_path, transaction_marker_path, validate_private_directory, LifecycleLock,
};

#[path = "lifecycle_manifest.rs"]
pub(super) mod lifecycle_manifest;

#[path = "lifecycle_commands.rs"]
mod commands;
mod persistence;
pub(crate) use commands::{lifecycle_status_json, run_lifecycle, ProArgs};
pub(super) use commands::{ProDeletionService, ProLifecycleService, ProManagePlan, ProSetupPlan};
use persistence::{
    cleanup_transaction_files, durable_write, prepare_install_directory,
    protect_existing_installation, publish_current, publish_previous, remove_current_pair,
    sync_install_directory, write_journal,
};
pub(super) use persistence::{replace_file, sync_parent_directory};

/// A fully downloaded release bundle. Artifact delivery owns construction of
/// this value; lifecycle only feeds it to the transactional installer.
#[derive(Debug, Clone)]
pub(crate) struct ProInstallArgs {
    artifact: PathBuf,
    manifest: PathBuf,
    signature: PathBuf,
}

impl ProInstallArgs {
    pub(crate) fn new(artifact: PathBuf, manifest: PathBuf, signature: PathBuf) -> Self {
        Self {
            artifact,
            manifest,
            signature,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PairIdentity {
    artifact_size: u64,
    artifact_sha256: String,
    marker_sha256: String,
    version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallTransaction {
    schema_version: u32,
    transaction_id: Uuid,
    state: TransactionState,
    old: Option<PairIdentity>,
    new: PairIdentity,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TransactionState {
    Preparing,
    Committed,
}

#[derive(Debug)]
struct SetupRepairRequiredError {
    message: &'static str,
}

impl std::fmt::Display for SetupRepairRequiredError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for SetupRepairRequiredError {}

fn setup_repair_required(message: &'static str) -> anyhow::Error {
    anyhow::Error::new(SetupRepairRequiredError { message })
}

pub(in crate::pro) fn is_setup_repair_required_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SetupRepairRequiredError>().is_some()
}

pub(in crate::pro) fn installation_artifacts_present(data_root: &Path) -> bool {
    let target = default_helper_path(data_root);
    let Some(bin) = target.parent() else {
        return true;
    };
    let Some(pro) = bin.parent() else {
        return true;
    };
    for directory in [
        data_root.to_path_buf(),
        pro.to_path_buf(),
        bin.to_path_buf(),
    ] {
        match fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return true,
        }
    }
    let paths = [
        Ok(target.clone()),
        install_marker_path(&target),
        previous_helper_path(&target),
        previous_marker_path(&target),
        transaction_journal_path(&target),
        transaction_journal_next_path(&target),
        transaction_helper_path(&target),
        transaction_marker_path(&target),
        publish_helper_path(&target),
        publish_marker_path(&target),
        rollback_helper_stage_path(&target),
        rollback_marker_stage_path(&target),
    ];
    paths.into_iter().any(|path| {
        let Ok(path) = path else {
            return true;
        };
        match fs::symlink_metadata(path) {
            Ok(_) => true,
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        }
    })
}

#[derive(Debug, Clone)]
pub(super) struct ValidatedPair {
    identity: PairIdentity,
    artifact: Vec<u8>,
    marker: Vec<u8>,
    manifest: ProManifest,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(super) enum SetupInstallation {
    Current(ValidatedPair),
    Missing,
    RepairRequired,
}

#[allow(clippy::large_enum_variant)]
enum PairInspection {
    Absent,
    Untrusted,
    Valid(ValidatedPair),
}

impl PairInspection {
    fn is_untrusted(&self) -> bool {
        matches!(self, Self::Untrusted)
    }
}

impl SetupInstallation {
    fn installed_version(&self) -> Option<&str> {
        match self {
            Self::Current(pair) => Some(pair.manifest.version.as_str()),
            Self::Missing | Self::RepairRequired => None,
        }
    }

    fn is_repair_required(&self) -> bool {
        matches!(self, Self::RepairRequired)
    }

    fn into_current(self) -> Option<ValidatedPair> {
        match self {
            Self::Current(pair) => Some(pair),
            Self::Missing | Self::RepairRequired => None,
        }
    }
}

impl PairIdentity {
    fn new(artifact: &[u8], marker: &[u8], manifest: &ProManifest) -> Self {
        Self {
            artifact_size: artifact.len() as u64,
            artifact_sha256: format!("{:x}", Sha256::digest(artifact)),
            marker_sha256: format!("{:x}", Sha256::digest(marker)),
            version: manifest.version.clone(),
        }
    }
}

#[derive(Default)]
struct Persistence {
    #[cfg(test)]
    crash_after: Option<usize>,
    #[cfg(test)]
    boundaries: Vec<&'static str>,
    #[cfg(test)]
    hard_exit: bool,
}

impl Persistence {
    fn boundary(&mut self, name: &'static str) -> Result<()> {
        #[cfg(test)]
        {
            self.boundaries.push(name);
            if self.crash_after == Some(self.boundaries.len()) {
                if self.hard_exit {
                    std::process::exit(86);
                }
                bail!("simulated_termination: {name}");
            }
        }
        #[cfg(not(test))]
        let _ = name;
        Ok(())
    }
}

pub(super) fn validated_installed_helper_path(data_root: &Path) -> Result<PathBuf> {
    validated_installed_helper(data_root).map(|helper| helper.path().to_path_buf())
}

pub(super) fn acquire_commercial_lifecycle_lock(
    data_root: &Path,
    create_pro_root: bool,
) -> Result<Option<impl Drop>> {
    LifecycleLock::acquire(&default_helper_path(data_root), create_pro_root)
}

pub(super) fn validated_installed_helper(data_root: &Path) -> Result<VerifiedHelperExecutable> {
    let target = default_helper_path(data_root);
    if !installation_artifacts_present(data_root) {
        bail!("pro_not_installed: signed Pro helper is not installed");
    }
    let trust = CommercialConfig::production()?.release_trust;
    let _lifecycle_lock = LifecycleLock::acquire(&target, false)?
        .ok_or_else(|| anyhow!("pro_not_installed: signed Pro helper is not installed"))?;
    let pair =
        reconcile_installation_locked(&target, trust.public_key_pem, &mut Persistence::default())?
            .ok_or_else(|| anyhow!("pro_not_installed: signed Pro helper is not installed"))?;
    validate_manifest_release_trust(&pair.manifest, trust)?;
    let marker_path = install_marker_path(&target)?;
    let helper = VerifiedHelperExecutable::open(data_root, &target, &marker_path)?;
    let locked_pair = validate_pair_bytes(
        helper.read_helper(MAX_ARTIFACT_BYTES)?,
        helper.read_marker(&marker_path, MAX_INSTALL_MARKER_BYTES)?,
        trust.public_key_pem,
    )?;
    validate_manifest_release_trust(&locked_pair.manifest, trust)?;
    if locked_pair.identity != pair.identity {
        bail!("invalid_response: installed Pro helper changed during validation");
    }
    Ok(helper)
}

#[cfg(test)]
fn validated_installed_helper_path_with_key(
    data_root: &Path,
    public_key_pem: &str,
) -> Result<PathBuf> {
    let target = default_helper_path(data_root);
    reconcile_installation(&target, public_key_pem, &mut Persistence::default())?
        .ok_or_else(|| anyhow!("pro_not_installed: signed Pro helper is not installed"))?;
    Ok(target)
}

#[cfg(test)]
fn lifecycle_status_json_with_key(data_root: &Path, public_key_pem: &str) -> serde_json::Value {
    let helper = super::client::status_with_helper_resolver(data_root, |data_root| {
        validated_installed_helper_path_with_key(data_root, public_key_pem)
    });
    commands::lifecycle_status_value(helper, false)
}

pub(crate) fn install_verified_bundle(
    bundle: &VerifiedArtifactBundle,
    data_root: &Path,
    trust: lifecycle_manifest::ReleaseTrust,
) -> Result<serde_json::Value> {
    let args = bundle.install_args();
    let target = default_helper_path(data_root);
    let _lifecycle_lock = LifecycleLock::acquire(&target, true)?
        .ok_or_else(|| anyhow!("invalid_request: failed to create Pro lifecycle lock"))?;
    let installation = reconcile_setup_installation_locked(
        &target,
        trust.public_key_pem,
        &mut Persistence::default(),
    )?;
    if let SetupInstallation::Current(current) = &installation {
        validate_manifest_release_trust(&current.manifest, trust)?;
    }
    let manifest_bytes = read_bounded(&args.manifest, MAX_MANIFEST_BYTES, "manifest")?;
    let signature = read_bounded(&args.signature, MAX_SIGNATURE_BYTES, "signature")?;
    let _ = verified_manifest_for_trust(&manifest_bytes, &signature, trust)?;
    install_for_setup_with_key_locked(
        &args,
        data_root,
        installation,
        trust.public_key_pem,
        &mut Persistence::default(),
    )
}

#[cfg(test)]
fn install_with_key(
    args: &ProInstallArgs,
    data_root: &Path,
    require_existing: bool,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<serde_json::Value> {
    let target = default_helper_path(data_root);
    let _lifecycle_lock = LifecycleLock::acquire(&target, true)?
        .ok_or_else(|| anyhow!("invalid_request: failed to create Pro lifecycle lock"))?;
    install_with_key_locked(
        args,
        data_root,
        require_existing,
        public_key_pem,
        persistence,
    )
}

#[cfg(test)]
fn install_with_key_locked(
    args: &ProInstallArgs,
    data_root: &Path,
    require_existing: bool,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<serde_json::Value> {
    let target = default_helper_path(data_root);
    let current = reconcile_installation_locked(&target, public_key_pem, persistence)?;
    if require_existing && current.is_none() {
        bail!("pro_not_installed: install Pro before updating it");
    }
    if !require_existing && current.is_some() {
        bail!("invalid_request: Pro is already installed; use ctx pro update");
    }
    install_candidate_with_key_locked(
        args,
        data_root,
        current.as_ref(),
        require_existing,
        public_key_pem,
        persistence,
    )
}

fn install_for_setup_with_key_locked(
    args: &ProInstallArgs,
    data_root: &Path,
    installation: SetupInstallation,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<serde_json::Value> {
    let replacing_existing =
        installation.is_repair_required() || matches!(&installation, SetupInstallation::Current(_));
    let current = installation.into_current();
    install_candidate_with_key_locked(
        args,
        data_root,
        current.as_ref(),
        replacing_existing,
        public_key_pem,
        persistence,
    )
}

fn install_candidate_with_key_locked(
    args: &ProInstallArgs,
    data_root: &Path,
    current: Option<&ValidatedPair>,
    replacing_existing: bool,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<serde_json::Value> {
    let target = default_helper_path(data_root);
    let manifest_bytes = read_bounded(&args.manifest, MAX_MANIFEST_BYTES, "manifest")?;
    let signature = read_bounded(&args.signature, MAX_SIGNATURE_BYTES, "signature")?;
    let manifest = verified_manifest(&manifest_bytes, &signature, public_key_pem)?;
    if let Some(current) = current {
        validate_update(current, &manifest)?;
    }
    let artifact = read_bounded(&args.artifact, MAX_ARTIFACT_BYTES, "artifact")?;
    if artifact.len() as u64 != manifest.artifact_size {
        bail!("invalid_response: Pro artifact size does not match signed manifest");
    }
    let actual = format!("{:x}", Sha256::digest(&artifact));
    if !actual.eq_ignore_ascii_case(&manifest.artifact_sha256) {
        bail!("invalid_response: Pro artifact digest does not match signed manifest");
    }
    let marker = ProInstallMarker::new(&manifest_bytes, &signature)?;
    let marker_bytes = serde_json::to_vec(&marker)
        .context("invalid_response: encode signed Pro install marker")?;
    let new_identity = PairIdentity::new(&artifact, &marker_bytes, &manifest);
    install_transaction_locked(
        &target,
        current,
        &artifact,
        &marker_bytes,
        new_identity,
        public_key_pem,
        persistence,
    )?;
    Ok(json!({
        "schema_version": 1,
        "installed": true,
        "updated": replacing_existing,
        "version": manifest.version,
        "source_commit": manifest.source_commit,
        "helper_path": target,
    }))
}

fn read_bounded(path: &Path, maximum: u64, label: &str) -> Result<Vec<u8>> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("invalid_request: inspect {label}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("invalid_request: {label} must be a regular non-symlink file");
    }
    if metadata.len() > maximum {
        bail!("invalid_request: {label} exceeds maximum size {maximum}");
    }
    fs::read(path).with_context(|| format!("invalid_request: read {label}"))
}

fn validate_update(current: &ValidatedPair, next: &ProManifest) -> Result<()> {
    let current_version = parse_release_version(&current.manifest.version)?;
    let next_version = parse_release_version(&next.version)?;
    match next_version.cmp(&current_version) {
        Ordering::Less => {
            bail!("invalid_request: Pro update would roll back the installed version")
        }
        Ordering::Equal
            if !next
                .artifact_sha256
                .eq_ignore_ascii_case(&current.identity.artifact_sha256) =>
        {
            bail!("invalid_request: Pro update reuses a version with different contents")
        }
        _ => Ok(()),
    }
}

fn install_transaction_locked(
    target: &Path,
    current: Option<&ValidatedPair>,
    artifact: &[u8],
    marker: &[u8],
    new_identity: PairIdentity,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<()> {
    prepare_install_directory(target, persistence)?;
    let mut transaction = InstallTransaction {
        schema_version: 1,
        transaction_id: Uuid::new_v4(),
        state: TransactionState::Preparing,
        old: current.map(|pair| pair.identity.clone()),
        new: new_identity,
    };
    write_journal(target, &transaction, persistence)?;
    durable_write(
        &transaction_helper_path(target)?,
        artifact,
        0o700,
        persistence,
        "write_transaction_helper",
        "chmod_transaction_helper",
        "fsync_transaction_helper",
    )?;
    durable_write(
        &transaction_marker_path(target)?,
        marker,
        0o600,
        persistence,
        "write_transaction_marker",
        "chmod_transaction_marker",
        "fsync_transaction_marker",
    )?;
    sync_install_directory(target, persistence, "fsync_staged_transaction_directory")?;
    transaction.state = TransactionState::Committed;
    write_journal(target, &transaction, persistence)?;
    let installed = reconcile_installation_locked(target, public_key_pem, persistence)?
        .ok_or_else(|| {
            anyhow!("invalid_response: committed Pro transaction produced no installed helper")
        })?;
    if installed.identity != transaction.new {
        bail!("invalid_response: committed Pro transaction published the wrong signed pair");
    }
    let final_pair = load_pair_at(target, &install_marker_path(target)?, public_key_pem)?
        .ok_or_else(|| anyhow!("invalid_response: final signed Pro helper pair is missing"))?;
    if final_pair.identity != transaction.new {
        bail!("invalid_response: final signed Pro helper pair verification failed");
    }
    Ok(())
}

#[cfg(test)]
fn reconcile_installation(
    target: &Path,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<Option<ValidatedPair>> {
    let Some(_lifecycle_lock) = LifecycleLock::acquire(target, false)? else {
        return Ok(None);
    };
    reconcile_installation_locked(target, public_key_pem, persistence)
}

fn reconcile_setup_installation_locked(
    target: &Path,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<SetupInstallation> {
    reconcile_installation_locked_with_setup_repair(target, public_key_pem, persistence, true)
}

fn reconcile_installation_locked(
    target: &Path,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<Option<ValidatedPair>> {
    match reconcile_installation_locked_with_setup_repair(
        target,
        public_key_pem,
        persistence,
        false,
    )? {
        SetupInstallation::Current(pair) => Ok(Some(pair)),
        SetupInstallation::Missing => Ok(None),
        SetupInstallation::RepairRequired => Err(setup_repair_required(
            "invalid_response: installed Pro helper and marker do not form a trusted pair",
        )),
    }
}

fn reconcile_installation_locked_with_setup_repair(
    target: &Path,
    public_key_pem: &str,
    persistence: &mut Persistence,
    allow_setup_repair: bool,
) -> Result<SetupInstallation> {
    let Some(parent) = target.parent() else {
        bail!("invalid_request: Pro install path has no parent");
    };
    if !parent.exists() {
        return Ok(SetupInstallation::Missing);
    }
    protect_existing_installation(target)?;
    let journal_path = transaction_journal_path(target)?;
    if !journal_path.exists() {
        let current = inspect_pair_at(target, &install_marker_path(target)?, public_key_pem)?;
        let current_untrusted = current.is_untrusted();
        if let PairInspection::Valid(pair) = current {
            cleanup_transaction_files(target, persistence)?;
            return Ok(SetupInstallation::Current(pair));
        }
        let previous = inspect_pair_at(
            &previous_helper_path(target)?,
            &previous_marker_path(target)?,
            public_key_pem,
        )?;
        let previous_untrusted = previous.is_untrusted();
        if let PairInspection::Valid(previous) = previous {
            publish_current(target, &previous, public_key_pem, persistence)?;
            cleanup_transaction_files(target, persistence)?;
            return Ok(SetupInstallation::Current(previous));
        }
        if current_untrusted || previous_untrusted {
            if allow_setup_repair {
                return Ok(SetupInstallation::RepairRequired);
            }
            return Err(setup_repair_required(
                "invalid_response: installed Pro helper and marker do not form a trusted pair",
            ));
        }
        cleanup_transaction_files(target, persistence)?;
        return Ok(SetupInstallation::Missing);
    }

    let journal_bytes = read_bounded(
        &journal_path,
        MAX_TRANSACTION_JOURNAL_BYTES,
        "transaction journal",
    )?;
    let transaction: InstallTransaction = serde_json::from_slice(&journal_bytes)
        .context("invalid_response: parse Pro transaction journal")?;
    validate_transaction(&transaction)?;
    let old = transaction
        .old
        .as_ref()
        .map(|identity| find_pair(target, identity, public_key_pem))
        .transpose()?
        .flatten();
    let new = find_pair(target, &transaction.new, public_key_pem)?;

    let chosen = if transaction.state == TransactionState::Committed {
        new.as_ref().or(old.as_ref())
    } else {
        old.as_ref()
    };
    let Some(chosen) = chosen else {
        if transaction.state == TransactionState::Committed {
            if allow_setup_repair {
                let current =
                    inspect_pair_at(target, &install_marker_path(target)?, public_key_pem)?;
                if let PairInspection::Valid(current) = current {
                    cleanup_transaction_files(target, persistence)?;
                    return Ok(SetupInstallation::Current(current));
                }
                let previous = inspect_pair_at(
                    &previous_helper_path(target)?,
                    &previous_marker_path(target)?,
                    public_key_pem,
                )?;
                if let PairInspection::Valid(previous) = previous {
                    publish_current(target, &previous, public_key_pem, persistence)?;
                    cleanup_transaction_files(target, persistence)?;
                    return Ok(SetupInstallation::Current(previous));
                }
                return Ok(SetupInstallation::RepairRequired);
            }
            return Err(setup_repair_required(
                "invalid_response: Pro transaction contains no recoverable signed helper pair",
            ));
        }
        if transaction.old.is_none() && transaction.state == TransactionState::Preparing {
            remove_current_pair(target, persistence)?;
            cleanup_transaction_files(target, persistence)?;
            return Ok(SetupInstallation::Missing);
        }
        bail!("invalid_response: Pro transaction contains no recoverable signed helper pair");
    };

    if transaction.state == TransactionState::Committed && chosen.identity == transaction.new {
        if let Some(old) = old.as_ref() {
            publish_previous(target, old, public_key_pem, persistence)?;
        }
    }
    publish_current(target, chosen, public_key_pem, persistence)?;
    let installed = load_pair_at(target, &install_marker_path(target)?, public_key_pem)?
        .ok_or_else(|| anyhow!("invalid_response: recovered Pro helper pair is missing"))?;
    cleanup_transaction_files(target, persistence)?;
    Ok(SetupInstallation::Current(installed))
}

fn validate_transaction(transaction: &InstallTransaction) -> Result<()> {
    if transaction.schema_version != 1 || transaction.transaction_id.is_nil() {
        bail!("invalid_response: invalid Pro transaction journal identity");
    }
    validate_pair_identity(&transaction.new)?;
    if let Some(old) = &transaction.old {
        validate_pair_identity(old)?;
    }
    Ok(())
}

fn validate_pair_identity(identity: &PairIdentity) -> Result<()> {
    if identity.artifact_size == 0
        || identity.artifact_size > MAX_ARTIFACT_BYTES
        || !is_lower_hex_digest(&identity.artifact_sha256)
        || !is_lower_hex_digest(&identity.marker_sha256)
        || parse_release_version(&identity.version).is_err()
    {
        bail!("invalid_response: invalid Pro transaction pair identity");
    }
    Ok(())
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn find_pair(
    target: &Path,
    identity: &PairIdentity,
    public_key_pem: &str,
) -> Result<Option<ValidatedPair>> {
    let helper_paths = [
        target.to_path_buf(),
        previous_helper_path(target)?,
        transaction_helper_path(target)?,
        publish_helper_path(target)?,
        rollback_helper_stage_path(target)?,
    ];
    let marker_paths = [
        install_marker_path(target)?,
        previous_marker_path(target)?,
        transaction_marker_path(target)?,
        publish_marker_path(target)?,
        rollback_marker_stage_path(target)?,
    ];
    let mut artifacts = Vec::new();
    for path in &helper_paths {
        if let Some(bytes) = read_candidate(path, MAX_ARTIFACT_BYTES)? {
            artifacts.push(bytes);
        }
    }
    let mut markers = Vec::new();
    for path in &marker_paths {
        if let Some(bytes) = read_candidate(path, MAX_INSTALL_MARKER_BYTES)? {
            markers.push(bytes);
        }
    }
    let artifact = artifacts.into_iter().find(|bytes| {
        bytes.len() as u64 == identity.artifact_size
            && format!("{:x}", Sha256::digest(bytes)) == identity.artifact_sha256
    });
    let marker = markers
        .into_iter()
        .find(|bytes| format!("{:x}", Sha256::digest(bytes)) == identity.marker_sha256);
    let (Some(artifact), Some(marker)) = (artifact, marker) else {
        return Ok(None);
    };
    let Ok(pair) = validate_pair_bytes(artifact, marker, public_key_pem) else {
        return Ok(None);
    };
    if pair.identity != *identity {
        return Ok(None);
    }
    Ok(Some(pair))
}

fn load_pair_at(
    helper_path: &Path,
    marker_path: &Path,
    public_key_pem: &str,
) -> Result<Option<ValidatedPair>> {
    match inspect_pair_at(helper_path, marker_path, public_key_pem)? {
        PairInspection::Absent => Ok(None),
        PairInspection::Valid(pair) => Ok(Some(pair)),
        PairInspection::Untrusted => {
            bail!("invalid_response: Pro helper and marker do not form a trusted pair")
        }
    }
}

fn inspect_pair_at(
    helper_path: &Path,
    marker_path: &Path,
    public_key_pem: &str,
) -> Result<PairInspection> {
    let helper = read_candidate(helper_path, MAX_ARTIFACT_BYTES)?;
    let marker = read_candidate(marker_path, MAX_INSTALL_MARKER_BYTES)?;
    match (helper, marker) {
        (None, None) => Ok(PairInspection::Absent),
        (Some(helper), Some(marker)) => match validate_pair_bytes(helper, marker, public_key_pem) {
            Ok(pair) => Ok(PairInspection::Valid(pair)),
            Err(_) => Ok(PairInspection::Untrusted),
        },
        _ => Ok(PairInspection::Untrusted),
    }
}

fn validate_pair_bytes(
    artifact: Vec<u8>,
    marker_bytes: Vec<u8>,
    public_key_pem: &str,
) -> Result<ValidatedPair> {
    let marker: ProInstallMarker = serde_json::from_slice(&marker_bytes)
        .context("invalid_response: parse installed Pro marker")?;
    let manifest = marker.signed_manifest(public_key_pem)?;
    let identity = PairIdentity::new(&artifact, &marker_bytes, &manifest);
    if artifact.len() as u64 != manifest.artifact_size
        || !identity
            .artifact_sha256
            .eq_ignore_ascii_case(&manifest.artifact_sha256)
    {
        bail!("invalid_response: installed Pro helper does not match its signed marker");
    }
    Ok(ValidatedPair {
        identity,
        artifact,
        marker: marker_bytes,
        manifest,
    })
}

fn read_candidate(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("invalid_request: inspect Pro transaction file"),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
        bail!("invalid_response: Pro transaction file is invalid");
    }
    fs::read(path)
        .map(Some)
        .context("invalid_request: read Pro transaction file")
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;

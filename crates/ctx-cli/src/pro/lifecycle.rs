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
pub(crate) use commands::{lifecycle_status_json, run_lifecycle, ProArgs};
pub(super) use commands::{ProDeletionService, ProLifecycleService, ProManagePlan, ProSetupPlan};

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

fn prepare_install_directory(target: &Path, persistence: &mut Persistence) -> Result<()> {
    let parent = layout_for_target(target)?.bin_dir();
    match fs::create_dir(&parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("invalid_request: create Pro install directory"),
    }
    persistence.boundary("create_install_directory")?;
    protect_install_directory_tree(target)?;
    persistence.boundary("chmod_install_directory")?;
    sync_install_directory(target, persistence, "fsync_install_directory")
}

#[allow(clippy::too_many_arguments)]
fn durable_write(
    path: &Path,
    contents: &[u8],
    unix_mode: u32,
    persistence: &mut Persistence,
    write_boundary: &'static str,
    chmod_boundary: &'static str,
    fsync_boundary: &'static str,
) -> Result<()> {
    remove_file_if_present(path, persistence, "remove_stale_staging_file")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(unix_mode);
    }
    let mut file = options
        .open(path)
        .context("invalid_request: create staged Pro install file")?;
    if unix_mode & 0o100 != 0 {
        restrict_private_executable(path)
            .context("invalid_request: protect staged Pro install executable")?;
    } else {
        restrict_private_file(path).context("invalid_request: protect staged Pro install file")?;
    }
    file.write_all(contents)
        .context("invalid_request: write staged Pro install file")?;
    persistence.boundary(write_boundary)?;
    persistence.boundary(chmod_boundary)?;
    file.sync_all()
        .context("invalid_request: sync staged Pro install file")?;
    persistence.boundary(fsync_boundary)?;
    let _ = unix_mode;
    Ok(())
}

fn write_journal(
    target: &Path,
    transaction: &InstallTransaction,
    persistence: &mut Persistence,
) -> Result<()> {
    let bytes = serde_json::to_vec(transaction)
        .context("invalid_response: encode Pro transaction journal")?;
    if bytes.len() as u64 > MAX_TRANSACTION_JOURNAL_BYTES {
        bail!("invalid_response: Pro transaction journal exceeds maximum size");
    }
    let next = transaction_journal_next_path(target)?;
    durable_write(
        &next,
        &bytes,
        0o600,
        persistence,
        "write_transaction_journal",
        "chmod_transaction_journal",
        "fsync_transaction_journal",
    )?;
    replace_file(&next, &transaction_journal_path(target)?)
        .context("invalid_request: publish Pro transaction journal")?;
    persistence.boundary("rename_transaction_journal")?;
    sync_install_directory(target, persistence, "fsync_transaction_journal_directory")
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

fn protect_install_directory_tree(target: &Path) -> Result<()> {
    let layout = layout_for_target(target)?;
    let pro = layout.pro_root();
    let bin = layout.bin_dir();
    for (directory, label) in [
        (layout.data_root(), "ctx data root"),
        (pro.as_path(), "Pro lifecycle root"),
        (bin.as_path(), "Pro install root"),
    ] {
        validate_private_directory(directory, label)?;
        restrict_private_directory(directory)
            .context("invalid_request: protect Pro install directory")?;
        verify_private_directory(directory)
            .context("invalid_request: verify Pro install directory")?;
    }
    Ok(())
}

fn protect_existing_installation(target: &Path) -> Result<()> {
    protect_install_directory_tree(target)?;
    let executables = [
        target.to_path_buf(),
        previous_helper_path(target)?,
        transaction_helper_path(target)?,
        publish_helper_path(target)?,
        rollback_helper_stage_path(target)?,
    ];
    for path in executables {
        protect_existing_install_file(&path, true)?;
    }
    let files = [
        install_marker_path(target)?,
        previous_marker_path(target)?,
        transaction_journal_path(target)?,
        transaction_journal_next_path(target)?,
        transaction_marker_path(target)?,
        publish_marker_path(target)?,
        rollback_marker_stage_path(target)?,
    ];
    for path in files {
        protect_existing_install_file(&path, false)?;
    }
    Ok(())
}

fn protect_existing_install_file(path: &Path, executable: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if executable {
                restrict_private_executable(path)
                    .context("invalid_request: protect Pro installation executable")
            } else {
                restrict_private_file(path)
                    .context("invalid_request: protect Pro installation file")
            }
        }
        Ok(_) => bail!("invalid_response: Pro installation path has an unsafe file type"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("invalid_request: inspect Pro installation file"),
    }
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

fn publish_current(
    target: &Path,
    pair: &ValidatedPair,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<()> {
    if load_pair_at(target, &install_marker_path(target)?, public_key_pem)
        .ok()
        .flatten()
        .is_some_and(|current| current.identity == pair.identity)
    {
        return Ok(());
    }
    publish_pair(
        target,
        &install_marker_path(target)?,
        &publish_helper_path(target)?,
        &publish_marker_path(target)?,
        pair,
        persistence,
        "write_current_helper_stage",
        "chmod_current_helper_stage",
        "fsync_current_helper_stage",
        "write_current_marker_stage",
        "chmod_current_marker_stage",
        "fsync_current_marker_stage",
        "rename_current_helper",
        "fsync_current_helper_directory",
        "rename_current_marker",
        "fsync_current_marker_directory",
    )
}

fn publish_previous(
    target: &Path,
    pair: &ValidatedPair,
    public_key_pem: &str,
    persistence: &mut Persistence,
) -> Result<()> {
    let helper = previous_helper_path(target)?;
    let marker = previous_marker_path(target)?;
    if load_pair_at(&helper, &marker, public_key_pem)
        .ok()
        .flatten()
        .is_some_and(|previous| previous.identity == pair.identity)
    {
        return Ok(());
    }
    publish_pair(
        &helper,
        &marker,
        &rollback_helper_stage_path(target)?,
        &rollback_marker_stage_path(target)?,
        pair,
        persistence,
        "write_rollback_helper_stage",
        "chmod_rollback_helper_stage",
        "fsync_rollback_helper_stage",
        "write_rollback_marker_stage",
        "chmod_rollback_marker_stage",
        "fsync_rollback_marker_stage",
        "rename_rollback_helper",
        "fsync_rollback_helper_directory",
        "rename_rollback_marker",
        "fsync_rollback_marker_directory",
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_pair(
    helper_target: &Path,
    marker_target: &Path,
    helper_stage: &Path,
    marker_stage: &Path,
    pair: &ValidatedPair,
    persistence: &mut Persistence,
    helper_write: &'static str,
    helper_chmod: &'static str,
    helper_fsync: &'static str,
    marker_write: &'static str,
    marker_chmod: &'static str,
    marker_fsync: &'static str,
    helper_rename: &'static str,
    helper_directory_fsync: &'static str,
    marker_rename: &'static str,
    marker_directory_fsync: &'static str,
) -> Result<()> {
    durable_write(
        helper_stage,
        &pair.artifact,
        0o700,
        persistence,
        helper_write,
        helper_chmod,
        helper_fsync,
    )?;
    durable_write(
        marker_stage,
        &pair.marker,
        0o600,
        persistence,
        marker_write,
        marker_chmod,
        marker_fsync,
    )?;
    sync_path_directory(
        helper_target,
        persistence,
        "fsync_publish_staging_directory",
    )?;
    replace_file(helper_stage, helper_target)
        .context("invalid_request: publish Pro helper file")?;
    persistence.boundary(helper_rename)?;
    sync_path_directory(helper_target, persistence, helper_directory_fsync)?;
    replace_file(marker_stage, marker_target)
        .context("invalid_request: publish Pro marker file")?;
    persistence.boundary(marker_rename)?;
    sync_path_directory(marker_target, persistence, marker_directory_fsync)
}

fn remove_current_pair(target: &Path, persistence: &mut Persistence) -> Result<()> {
    remove_file_if_present(target, persistence, "remove_incomplete_current_helper")?;
    remove_file_if_present(
        &install_marker_path(target)?,
        persistence,
        "remove_incomplete_current_marker",
    )?;
    sync_install_directory(target, persistence, "fsync_removed_current_pair_directory")
}

fn cleanup_transaction_files(target: &Path, persistence: &mut Persistence) -> Result<()> {
    for (path, boundary) in [
        (
            transaction_journal_next_path(target)?,
            "remove_transaction_journal_next",
        ),
        (
            transaction_helper_path(target)?,
            "remove_transaction_helper",
        ),
        (
            transaction_marker_path(target)?,
            "remove_transaction_marker",
        ),
        (publish_helper_path(target)?, "remove_publish_helper"),
        (publish_marker_path(target)?, "remove_publish_marker"),
        (
            rollback_helper_stage_path(target)?,
            "remove_rollback_helper_stage",
        ),
        (
            rollback_marker_stage_path(target)?,
            "remove_rollback_marker_stage",
        ),
    ] {
        remove_file_if_present(&path, persistence, boundary)?;
    }
    sync_install_directory(target, persistence, "fsync_transaction_cleanup_directory")?;
    remove_file_if_present(
        &transaction_journal_path(target)?,
        persistence,
        "remove_transaction_journal",
    )?;
    sync_install_directory(target, persistence, "fsync_journal_removal_directory")
}

fn remove_file_if_present(
    path: &Path,
    persistence: &mut Persistence,
    boundary: &'static str,
) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => persistence.boundary(boundary),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => bail!("invalid_request: remove Pro transaction file"),
    }
}

fn sync_install_directory(
    target: &Path,
    persistence: &mut Persistence,
    boundary: &'static str,
) -> Result<()> {
    sync_path_directory(target, persistence, boundary)
}

fn sync_path_directory(
    path: &Path,
    persistence: &mut Persistence,
    boundary: &'static str,
) -> Result<()> {
    sync_parent_directory(path)?;
    persistence.boundary(boundary)
}

pub(super) fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("invalid_request: Pro install path has no parent"))?;
    #[cfg(not(windows))]
    let directory =
        fs::File::open(parent).context("invalid_request: open Pro install directory")?;
    #[cfg(windows)]
    let directory = {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
            .open(parent)
            .context("invalid_request: open Pro install directory")?
    };
    directory
        .sync_all()
        .context("invalid_request: sync Pro install directory")?;
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
pub(super) fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both path buffers are NUL-terminated and remain alive for the call.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;

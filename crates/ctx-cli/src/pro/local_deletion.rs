use std::{
    collections::BTreeSet,
    fs,
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, bail, Context as _, Result};
use ctx_history_core::platform_security::{
    restrict_private_file, verify_private_directory, verify_private_file,
};
use ctx_pro_host_protocol::{
    decode_base64url, installation_key_thumbprint, is_pro_graph_artifact_file_name,
    pro_graph_record_id, valid_pro_installation_id, ProFilesystemLayout,
    INSTALLATION_PUBLIC_KEY_BYTES,
};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use super::{
    commercial_deletion,
    credential_vault::{
        CredentialRecord, CredentialRecordKind, CredentialVaultError, CredentialVaultNamespace,
        PlatformCredentialVault,
    },
    graph_key_deletion,
    lifecycle::{replace_file, sync_parent_directory, ProDeletionService},
};

const PRO_INITIALIZATION_MARKER_FILE_NAME: &str = ".ctx-pro.initialized";
const PRO_INITIALIZATION_MARKER_CONTENT: &[u8] = b"ctx-local-pro-initialized-v1\n";
const GRAPH_KEY_CLEANUP_PHASE_FILE_NAME: &str = ".ctx-pro.graph-key-cleanup.json";
const GRAPH_KEY_CLEANUP_PHASE_SCHEMA_VERSION: u16 = 2;
const MAX_GRAPH_KEY_CLEANUP_PHASE_BYTES: u64 = 4 * 1024;
const MAX_RECORDED_GRAPH_KEYS: usize = 4;

#[derive(Debug, Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum GraphKeyCredentialNamespace {
    Production,
    Staging,
}

impl GraphKeyCredentialNamespace {
    const fn credential_vault_namespace(self) -> CredentialVaultNamespace {
        match self {
            Self::Production => CredentialVaultNamespace::Production,
            Self::Staging => CredentialVaultNamespace::Staging,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct GraphKeyDeletionTarget {
    namespace: GraphKeyCredentialNamespace,
    installation_key_thumbprint: String,
}

impl GraphKeyDeletionTarget {
    fn validate(&self) -> bool {
        decode_base64url(&self.installation_key_thumbprint)
            .is_some_and(|decoded| decoded.len() == INSTALLATION_PUBLIC_KEY_BYTES)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GraphKeyCleanupPhase {
    schema_version: u16,
    installation_id: String,
    targets: Vec<GraphKeyDeletionTarget>,
}

impl GraphKeyCleanupPhase {
    fn validate(&self) -> Result<()> {
        if self.schema_version != GRAPH_KEY_CLEANUP_PHASE_SCHEMA_VERSION
            || !valid_pro_installation_id(&self.installation_id)
            || self.targets.len() > MAX_RECORDED_GRAPH_KEYS
            || self.targets.windows(2).any(|pair| pair[0] >= pair[1])
            || self.targets.iter().any(|target| !target.validate())
        {
            bail!("invalid_request: graph-key cleanup phase is invalid");
        }
        Ok(())
    }

    fn validated_targets(self, installation_id: &str) -> Result<BTreeSet<GraphKeyDeletionTarget>> {
        self.validate()?;
        if self.installation_id != installation_id {
            bail!("key_store_unavailable: graph-key cleanup identity does not match this root");
        }
        Ok(self.targets.into_iter().collect())
    }
}

pub(super) struct LocalDeletionService<B = PlatformDeletionBackend> {
    backend: B,
    helperless_bootstrap: bool,
}

#[cfg(test)]
impl<B> LocalDeletionService<B> {
    fn with_backend_for_test(backend: B) -> Self {
        Self {
            backend,
            helperless_bootstrap: false,
        }
    }
}

impl LocalDeletionService<PlatformDeletionBackend> {
    pub(super) const fn production() -> Self {
        Self {
            backend: PlatformDeletionBackend,
            helperless_bootstrap: false,
        }
    }
}

impl<B: DeletionBackend> ProDeletionService for LocalDeletionService<B> {
    fn delete_graph_data(&mut self, data_root: &Path) -> Result<()> {
        self.helperless_bootstrap = false;
        let graph = GraphArtifacts::inspect(data_root)?;
        let initialization_recorded = local_pro_initialization_indicator_exists(data_root)?;
        let helper_present = local_pro_helper_exists(data_root)?;
        let cleanup_phase = read_graph_key_cleanup_phase(data_root)?;
        if !graph.any_present
            && !initialization_recorded
            && !helper_present
            && cleanup_phase.is_none()
        {
            return Ok(());
        }
        let installation_id = crate::identity::existing_installation_id(data_root)
            .context("key_store_unavailable: load local Pro installation identity")?
            .ok_or_else(|| {
                anyhow!("key_store_unavailable: local Pro installation identity is missing")
            })?;
        let helperless_bootstrap = initialization_recorded
            && !graph.any_present
            && !helper_present
            && cleanup_phase
                .as_ref()
                .is_none_or(|phase| phase.targets.is_empty());
        let targets = match cleanup_phase {
            Some(phase) => {
                let targets = phase.validated_targets(&installation_id)?;
                if (graph.any_present || helper_present) && targets.is_empty() {
                    bail!(
                        "key_store_unavailable: graph-key cleanup phase is incomplete while local Pro artifacts remain"
                    );
                }
                targets
            }
            None if helperless_bootstrap => {
                let targets = BTreeSet::new();
                write_graph_key_cleanup_phase(data_root, &installation_id, &targets)?;
                targets
            }
            None => {
                let targets = self.backend.graph_key_deletion_targets(data_root)?;
                if (graph.any_present || helper_present) && targets.is_empty() {
                    bail!(
                        "key_store_unavailable: graph-key identity is missing while local Pro artifacts remain"
                    );
                }
                write_graph_key_cleanup_phase(data_root, &installation_id, &targets)?;
                targets
            }
        };
        graph.delete()?;
        if GraphArtifacts::inspect(data_root)?.any_present {
            bail!("key_store_unavailable: encrypted Pro graph deletion could not be verified");
        }
        if !helperless_bootstrap {
            for target in &targets {
                let graph_id =
                    pro_graph_record_id(&installation_id, &target.installation_key_thumbprint)
                        .ok_or_else(|| {
                            anyhow!(
                                "key_store_unavailable: local Pro installation identity is invalid"
                            )
                        })?;
                self.backend
                    .delete_graph_record(data_root, target, &graph_id)?;
            }
        }
        self.helperless_bootstrap = helperless_bootstrap;
        Ok(())
    }

    fn delete_commercial_credentials(&mut self, data_root: &Path) -> Result<()> {
        if self.helperless_bootstrap {
            self.backend.delete_partial_bootstrap_credentials(data_root)
        } else {
            self.backend.delete_commercial_credentials(data_root)
        }
    }

    fn finish_deletion(&mut self, data_root: &Path) -> Result<()> {
        clear_graph_key_cleanup_phase(data_root)
    }
}

pub(super) fn local_pro_graph_data_exists(data_root: &Path) -> Result<bool> {
    Ok(GraphArtifacts::inspect(data_root)?.any_present)
}

pub(super) fn local_pro_initialization_indicator_exists(data_root: &Path) -> Result<bool> {
    let path = local_pro_initialization_indicator_path(data_root);
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            verify_private_file(&path).context("verify local Pro initialization marker")?;
            if fs::read(&path).context("read local Pro initialization marker")?
                != PRO_INITIALIZATION_MARKER_CONTENT
            {
                bail!("invalid_request: local Pro initialization marker has invalid content");
            }
            Ok(true)
        }
        Ok(_) => bail!("invalid_request: local Pro initialization marker is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("inspect local Pro initialization marker"),
    }
}

pub(super) fn write_local_pro_initialization_indicator(data_root: &Path) -> Result<()> {
    let layout = ProFilesystemLayout::new(data_root);
    let pro_root = layout.pro_root();
    verify_private_directory(&pro_root)
        .context("invalid_request: verify private Pro lifecycle directory")?;
    if local_pro_initialization_indicator_exists(data_root)? {
        return Ok(());
    }

    let path = local_pro_initialization_indicator_path(data_root);
    let staged = local_pro_initialization_indicator_staged_path(data_root);
    publish_private_lifecycle_file(
        &path,
        &staged,
        PRO_INITIALIZATION_MARKER_CONTENT,
        "local Pro initialization marker",
    )
}

pub(super) fn clear_local_pro_initialization_indicator(data_root: &Path) -> Result<()> {
    let path = local_pro_initialization_indicator_path(data_root);
    let removed = delete_private_lifecycle_file(&path, "local Pro initialization marker")?;
    let staged = local_pro_initialization_indicator_staged_path(data_root);
    let removed_staged = delete_private_lifecycle_file(&staged, "local Pro initialization marker")?;
    if removed || removed_staged {
        sync_parent_directory(&path)?;
    }
    Ok(())
}

pub(super) fn local_pro_graph_key_cleanup_phase_exists(data_root: &Path) -> Result<bool> {
    Ok(read_graph_key_cleanup_phase(data_root)?.is_some())
}

fn local_pro_initialization_indicator_path(data_root: &Path) -> PathBuf {
    ProFilesystemLayout::new(data_root)
        .pro_root()
        .join(PRO_INITIALIZATION_MARKER_FILE_NAME)
}

fn local_pro_initialization_indicator_staged_path(data_root: &Path) -> PathBuf {
    local_pro_initialization_indicator_path(data_root).with_extension("initialized.next")
}

fn graph_key_cleanup_phase_path(data_root: &Path) -> PathBuf {
    ProFilesystemLayout::new(data_root)
        .pro_root()
        .join(GRAPH_KEY_CLEANUP_PHASE_FILE_NAME)
}

fn graph_key_cleanup_phase_staged_path(data_root: &Path) -> PathBuf {
    graph_key_cleanup_phase_path(data_root).with_extension("json.next")
}

fn read_graph_key_cleanup_phase(data_root: &Path) -> Result<Option<GraphKeyCleanupPhase>> {
    let path = graph_key_cleanup_phase_path(data_root);
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!("invalid_request: graph-key cleanup phase is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect graph-key cleanup phase"),
    }
    verify_private_file(&path).context("verify graph-key cleanup phase")?;
    let mut contents = Vec::new();
    fs::File::open(&path)
        .context("open graph-key cleanup phase")?
        .take(MAX_GRAPH_KEY_CLEANUP_PHASE_BYTES + 1)
        .read_to_end(&mut contents)
        .context("read graph-key cleanup phase")?;
    if contents.len() as u64 > MAX_GRAPH_KEY_CLEANUP_PHASE_BYTES {
        bail!("invalid_request: graph-key cleanup phase exceeds the maximum size");
    }
    let phase: GraphKeyCleanupPhase = serde_json::from_slice(&contents)
        .context("invalid_request: decode graph-key cleanup phase")?;
    phase.validate()?;
    Ok(Some(phase))
}

fn write_graph_key_cleanup_phase(
    data_root: &Path,
    installation_id: &str,
    targets: &BTreeSet<GraphKeyDeletionTarget>,
) -> Result<()> {
    let phase = GraphKeyCleanupPhase {
        schema_version: GRAPH_KEY_CLEANUP_PHASE_SCHEMA_VERSION,
        installation_id: installation_id.to_owned(),
        targets: targets.iter().cloned().collect(),
    };
    phase.validate()?;
    let contents =
        serde_json::to_vec(&phase).context("invalid_request: encode graph-key cleanup phase")?;
    if contents.len() as u64 > MAX_GRAPH_KEY_CLEANUP_PHASE_BYTES {
        bail!("invalid_request: graph-key cleanup phase exceeds the maximum size");
    }
    publish_private_lifecycle_file(
        &graph_key_cleanup_phase_path(data_root),
        &graph_key_cleanup_phase_staged_path(data_root),
        &contents,
        "graph-key cleanup phase",
    )
}

#[cfg(test)]
pub(super) fn write_empty_graph_key_cleanup_phase_for_test(
    data_root: &Path,
    installation_id: &str,
) -> Result<()> {
    write_graph_key_cleanup_phase(data_root, installation_id, &BTreeSet::new())
}

fn clear_graph_key_cleanup_phase(data_root: &Path) -> Result<()> {
    let path = graph_key_cleanup_phase_path(data_root);
    let removed = delete_private_lifecycle_file(&path, "graph-key cleanup phase")?;
    let staged = graph_key_cleanup_phase_staged_path(data_root);
    let removed_staged = delete_private_lifecycle_file(&staged, "graph-key cleanup phase")?;
    if removed || removed_staged {
        sync_parent_directory(&path)?;
    }
    Ok(())
}

fn local_pro_helper_exists(data_root: &Path) -> Result<bool> {
    let layout = ProFilesystemLayout::new(data_root);
    for path in [layout.helper_path(), layout.helper_marker_path()] {
        match path.symlink_metadata() {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                return Ok(true);
            }
            Ok(_) => bail!("invalid_request: local Pro helper artifact is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect local Pro helper artifact"),
        }
    }
    Ok(false)
}

fn publish_private_lifecycle_file(
    path: &Path,
    staged: &Path,
    contents: &[u8],
    label: &str,
) -> Result<()> {
    delete_private_lifecycle_file(staged, label)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    let mut file = options
        .open(staged)
        .with_context(|| format!("create {label}"))?;
    restrict_private_file(staged).with_context(|| format!("protect {label}"))?;
    file.write_all(contents)
        .with_context(|| format!("write {label}"))?;
    file.sync_all().with_context(|| format!("sync {label}"))?;
    verify_private_file(staged).with_context(|| format!("verify {label}"))?;
    replace_file(staged, path).with_context(|| format!("publish {label}"))?;
    sync_parent_directory(path)
}

fn delete_private_lifecycle_file(path: &Path, label: &str) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path).with_context(|| format!("remove {label}"))?;
            Ok(true)
        }
        Ok(_) => bail!("invalid_request: {label} is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {label}")),
    }
}

trait DeletionBackend {
    fn graph_key_deletion_targets(
        &self,
        data_root: &Path,
    ) -> Result<BTreeSet<GraphKeyDeletionTarget>>;
    fn delete_graph_record(
        &self,
        data_root: &Path,
        target: &GraphKeyDeletionTarget,
        graph_id: &str,
    ) -> Result<()>;
    fn delete_partial_bootstrap_credentials(&self, data_root: &Path) -> Result<()>;
    fn delete_commercial_credentials(&self, data_root: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PlatformDeletionBackend;

impl DeletionBackend for PlatformDeletionBackend {
    fn graph_key_deletion_targets(
        &self,
        data_root: &Path,
    ) -> Result<BTreeSet<GraphKeyDeletionTarget>> {
        let mut targets = BTreeSet::new();
        for namespace in [
            GraphKeyCredentialNamespace::Production,
            GraphKeyCredentialNamespace::Staging,
        ] {
            let vault = PlatformCredentialVault::production(
                data_root,
                namespace.credential_vault_namespace(),
            )
            .map_err(vault_error)?;
            targets.extend(recorded_graph_key_targets(namespace, &vault)?);
        }
        Ok(targets)
    }

    fn delete_graph_record(
        &self,
        data_root: &Path,
        _target: &GraphKeyDeletionTarget,
        graph_id: &str,
    ) -> Result<()> {
        graph_key_deletion::delete_selected(data_root, graph_id)
    }

    fn delete_partial_bootstrap_credentials(&self, data_root: &Path) -> Result<()> {
        delete_partial_bootstrap_credentials(data_root)
    }

    fn delete_commercial_credentials(&self, data_root: &Path) -> Result<()> {
        commercial_deletion::delete_credentials(data_root)
    }
}

fn delete_partial_bootstrap_credentials(data_root: &Path) -> Result<()> {
    for namespace in [
        CredentialVaultNamespace::Production,
        CredentialVaultNamespace::Staging,
    ] {
        let vault =
            PlatformCredentialVault::production(data_root, namespace).map_err(vault_error)?;
        for kind in [
            CredentialRecordKind::WorkOsSession,
            CredentialRecordKind::AnonymousTrial,
            CredentialRecordKind::SignedEntitlement,
            CredentialRecordKind::InstallationSigningKey,
        ] {
            match vault.delete(kind) {
                Ok(()) | Err(CredentialVaultError::NotFound) => {}
                Err(error) => return Err(vault_error(error)),
            }
            match vault.load(kind) {
                Err(CredentialVaultError::NotFound) => {}
                Ok(_) | Err(CredentialVaultError::Corrupt) => {
                    bail!(
                        "key_store_unavailable: partial Pro credential deletion could not be verified"
                    )
                }
                Err(error) => return Err(vault_error(error)),
            }
        }
    }
    PlatformCredentialVault::cleanup_backend_state(data_root).map_err(vault_error)
}

trait CredentialRecordReader {
    fn load_record(
        &self,
        kind: CredentialRecordKind,
    ) -> Result<CredentialRecord, CredentialVaultError>;
}

impl CredentialRecordReader for PlatformCredentialVault {
    fn load_record(
        &self,
        kind: CredentialRecordKind,
    ) -> Result<CredentialRecord, CredentialVaultError> {
        self.load(kind)
    }
}

fn recorded_graph_key_targets(
    namespace: GraphKeyCredentialNamespace,
    vault: &impl CredentialRecordReader,
) -> Result<BTreeSet<GraphKeyDeletionTarget>> {
    let installation_key_thumbprint =
        match vault.load_record(CredentialRecordKind::InstallationSigningKey) {
            Ok(CredentialRecord::InstallationSigningKey(seed)) => {
                let public_key = SigningKey::from_bytes(seed.expose())
                    .verifying_key()
                    .to_bytes();
                Some(installation_key_thumbprint(&public_key))
            }
            Ok(_) => bail!("key_store_unavailable: installation key record has the wrong type"),
            Err(CredentialVaultError::NotFound) => None,
            Err(error) => return Err(vault_error(error)),
        };
    let entitlement_thumbprint = match vault.load_record(CredentialRecordKind::SignedEntitlement) {
        Ok(CredentialRecord::SignedEntitlement(entitlement)) => Some(
            entitlement
                .as_inner()
                .grant
                .installation_key_thumbprint
                .clone(),
        ),
        Ok(_) => bail!("key_store_unavailable: signed entitlement record has the wrong type"),
        Err(CredentialVaultError::NotFound) => None,
        Err(error) => return Err(vault_error(error)),
    };
    if matches!(
        (&installation_key_thumbprint, &entitlement_thumbprint),
        (Some(key), Some(entitlement)) if key != entitlement
    ) {
        bail!(
            "key_store_unavailable: installation key and signed entitlement disagree in one credential namespace"
        );
    }
    Ok(installation_key_thumbprint
        .or(entitlement_thumbprint)
        .map(|installation_key_thumbprint| {
            BTreeSet::from([GraphKeyDeletionTarget {
                namespace,
                installation_key_thumbprint,
            }])
        })
        .unwrap_or_default())
}

pub(super) fn vault_error(error: CredentialVaultError) -> anyhow::Error {
    let code = match error {
        CredentialVaultError::Locked => "key_store_locked",
        CredentialVaultError::NotFound
        | CredentialVaultError::Corrupt
        | CredentialVaultError::Ambiguous
        | CredentialVaultError::Unavailable { .. }
        | CredentialVaultError::SecretTooLarge { .. }
        | CredentialVaultError::InvalidDataRoot
        | CredentialVaultError::InvalidRecordId
        | CredentialVaultError::EntropyUnavailable
        | CredentialVaultError::Backend => "key_store_unavailable",
    };
    anyhow!("{code}: {error}")
}

struct GraphArtifacts {
    existing_root: Option<PathBuf>,
    paths: Vec<PathBuf>,
    any_present: bool,
}

impl GraphArtifacts {
    fn inspect(data_root: &Path) -> Result<Self> {
        validate_data_root(data_root)?;
        match data_root.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    existing_root: None,
                    paths: Vec::new(),
                    any_present: false,
                });
            }
            Ok(_) => {}
            Err(error) => {
                return Err(error)
                    .context("invalid_request: inspect ctx data root for Pro deletion")
            }
        }
        let canonical_data_root = data_root
            .canonicalize()
            .context("invalid_request: resolve ctx data root for Pro deletion")?;
        let metadata = canonical_data_root
            .symlink_metadata()
            .context("invalid_request: inspect ctx data root for Pro deletion")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            bail!("invalid_request: ctx data root is not a safe directory");
        }

        let supplied_pro_root = ProFilesystemLayout::new(data_root).pro_root();
        let pro_root = match supplied_pro_root.symlink_metadata() {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                let canonical = supplied_pro_root
                    .canonicalize()
                    .context("invalid_request: resolve Pro data directory")?;
                canonical
            }
            Ok(_) => bail!("invalid_request: Pro data directory is not a safe directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    existing_root: None,
                    paths: Vec::new(),
                    any_present: false,
                });
            }
            Err(error) => return Err(error).context("invalid_request: inspect Pro data directory"),
        };
        let graph_root = pro_root.join(ctx_pro_host_protocol::PRO_GRAPH_DIRECTORY_NAME);
        let metadata = match graph_root.symlink_metadata() {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => metadata,
            Ok(_) => bail!("invalid_request: Pro graph path is not a safe directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    existing_root: None,
                    paths: Vec::new(),
                    any_present: false,
                });
            }
            Err(error) => {
                return Err(error).context("invalid_request: inspect Pro graph directory")
            }
        };
        let pro_metadata = pro_root
            .symlink_metadata()
            .context("invalid_request: inspect Pro data directory")?;
        validate_graph_root_metadata(&pro_root, &pro_metadata)?;
        verify_private_directory(&pro_root)
            .context("invalid_request: verify private Pro data directory")?;
        validate_graph_root_metadata(&graph_root, &metadata)?;
        verify_private_directory(&graph_root)
            .context("invalid_request: verify private Pro graph directory")?;
        let paths = inventory_graph_files(&graph_root)?;
        Ok(Self {
            existing_root: Some(graph_root),
            any_present: !paths.is_empty(),
            paths,
        })
    }

    fn delete(self) -> Result<()> {
        let Some(root) = self.existing_root else {
            return Ok(());
        };
        let metadata = root
            .symlink_metadata()
            .context("invalid_request: revalidate Pro data directory")?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || !root.canonicalize().is_ok_and(|canonical| canonical == root)
        {
            bail!("invalid_request: Pro data directory changed during deletion");
        }
        validate_graph_root_metadata(&root, &metadata)?;
        verify_private_directory(&root)
            .context("invalid_request: reverify private Pro graph directory")?;
        let current_paths = inventory_graph_files(&root)?;
        if current_paths != self.paths {
            bail!("invalid_request: Pro graph inventory changed during deletion");
        }
        for path in &self.paths {
            delete_regular_file(path)?;
        }
        if let Some(path) = self.paths.first() {
            sync_parent_directory(path)?;
        }
        fs::remove_dir(&root).context("remove encrypted Pro graph directory")?;
        sync_parent_directory(&root)?;
        match root.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) | Err(_) => {
                bail!("invalid_request: encrypted Pro graph deletion could not be verified")
            }
        }
    }
}

fn inventory_graph_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).context("invalid_request: read Pro graph directory")? {
        let entry = entry.context("invalid_request: read Pro graph entry")?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| anyhow!("invalid_request: Pro graph entry name is not UTF-8"))?;
        if !is_pro_graph_artifact_file_name(name.as_bytes()) {
            bail!("invalid_request: Pro graph directory contains an unexpected entry");
        }
        let path = root.join(name);
        let metadata = path
            .symlink_metadata()
            .context("invalid_request: inspect Pro graph artifact")?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("invalid_request: Pro graph artifact is not a safe regular file");
        }
        validate_graph_file_metadata(&path, &metadata)?;
        verify_private_file(&path).context("invalid_request: verify private Pro graph artifact")?;
        paths.push(path);
    }
    paths.sort_unstable();
    Ok(paths)
}

#[cfg(unix)]
fn validate_graph_root_metadata(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid || metadata.permissions().mode() & 0o077 != 0 {
        bail!(
            "invalid_request: Pro data directory must be owned by the current user and accessible only to that user: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_graph_root_metadata(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn validate_graph_file_metadata(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid || metadata.nlink() != 1 {
        bail!(
            "invalid_request: Pro graph artifact must be owned by the current user and have one link: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_graph_file_metadata(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

fn validate_data_root(data_root: &Path) -> Result<()> {
    if !data_root.is_absolute()
        || data_root
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("invalid_request: ctx data root must be a safe absolute path");
    }
    Ok(())
}

fn delete_regular_file(path: &Path) -> Result<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path).context("remove encrypted Pro graph file")
        }
        Ok(_) => bail!("invalid_request: Pro graph path changed during deletion"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("inspect encrypted Pro graph file"),
    }
}

#[cfg(test)]
#[path = "local_deletion/tests.rs"]
mod tests;

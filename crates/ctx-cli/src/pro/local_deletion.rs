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
    decode_base64url, installation_key_thumbprint, pro_graph_record_id, valid_pro_installation_id,
    ProFilesystemLayout, INSTALLATION_PUBLIC_KEY_BYTES, PRO_GRAPH_FILE_NAME,
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

const GRAPH_VARIANTS: [&str; 3] = ["", ".next", ".previous"];
const SQLITE_AUXILIARY_SUFFIXES: [&str; 5] = ["-journal", "-wal", "-shm", "-lock", ".lock"];
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
}

impl LocalDeletionService<PlatformDeletionBackend> {
    pub(super) const fn production() -> Self {
        Self {
            backend: PlatformDeletionBackend,
        }
    }
}

impl<B: DeletionBackend> ProDeletionService for LocalDeletionService<B> {
    fn delete_graph_data(&mut self, data_root: &Path) -> Result<()> {
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
        for target in &targets {
            let graph_id =
                pro_graph_record_id(&installation_id, &target.installation_key_thumbprint)
                    .ok_or_else(|| {
                        anyhow!("key_store_unavailable: local Pro installation identity is invalid")
                    })?;
            self.backend
                .delete_graph_record(data_root, target, &graph_id)?;
        }
        graph.delete()?;
        if GraphArtifacts::inspect(data_root)?.any_present {
            bail!("key_store_unavailable: encrypted Pro graph deletion could not be verified");
        }
        Ok(())
    }

    fn delete_commercial_credentials(&mut self, data_root: &Path) -> Result<()> {
        self.backend.delete_commercial_credentials(data_root)
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
    let path = ProFilesystemLayout::new(data_root).helper_path();
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => bail!("invalid_request: local Pro helper is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("inspect local Pro helper"),
    }
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
        target: &GraphKeyDeletionTarget,
        _graph_id: &str,
    ) -> Result<()> {
        graph_key_deletion::delete_selected(
            data_root,
            target.namespace.credential_vault_namespace(),
            &target.installation_key_thumbprint,
        )
    }

    fn delete_commercial_credentials(&self, data_root: &Path) -> Result<()> {
        commercial_deletion::delete_credentials(data_root)
    }
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
                let graph_root = ProFilesystemLayout::new(data_root).pro_root();
                return Ok(Self {
                    existing_root: None,
                    paths: graph_paths(&graph_root),
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

        let supplied_root = ProFilesystemLayout::new(data_root).pro_root();
        let (graph_root, existing_root) = match supplied_root.symlink_metadata() {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                let canonical = supplied_root
                    .canonicalize()
                    .context("invalid_request: resolve Pro data directory")?;
                (canonical.clone(), Some(canonical))
            }
            Ok(_) => bail!("invalid_request: Pro data directory is not a safe directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
                ProFilesystemLayout::new(&canonical_data_root).pro_root(),
                None,
            ),
            Err(error) => return Err(error).context("invalid_request: inspect Pro data directory"),
        };
        let paths = graph_paths(&graph_root);
        let mut any_present = false;
        if existing_root.is_some() {
            for path in &paths {
                match path.symlink_metadata() {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        any_present = true;
                    }
                    Ok(_) => bail!("invalid_request: Pro graph path is not a regular file"),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).context("invalid_request: inspect Pro graph file")
                    }
                }
            }
        }
        if any_present {
            let root = existing_root.as_ref().ok_or_else(|| {
                anyhow!("invalid_request: Pro graph data exists without a Pro data directory")
            })?;
            let metadata = root
                .symlink_metadata()
                .context("invalid_request: inspect Pro data directory")?;
            validate_graph_root_metadata(root, &metadata)?;
            verify_private_directory(root)
                .context("invalid_request: verify private Pro data directory")?;
        }
        Ok(Self {
            existing_root,
            paths,
            any_present,
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
            .context("invalid_request: reverify private Pro data directory")?;
        for path in self.paths {
            delete_regular_file(&path)?;
        }
        Ok(())
    }
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

fn graph_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths =
        Vec::with_capacity(GRAPH_VARIANTS.len() * (SQLITE_AUXILIARY_SUFFIXES.len() + 1));
    for variant in GRAPH_VARIANTS {
        let base = root.join(format!("{PRO_GRAPH_FILE_NAME}{variant}"));
        paths.push(base.clone());
        for suffix in SQLITE_AUXILIARY_SUFFIXES {
            paths.push(path_with_suffix(&base, suffix));
        }
    }
    paths
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
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
mod tests {
    use std::cell::{Cell, RefCell};

    use ctx_history_core::platform_security::restrict_private_directory;
    use tempfile::tempdir;

    use super::*;

    #[derive(Default)]
    struct RecordingBackend {
        targets: BTreeSet<GraphKeyDeletionTarget>,
        inventory_reads: Cell<usize>,
        corrupt_inventory: bool,
        graph_key_missing: bool,
        fail_graph_key_verification: bool,
        fail_commercial_credentials_after_delete: bool,
        require_cleanup_phase_for_graph_delete: Option<PathBuf>,
        make_root_unsafe_on_graph_delete: Option<PathBuf>,
        deleted: RefCell<Vec<String>>,
        deletion_thumbprints: RefCell<Vec<String>>,
        deletion_namespaces: RefCell<Vec<GraphKeyCredentialNamespace>>,
        credentials_deleted: RefCell<bool>,
    }

    impl DeletionBackend for RecordingBackend {
        fn graph_key_deletion_targets(
            &self,
            _data_root: &Path,
        ) -> Result<BTreeSet<GraphKeyDeletionTarget>> {
            self.inventory_reads
                .set(self.inventory_reads.get().saturating_add(1));
            if self.corrupt_inventory {
                return Err(vault_error(CredentialVaultError::Corrupt));
            }
            Ok(self.targets.clone())
        }

        fn delete_graph_record(
            &self,
            _data_root: &Path,
            target: &GraphKeyDeletionTarget,
            graph_id: &str,
        ) -> Result<()> {
            if let Some(root) = &self.require_cleanup_phase_for_graph_delete {
                match local_pro_graph_key_cleanup_phase_exists(root) {
                    Ok(true) => {}
                    Ok(false) | Err(_) => {
                        bail!("key_store_unavailable: cleanup phase was not durable")
                    }
                }
            }
            self.deleted.borrow_mut().push(graph_id.to_owned());
            self.deletion_thumbprints
                .borrow_mut()
                .push(target.installation_key_thumbprint.clone());
            self.deletion_namespaces.borrow_mut().push(target.namespace);
            if let Some(root) = &self.make_root_unsafe_on_graph_delete {
                make_private_directory_unsafe(root)
                    .context("key_store_unavailable: make graph root unsafe")?;
            }
            if self.fail_graph_key_verification {
                bail!("key_store_unavailable: simulated graph-key verification failure");
            }
            let _ = self.graph_key_missing;
            Ok(())
        }

        fn delete_commercial_credentials(&self, _data_root: &Path) -> Result<()> {
            self.credentials_deleted.replace(true);
            if self.fail_commercial_credentials_after_delete {
                bail!("key_store_unavailable: simulated late credential deletion failure");
            }
            Ok(())
        }
    }

    fn test_thumbprint(seed: u8) -> String {
        installation_key_thumbprint(
            &SigningKey::from_bytes(&[seed; INSTALLATION_PUBLIC_KEY_BYTES])
                .verifying_key()
                .to_bytes(),
        )
    }

    fn test_target(namespace: GraphKeyCredentialNamespace, seed: u8) -> GraphKeyDeletionTarget {
        GraphKeyDeletionTarget {
            namespace,
            installation_key_thumbprint: test_thumbprint(seed),
        }
    }

    struct ValidKeyCorruptEntitlementReader {
        loads: Cell<usize>,
    }

    impl CredentialRecordReader for ValidKeyCorruptEntitlementReader {
        fn load_record(
            &self,
            kind: CredentialRecordKind,
        ) -> Result<CredentialRecord, CredentialVaultError> {
            self.loads.set(self.loads.get().saturating_add(1));
            match kind {
                CredentialRecordKind::InstallationSigningKey => {
                    Ok(CredentialRecord::InstallationSigningKey(
                        super::super::credential_vault::InstallationSigningKeySeed::from_bytes(
                            [10; INSTALLATION_PUBLIC_KEY_BYTES],
                        ),
                    ))
                }
                CredentialRecordKind::SignedEntitlement => Err(CredentialVaultError::Corrupt),
                CredentialRecordKind::WorkOsSession | CredentialRecordKind::AnonymousTrial => {
                    Err(CredentialVaultError::Backend)
                }
            }
        }
    }

    struct MismatchedKeyAndEntitlementReader;

    impl CredentialRecordReader for MismatchedKeyAndEntitlementReader {
        fn load_record(
            &self,
            kind: CredentialRecordKind,
        ) -> Result<CredentialRecord, CredentialVaultError> {
            match kind {
                CredentialRecordKind::InstallationSigningKey => {
                    Ok(CredentialRecord::InstallationSigningKey(
                        super::super::credential_vault::InstallationSigningKeySeed::from_bytes(
                            [20; INSTALLATION_PUBLIC_KEY_BYTES],
                        ),
                    ))
                }
                CredentialRecordKind::SignedEntitlement => {
                    use ctx_pro_host_protocol::{
                        base64url, EntitlementAccessKind, EntitlementCapability, EntitlementGrant,
                        SignedEntitlement, ED25519_SIGNATURE_BYTES, ENTITLEMENT_SCHEMA_VERSION,
                        PROTOCOL_VERSION,
                    };

                    let public_key = SigningKey::from_bytes(&[21; INSTALLATION_PUBLIC_KEY_BYTES])
                        .verifying_key()
                        .to_bytes();
                    let entitlement = SignedEntitlement {
                        grant: EntitlementGrant {
                            schema_version: ENTITLEMENT_SCHEMA_VERSION,
                            issuer: "https://commercial.staging.ctx.rs".to_owned(),
                            key_id: "staging-2026-07-v2".to_owned(),
                            grant_id: "grant-mismatch".to_owned(),
                            subject: "subject".to_owned(),
                            account_id: "account".to_owned(),
                            product: "ctx-local-pro".to_owned(),
                            access_kind: EntitlementAccessKind::Active,
                            installation_key_thumbprint: installation_key_thumbprint(&public_key),
                            issued_at_unix: 1_800_000_000,
                            not_before_unix: 1_799_999_700,
                            refresh_after_unix: 1_800_345_600,
                            access_deadline_unix: 1_802_592_000,
                            grace_deadline_unix: 1_803_196_800,
                            expires_at_unix: 1_800_604_800,
                            minimum_helper_protocol: PROTOCOL_VERSION,
                            revocation_epoch: 0,
                            capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
                        },
                        signature_base64url: base64url(&[7; ED25519_SIGNATURE_BYTES]),
                    };
                    Ok(CredentialRecord::SignedEntitlement(
                        super::super::credential_vault::BoundedSignedEntitlement::new(entitlement)?,
                    ))
                }
                CredentialRecordKind::WorkOsSession | CredentialRecordKind::AnonymousTrial => {
                    Err(CredentialVaultError::Backend)
                }
            }
        }
    }

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempdir().unwrap();
        crate::identity::installation_id(root.path()).unwrap();
        let pro = ProFilesystemLayout::new(root.path()).pro_root();
        fs::create_dir(&pro).unwrap();
        restrict_private_directory(&pro).unwrap();
        fs::write(pro.join(PRO_GRAPH_FILE_NAME), "ciphertext").unwrap();
        (root, pro)
    }

    #[cfg(unix)]
    fn make_private_directory_unsafe(path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
    }

    #[cfg(windows)]
    fn make_private_directory_unsafe(path: &Path) -> std::io::Result<()> {
        let status = std::process::Command::new("icacls.exe")
            .arg(path)
            .args(["/grant", "*S-1-1-0:F"])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(
                "failed to make Windows Pro directory ACL unsafe",
            ))
        }
    }

    #[test]
    fn direct_deletion_accepts_an_already_missing_graph_key() {
        let (root, pro) = fixture();
        let backend = RecordingBackend {
            targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 1)]),
            graph_key_missing: true,
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        service.delete_graph_data(root.path()).unwrap();
        assert!(!pro.join(PRO_GRAPH_FILE_NAME).exists());
        assert_eq!(service.backend.deleted.borrow().len(), 1);
    }

    #[test]
    fn mixed_valid_and_corrupt_record_inventory_fails_closed() {
        let reader = ValidKeyCorruptEntitlementReader {
            loads: Cell::new(0),
        };
        let error = recorded_graph_key_targets(GraphKeyCredentialNamespace::Production, &reader)
            .unwrap_err();
        assert!(error.to_string().starts_with("key_store_unavailable:"));
        assert_eq!(reader.loads.get(), 2);
    }

    #[test]
    fn mismatched_key_and_entitlement_in_one_namespace_fail_closed() {
        let error = recorded_graph_key_targets(
            GraphKeyCredentialNamespace::Staging,
            &MismatchedKeyAndEntitlementReader,
        )
        .unwrap_err();
        assert!(error.to_string().starts_with("key_store_unavailable:"));
    }

    #[test]
    fn mixed_valid_and_corrupt_namespace_inventory_deletes_nothing() {
        let (root, pro) = fixture();
        let backend = RecordingBackend {
            // Model a valid thumbprint observed in one exact namespace before a
            // corrupt record makes the complete two-namespace inventory unverifiable.
            targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 11)]),
            corrupt_inventory: true,
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        let error = service.delete_graph_data(root.path()).unwrap_err();
        assert!(error.to_string().starts_with("key_store_unavailable:"));
        assert_eq!(service.backend.inventory_reads.get(), 1);
        assert!(service.backend.deleted.borrow().is_empty());
        assert!(pro.join(PRO_GRAPH_FILE_NAME).exists());
        assert!(!local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
    }

    #[test]
    fn missing_or_empty_roots_are_vault_free_idempotent_noops() {
        let parent = tempdir().unwrap();
        let missing = parent.path().join("missing");
        let mut missing_service = LocalDeletionService {
            backend: RecordingBackend::default(),
        };
        missing_service.delete_graph_data(&missing).unwrap();
        assert!(!missing.exists());
        assert!(missing_service.backend.deleted.borrow().is_empty());
        assert_eq!(missing_service.backend.inventory_reads.get(), 0);

        let empty = tempdir().unwrap();
        crate::identity::installation_id(empty.path()).unwrap();
        let empty_pro = ProFilesystemLayout::new(empty.path()).pro_root();
        fs::create_dir(&empty_pro).unwrap();
        let mut empty_service = LocalDeletionService {
            backend: RecordingBackend::default(),
        };
        empty_service.delete_graph_data(empty.path()).unwrap();
        assert!(empty_pro.is_dir());
        assert!(empty_service.backend.deleted.borrow().is_empty());
        assert_eq!(empty_service.backend.inventory_reads.get(), 0);
    }

    #[test]
    fn helper_present_without_graph_deletes_recorded_graph_keys() {
        let root = tempdir().unwrap();
        let installation_id = crate::identity::installation_id(root.path()).unwrap();
        let layout = ProFilesystemLayout::new(root.path());
        let pro = layout.pro_root();
        fs::create_dir(&pro).unwrap();
        restrict_private_directory(&pro).unwrap();
        fs::create_dir(layout.bin_dir()).unwrap();
        fs::write(layout.helper_path(), b"signed helper").unwrap();

        let targets = BTreeSet::from([
            test_target(GraphKeyCredentialNamespace::Production, 2),
            test_target(GraphKeyCredentialNamespace::Staging, 3),
        ]);
        let expected_graph_ids = targets
            .iter()
            .map(|target| {
                pro_graph_record_id(&installation_id, &target.installation_key_thumbprint).unwrap()
            })
            .collect::<Vec<_>>();
        let expected_thumbprints = targets
            .iter()
            .map(|target| target.installation_key_thumbprint.clone())
            .collect::<Vec<_>>();
        let expected_namespaces = targets
            .iter()
            .map(|target| target.namespace)
            .collect::<Vec<_>>();
        let backend = RecordingBackend {
            targets,
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        service.delete_graph_data(root.path()).unwrap();

        assert_eq!(service.backend.inventory_reads.get(), 1);
        assert_eq!(
            service.backend.deleted.borrow().as_slice(),
            expected_graph_ids
        );
        assert_eq!(
            service.backend.deletion_thumbprints.borrow().as_slice(),
            expected_thumbprints
        );
        assert_eq!(
            service.backend.deletion_namespaces.borrow().as_slice(),
            expected_namespaces
        );
        assert!(layout.helper_path().exists());
        assert!(!layout.graph_path().exists());
    }

    #[test]
    fn cleanup_phase_is_durable_before_graph_key_deletion() {
        let (root, _) = fixture();
        let backend = RecordingBackend {
            targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 12)]),
            require_cleanup_phase_for_graph_delete: Some(root.path().to_path_buf()),
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        service.delete_graph_data(root.path()).unwrap();
        assert_eq!(service.backend.deleted.borrow().len(), 1);
        assert!(local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
    }

    #[test]
    fn late_failure_retries_from_cleanup_phase_after_records_are_gone() {
        let (root, pro) = fixture();
        let target = test_target(GraphKeyCredentialNamespace::Staging, 13);
        let first_backend = RecordingBackend {
            targets: BTreeSet::from([target]),
            fail_commercial_credentials_after_delete: true,
            ..RecordingBackend::default()
        };
        let mut first = LocalDeletionService {
            backend: first_backend,
        };
        first.delete_graph_data(root.path()).unwrap();
        assert!(!pro.join(PRO_GRAPH_FILE_NAME).exists());
        assert!(first.delete_commercial_credentials(root.path()).is_err());
        assert!(*first.backend.credentials_deleted.borrow());
        assert!(local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());

        let retry_backend = RecordingBackend {
            corrupt_inventory: true,
            graph_key_missing: true,
            ..RecordingBackend::default()
        };
        let mut retry = LocalDeletionService {
            backend: retry_backend,
        };
        retry.delete_graph_data(root.path()).unwrap();
        assert_eq!(retry.backend.inventory_reads.get(), 0);
        assert_eq!(retry.backend.deleted.borrow().len(), 1);
        retry.delete_commercial_credentials(root.path()).unwrap();
        retry.finish_deletion(root.path()).unwrap();
        assert!(!local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
    }

    #[test]
    fn cleanup_phase_cannot_cross_installation_identities() {
        let (root, pro) = fixture();
        let other_installation_id = "5d98d375-4ac4-4507-be4b-c435e373f042";
        write_graph_key_cleanup_phase(
            root.path(),
            other_installation_id,
            &BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 14)]),
        )
        .unwrap();
        let mut service = LocalDeletionService {
            backend: RecordingBackend::default(),
        };
        let error = service.delete_graph_data(root.path()).unwrap_err();
        assert!(error.to_string().starts_with("key_store_unavailable:"));
        assert!(service.backend.deleted.borrow().is_empty());
        assert!(pro.join(PRO_GRAPH_FILE_NAME).exists());
    }

    #[test]
    fn empty_cleanup_phase_cannot_delete_graph_artifacts() {
        let (root, pro) = fixture();
        let installation_id = crate::identity::existing_installation_id(root.path())
            .unwrap()
            .unwrap();
        write_graph_key_cleanup_phase(root.path(), &installation_id, &BTreeSet::new()).unwrap();
        let mut service = LocalDeletionService {
            backend: RecordingBackend::default(),
        };
        let error = service.delete_graph_data(root.path()).unwrap_err();
        assert!(error.to_string().starts_with("key_store_unavailable:"));
        assert!(service.backend.deleted.borrow().is_empty());
        assert!(pro.join(PRO_GRAPH_FILE_NAME).exists());
    }

    #[test]
    fn graph_family_and_interrupted_rebuild_files_are_all_removed() {
        let (root, pro) = fixture();
        for path in graph_paths(&pro) {
            fs::write(path, "ciphertext").unwrap();
        }
        let backend = RecordingBackend {
            targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 4)]),
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        service.delete_graph_data(root.path()).unwrap();
        assert!(graph_paths(&pro).iter().all(|path| !path.exists()));
    }

    #[test]
    fn failed_authoritative_key_inventory_verification_preserves_graph_files() {
        let (root, pro) = fixture();
        let backend = RecordingBackend {
            targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 5)]),
            fail_graph_key_verification: true,
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        assert!(service.delete_graph_data(root.path()).is_err());
        assert!(pro.join(PRO_GRAPH_FILE_NAME).exists());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn graph_root_is_reverified_immediately_before_file_removal() {
        let (root, pro) = fixture();
        let graph = pro.join(PRO_GRAPH_FILE_NAME);
        let backend = RecordingBackend {
            targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 6)]),
            make_root_unsafe_on_graph_delete: Some(pro.clone()),
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        assert!(service.delete_graph_data(root.path()).is_err());
        assert_eq!(service.backend.deleted.borrow().len(), 1);
        assert!(graph.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_graph_or_graph_root_fails_before_key_deletion() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        crate::identity::installation_id(root.path()).unwrap();
        symlink(
            outside.path(),
            ProFilesystemLayout::new(root.path()).pro_root(),
        )
        .unwrap();
        let backend = RecordingBackend {
            targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 7)]),
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        assert!(service.delete_graph_data(root.path()).is_err());
        assert!(service.backend.deleted.borrow().is_empty());

        let pro_root = ProFilesystemLayout::new(root.path()).pro_root();
        fs::remove_file(&pro_root).unwrap();
        fs::create_dir(&pro_root).unwrap();
        fs::set_permissions(
            &pro_root,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let outside_file = outside.path().join("graph");
        fs::write(&outside_file, "outside").unwrap();
        symlink(&outside_file, pro_root.join(PRO_GRAPH_FILE_NAME)).unwrap();
        assert!(service.delete_graph_data(root.path()).is_err());
        assert!(service.backend.deleted.borrow().is_empty());
        assert_eq!(fs::read(outside_file).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn shared_graph_root_fails_before_key_deletion() {
        use std::os::unix::fs::PermissionsExt as _;

        let (root, pro) = fixture();
        fs::set_permissions(&pro, fs::Permissions::from_mode(0o755)).unwrap();
        let backend = RecordingBackend {
            targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 8)]),
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        assert!(service.delete_graph_data(root.path()).is_err());
        assert!(service.backend.deleted.borrow().is_empty());
        assert!(pro.join(PRO_GRAPH_FILE_NAME).exists());
    }

    #[cfg(windows)]
    #[test]
    fn shared_graph_root_fails_before_key_derivation_on_windows() {
        let (root, pro) = fixture();
        make_private_directory_unsafe(&pro).unwrap();
        let backend = RecordingBackend {
            targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 9)]),
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        assert!(service.delete_graph_data(root.path()).is_err());
        assert!(service.backend.deleted.borrow().is_empty());
        assert!(pro.join(PRO_GRAPH_FILE_NAME).exists());
    }

    #[test]
    fn graph_identity_matches_private_protocol_v1_format() {
        let id = pro_graph_record_id("6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8", "thumbprint").unwrap();
        assert_eq!(
            id,
            "ctx-pro-installation-graph-v1:6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8:thumbprint"
        );
    }

    #[test]
    fn missing_identity_fails_closed_while_ciphertext_remains() {
        let (root, pro) = fixture();
        let mut service = LocalDeletionService {
            backend: RecordingBackend::default(),
        };
        assert!(service
            .delete_graph_data(root.path())
            .unwrap_err()
            .to_string()
            .starts_with("key_store_unavailable:"));
        assert!(pro.join(PRO_GRAPH_FILE_NAME).exists());
    }
}

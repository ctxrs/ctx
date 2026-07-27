use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, bail, Context as _, Result};
use ctx_history_core::platform_security::verify_private_directory;
use ctx_pro_host_protocol::{
    installation_key_thumbprint, pro_graph_record_id, ProFilesystemLayout, PRO_GRAPH_FILE_NAME,
};
use ed25519_dalek::SigningKey;

use super::{
    credential_vault::{
        CredentialRecord, CredentialRecordKind, CredentialVaultError, CredentialVaultNamespace,
        PlatformCredentialVault,
    },
    graph_key_deletion,
    lifecycle::ProDeletionService,
};

const GRAPH_VARIANTS: [&str; 3] = ["", ".next", ".previous"];
const SQLITE_AUXILIARY_SUFFIXES: [&str; 5] = ["-journal", "-wal", "-shm", "-lock", ".lock"];

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
        let installation_id = crate::identity::existing_installation_id(data_root)
            .context("key_store_unavailable: load local Pro installation identity")?
            .ok_or_else(|| {
                anyhow!("key_store_unavailable: local Pro installation identity is missing")
            })?;
        let thumbprints = self.backend.installation_thumbprints(data_root)?;
        if graph.any_present && thumbprints.is_empty() {
            bail!(
                "key_store_unavailable: installation identity is missing while encrypted Pro graph data remains"
            );
        }
        for thumbprint in thumbprints {
            let graph_id = pro_graph_record_id(&installation_id, &thumbprint).ok_or_else(|| {
                anyhow!("key_store_unavailable: local Pro installation identity is invalid")
            })?;
            match self.backend.delete_graph_record(&graph_id) {
                Ok(()) | Err(CredentialVaultError::NotFound) => {}
                Err(error) => return Err(vault_error(error)),
            }
            self.backend
                .verify_graph_record_absent(&graph_id)
                .map_err(vault_error)?;
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
}

trait DeletionBackend {
    fn installation_thumbprints(&self, data_root: &Path) -> Result<BTreeSet<String>>;
    fn delete_graph_record(&self, graph_id: &str) -> Result<(), CredentialVaultError>;
    fn verify_graph_record_absent(&self, graph_id: &str) -> Result<(), CredentialVaultError>;
    fn delete_commercial_credentials(&self, data_root: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PlatformDeletionBackend;

impl DeletionBackend for PlatformDeletionBackend {
    fn installation_thumbprints(&self, data_root: &Path) -> Result<BTreeSet<String>> {
        let mut thumbprints = BTreeSet::new();
        for namespace in [
            CredentialVaultNamespace::Production,
            CredentialVaultNamespace::Staging,
        ] {
            let vault =
                PlatformCredentialVault::production(data_root, namespace).map_err(vault_error)?;
            if let Some(thumbprint) = deletion_thumbprint(&vault)? {
                thumbprints.insert(thumbprint);
            }
        }
        Ok(thumbprints)
    }

    fn delete_graph_record(&self, graph_id: &str) -> Result<(), CredentialVaultError> {
        graph_key_deletion::delete(graph_id)
    }

    fn verify_graph_record_absent(&self, graph_id: &str) -> Result<(), CredentialVaultError> {
        match graph_key_deletion::delete(graph_id) {
            Err(CredentialVaultError::NotFound) => Ok(()),
            Ok(()) => Err(CredentialVaultError::Backend),
            Err(error) => Err(error),
        }
    }

    fn delete_commercial_credentials(&self, data_root: &Path) -> Result<()> {
        for namespace in [
            CredentialVaultNamespace::Production,
            CredentialVaultNamespace::Staging,
        ] {
            let vault =
                PlatformCredentialVault::production(data_root, namespace).map_err(vault_error)?;
            for kind in [
                CredentialRecordKind::WorkOsSession,
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
                        bail!("key_store_unavailable: local Pro credential deletion could not be verified")
                    }
                    Err(error) => return Err(vault_error(error)),
                }
            }
        }
        Ok(())
    }
}

fn deletion_thumbprint(vault: &PlatformCredentialVault) -> Result<Option<String>> {
    match vault.load(CredentialRecordKind::InstallationSigningKey) {
        Ok(CredentialRecord::InstallationSigningKey(seed)) => {
            let public_key = SigningKey::from_bytes(seed.expose())
                .verifying_key()
                .to_bytes();
            Ok(Some(installation_key_thumbprint(&public_key)))
        }
        Ok(_) => Err(anyhow!(
            "key_store_unavailable: installation key record has the wrong type"
        )),
        Err(CredentialVaultError::NotFound | CredentialVaultError::Corrupt) => {
            entitlement_thumbprint(vault)
        }
        Err(error) => Err(vault_error(error)),
    }
}

fn entitlement_thumbprint(vault: &PlatformCredentialVault) -> Result<Option<String>> {
    match vault.load(CredentialRecordKind::SignedEntitlement) {
        Ok(CredentialRecord::SignedEntitlement(entitlement)) => Ok(Some(
            entitlement
                .as_inner()
                .grant
                .installation_key_thumbprint
                .clone(),
        )),
        Ok(_) => Err(anyhow!(
            "key_store_unavailable: signed entitlement record has the wrong type"
        )),
        Err(CredentialVaultError::NotFound) => Ok(None),
        Err(error) => Err(vault_error(error)),
    }
}

fn vault_error(error: CredentialVaultError) -> anyhow::Error {
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
                validate_graph_root_metadata(&canonical, &metadata)?;
                verify_private_directory(&canonical)
                    .context("invalid_request: verify private Pro data directory")?;
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
    use std::cell::RefCell;

    use ctx_history_core::platform_security::restrict_private_directory;
    use tempfile::tempdir;

    use super::*;

    #[derive(Default)]
    struct RecordingBackend {
        thumbprints: BTreeSet<String>,
        graph_key_missing: bool,
        fail_graph_key_verification: bool,
        make_root_unsafe_on_graph_delete: Option<PathBuf>,
        deleted: RefCell<Vec<String>>,
        credentials_deleted: RefCell<bool>,
    }

    impl DeletionBackend for RecordingBackend {
        fn installation_thumbprints(&self, _data_root: &Path) -> Result<BTreeSet<String>> {
            Ok(self.thumbprints.clone())
        }

        fn delete_graph_record(&self, graph_id: &str) -> Result<(), CredentialVaultError> {
            self.deleted.borrow_mut().push(graph_id.to_owned());
            if let Some(root) = &self.make_root_unsafe_on_graph_delete {
                make_private_directory_unsafe(root).map_err(|_| CredentialVaultError::Backend)?;
            }
            if self.graph_key_missing {
                Err(CredentialVaultError::NotFound)
            } else {
                Ok(())
            }
        }

        fn verify_graph_record_absent(&self, _graph_id: &str) -> Result<(), CredentialVaultError> {
            if self.fail_graph_key_verification {
                Err(CredentialVaultError::Backend)
            } else {
                Ok(())
            }
        }

        fn delete_commercial_credentials(&self, _data_root: &Path) -> Result<()> {
            self.credentials_deleted.replace(true);
            Ok(())
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
            thumbprints: BTreeSet::from(["thumbprint".to_owned()]),
            graph_key_missing: true,
            ..RecordingBackend::default()
        };
        let mut service = LocalDeletionService { backend };
        service.delete_graph_data(root.path()).unwrap();
        assert!(!pro.join(PRO_GRAPH_FILE_NAME).exists());
        assert_eq!(service.backend.deleted.borrow().len(), 1);
    }

    #[test]
    fn graph_family_and_interrupted_rebuild_files_are_all_removed() {
        let (root, pro) = fixture();
        for path in graph_paths(&pro) {
            fs::write(path, "ciphertext").unwrap();
        }
        let backend = RecordingBackend {
            thumbprints: BTreeSet::from(["thumbprint".to_owned()]),
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
            thumbprints: BTreeSet::from(["thumbprint".to_owned()]),
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
            thumbprints: BTreeSet::from(["thumbprint".to_owned()]),
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
            thumbprints: BTreeSet::from(["thumbprint".to_owned()]),
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
            thumbprints: BTreeSet::from(["thumbprint".to_owned()]),
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
            thumbprints: BTreeSet::from(["thumbprint".to_owned()]),
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

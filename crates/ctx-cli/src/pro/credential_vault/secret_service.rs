use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use secret_service::blocking::{Collection, Item, SecretService};
use secret_service::{EncryptionType, Error as SecretServiceError};

use super::{validate_record_id, CredentialVaultBackend, CredentialVaultError, SecretBytes};

const SERVICE: &str = "com.ctx.pro.credentials.v1";
const KIND: &str = "ctx-pro-commercial-credential-v1";
const CONTENT_TYPE: &str = "application/octet-stream";
const LABEL: &str = "ctx Pro credential";
const LOCK_FILE: &str = ".ctx-pro-credential-vault-v1.lock";
const PLATFORM: &str = std::env::consts::OS;
#[cfg(target_os = "linux")]
const SECRET_SERVICE_BUS_NAME: &str = "org.freedesktop.secrets";

#[derive(Debug, Clone, Copy)]
pub(super) struct PlatformBackend;

impl PlatformBackend {
    pub(super) const fn production() -> Self {
        Self
    }

    #[cfg(target_os = "linux")]
    pub(super) fn probe(&self) -> Result<(), CredentialVaultError> {
        require_secret_service_provider()?;
        let service = connect()?;
        persistent_default_collection(&service).map(|_| ())
    }

    fn load_unlocked(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        with_unique_item(record_id, |item| {
            let raw = item.get_secret().map_err(map_secret_service_error)?;
            SecretBytes::new(raw)
        })
    }

    fn store_unlocked(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
        let service = connect()?;
        let matches = find_items(&service, record_id)?;
        match matches.as_slice() {
            [] => {
                persistent_default_collection(&service)?
                    .create_item(LABEL, attributes(record_id), value, true, CONTENT_TYPE)
                    .map_err(map_secret_service_error)?;
                Ok(())
            }
            [item] => item
                .set_secret(value, CONTENT_TYPE)
                .map_err(map_secret_service_error),
            _ => Err(CredentialVaultError::Ambiguous),
        }
    }
}

impl CredentialVaultBackend for PlatformBackend {
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        self.load_unlocked(record_id)
    }

    fn load_or_store(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        drop(SecretBytes::new(candidate.to_vec())?);
        with_vault_lock(|| match self.load_unlocked(record_id) {
            Ok(existing) => Ok(existing),
            Err(CredentialVaultError::NotFound) => {
                self.store_unlocked(record_id, candidate)?;
                self.load_unlocked(record_id)
            }
            Err(error) => Err(error),
        })
    }

    fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        drop(SecretBytes::new(value.to_vec())?);
        with_vault_lock(|| {
            self.store_unlocked(record_id, value)?;
            let persisted = self.load_unlocked(record_id)?;
            if persisted.as_slice() != value {
                return Err(CredentialVaultError::Backend);
            }
            Ok(())
        })
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        with_vault_lock(|| {
            with_unique_item(record_id, |item| {
                item.delete().map_err(map_secret_service_error)
            })
        })
    }
}

fn connect() -> Result<SecretService<'static>, CredentialVaultError> {
    SecretService::connect(EncryptionType::Dh).map_err(map_secret_service_error)
}

fn with_unique_item<T>(
    record_id: &str,
    operation: impl FnOnce(&Item<'_>) -> Result<T, CredentialVaultError>,
) -> Result<T, CredentialVaultError> {
    let service = connect()?;
    let matches = find_items(&service, record_id)?;
    match matches.as_slice() {
        [] => Err(CredentialVaultError::NotFound),
        [item] => operation(item),
        _ => Err(CredentialVaultError::Ambiguous),
    }
}

fn find_items<'a>(
    service: &'a SecretService<'a>,
    record_id: &str,
) -> Result<Vec<Item<'a>>, CredentialVaultError> {
    let collection = persistent_default_collection(service)?;
    let collection_path = collection.collection_path.as_str();
    let matches = service
        .search_items(attributes(record_id))
        .map_err(map_secret_service_error)?;
    if matches
        .locked
        .iter()
        .any(|item| belongs_to_collection(item.item_path.as_str(), collection_path))
    {
        return Err(CredentialVaultError::Locked);
    }
    Ok(matches
        .unlocked
        .into_iter()
        .filter(|item| belongs_to_collection(item.item_path.as_str(), collection_path))
        .collect())
}

fn persistent_default_collection<'a>(
    service: &'a SecretService<'a>,
) -> Result<Collection<'a>, CredentialVaultError> {
    let collection = service
        .get_default_collection()
        .map_err(map_persistent_collection_error)?;
    match service.get_collection_by_alias("session") {
        Ok(session) if session.collection_path == collection.collection_path => {
            return Err(persistent_collection_unavailable());
        }
        Ok(_) | Err(SecretServiceError::NoResult) => {}
        Err(error) => return Err(map_secret_service_error(error)),
    }
    if collection
        .is_locked()
        .map_err(map_persistent_collection_object_error)?
    {
        return Err(CredentialVaultError::Locked);
    }
    Ok(collection)
}

fn attributes(record_id: &str) -> HashMap<&str, &str> {
    HashMap::from([
        ("service", SERVICE),
        ("username", record_id),
        ("ctx-pro-kind", KIND),
    ])
}

fn belongs_to_collection(item_path: &str, collection_path: &str) -> bool {
    item_path
        .strip_prefix(collection_path)
        .is_some_and(|relative| relative.starts_with('/'))
}

fn map_secret_service_error(error: SecretServiceError) -> CredentialVaultError {
    match error {
        SecretServiceError::Locked | SecretServiceError::Prompt => CredentialVaultError::Locked,
        #[cfg(target_os = "freebsd")]
        SecretServiceError::Unavailable => unavailable(),
        _ => CredentialVaultError::Backend,
    }
}

fn map_persistent_collection_error(error: SecretServiceError) -> CredentialVaultError {
    if matches!(error, SecretServiceError::NoResult) {
        persistent_collection_unavailable()
    } else {
        map_secret_service_error(error)
    }
}

#[cfg(target_os = "linux")]
fn map_persistent_collection_object_error(error: SecretServiceError) -> CredentialVaultError {
    if matches!(
        &error,
        SecretServiceError::ZbusFdo(
            zbus::fdo::Error::UnknownMethod(_) | zbus::fdo::Error::UnknownObject(_)
        )
    ) {
        drop(error);
        persistent_collection_unavailable()
    } else {
        map_secret_service_error(error)
    }
}

#[cfg(target_os = "freebsd")]
fn map_persistent_collection_object_error(error: SecretServiceError) -> CredentialVaultError {
    map_secret_service_error(error)
}

const fn persistent_collection_unavailable() -> CredentialVaultError {
    #[cfg(target_os = "linux")]
    {
        unavailable()
    }
    #[cfg(target_os = "freebsd")]
    {
        unavailable()
    }
}

const fn unavailable() -> CredentialVaultError {
    CredentialVaultError::Unavailable { platform: PLATFORM }
}

#[cfg(target_os = "linux")]
fn require_secret_service_provider() -> Result<(), CredentialVaultError> {
    let connection =
        zbus::blocking::Connection::session().map_err(map_session_bus_connection_error)?;
    let proxy = zbus::blocking::fdo::DBusProxy::new(&connection)
        .map_err(|_| CredentialVaultError::Backend)?;
    let name = zbus::names::BusName::try_from(SECRET_SERVICE_BUS_NAME)
        .map_err(|_| CredentialVaultError::Backend)?;
    let has_owner = proxy
        .name_has_owner(name)
        .map_err(|_| CredentialVaultError::Backend)?;
    let activatable = if has_owner {
        true
    } else {
        proxy
            .list_activatable_names()
            .map_err(|_| CredentialVaultError::Backend)?
            .iter()
            .any(|name| name.as_str() == SECRET_SERVICE_BUS_NAME)
    };
    classify_secret_service_provider(has_owner, activatable)
}

#[cfg(target_os = "linux")]
fn map_session_bus_connection_error(error: zbus::Error) -> CredentialVaultError {
    let deterministically_absent = matches!(
        &error,
        zbus::Error::InputOutput(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            )
    );
    drop(error);
    if deterministically_absent {
        unavailable()
    } else {
        CredentialVaultError::Backend
    }
}

#[cfg(target_os = "linux")]
const fn classify_secret_service_provider(
    has_owner: bool,
    activatable: bool,
) -> Result<(), CredentialVaultError> {
    if has_owner || activatable {
        Ok(())
    } else {
        Err(unavailable())
    }
}

fn with_vault_lock<T>(
    operation: impl FnOnce() -> Result<T, CredentialVaultError>,
) -> Result<T, CredentialVaultError> {
    let path = initialization_lock_path()?;
    let file = open_lock_file(&path)?;
    fs2::FileExt::lock_exclusive(&file).map_err(|_| CredentialVaultError::Backend)?;
    let result = operation();
    let unlock = fs2::FileExt::unlock(&file).map_err(|_| CredentialVaultError::Backend);
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn initialization_lock_path() -> Result<PathBuf, CredentialVaultError> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(unavailable)?;
    lock_path_in_runtime_dir(&runtime_dir)
}

fn lock_path_in_runtime_dir(runtime_dir: &Path) -> Result<PathBuf, CredentialVaultError> {
    let metadata = runtime_dir.symlink_metadata().map_err(|_| unavailable())?;
    if !runtime_dir.is_absolute()
        || !metadata.file_type().is_dir()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != effective_uid()
    {
        return Err(unavailable());
    }
    Ok(runtime_dir.join(LOCK_FILE))
}

fn open_lock_file(path: &Path) -> Result<File, CredentialVaultError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| unavailable())?;
    let metadata = file.metadata().map_err(|_| unavailable())?;
    if !metadata.file_type().is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != effective_uid()
        || metadata.nlink() != 1
    {
        return Err(unavailable());
    }
    Ok(file)
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no arguments and no failure mode on supported Unix targets.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::os::unix::fs::symlink;
    use std::sync::Mutex;
    use zeroize::Zeroize as _;

    use ctx_pro_host_protocol::{
        base64url, installation_key_thumbprint, EntitlementAccessKind, EntitlementCapability,
        EntitlementGrant, SignedEntitlement, ED25519_SIGNATURE_BYTES,
    };

    use super::super::{
        load_record, store_record, BoundedSignedEntitlement, CredentialRecord, CredentialRecordIds,
        CredentialRecordKind, CredentialVaultNamespace, WorkOsSessionMaterial,
        ENTITLEMENT_SCHEMA_VERSION, INSTALLATION_PUBLIC_KEY_BYTES, MAX_ID_BYTES, PROTOCOL_VERSION,
    };
    use super::*;

    #[derive(Default)]
    struct MemoryBackend(Mutex<HashMap<String, Vec<u8>>>);

    impl CredentialVaultBackend for MemoryBackend {
        fn load(&self, id: &str) -> Result<SecretBytes, CredentialVaultError> {
            validate_record_id(id)?;
            let values = self.0.lock().map_err(|_| CredentialVaultError::Backend)?;
            SecretBytes::new(
                values
                    .get(id)
                    .cloned()
                    .ok_or(CredentialVaultError::NotFound)?,
            )
        }

        fn load_or_store(
            &self,
            id: &str,
            candidate: &[u8],
        ) -> Result<SecretBytes, CredentialVaultError> {
            validate_record_id(id)?;
            let mut values = self.0.lock().map_err(|_| CredentialVaultError::Backend)?;
            let value = values
                .entry(id.to_owned())
                .or_insert_with(|| candidate.to_vec());
            SecretBytes::new(value.clone())
        }

        fn store(&self, id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
            validate_record_id(id)?;
            self.0
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?
                .insert(id.to_owned(), value.to_vec());
            Ok(())
        }

        fn delete(&self, id: &str) -> Result<(), CredentialVaultError> {
            validate_record_id(id)?;
            self.0
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?
                .remove(id)
                .map(|mut value| value.zeroize())
                .ok_or(CredentialVaultError::NotFound)
        }
    }

    #[test]
    fn typed_workos_record_round_trips_under_an_opaque_id() -> anyhow::Result<()> {
        let backend = MemoryBackend::default();
        let record_ids = CredentialRecordIds::new(
            "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8",
            CredentialVaultNamespace::Production,
        )?;
        store_record(
            &backend,
            &record_ids,
            &CredentialRecord::WorkOsSession(
                WorkOsSessionMaterial::new(
                    "access-token".to_owned(),
                    Some("refresh-token".to_owned()),
                    1_800_000_000,
                )?
                .with_entitlement_refresh_not_before_unix(Some(1_799_000_000))?,
            ),
        )?;
        let kind = CredentialRecordKind::WorkOsSession;
        let CredentialRecord::WorkOsSession(loaded) = load_record(
            &backend,
            record_ids.get(kind),
            CredentialRecordKind::WorkOsSession,
        )?
        else {
            return Err(anyhow::anyhow!("unexpected record kind"));
        };
        assert_eq!(loaded.access_token(), "access-token");
        assert_eq!(loaded.refresh_token(), Some("refresh-token"));
        assert_eq!(loaded.access_expires_at_unix(), 1_800_000_000);
        assert_eq!(
            loaded.entitlement_refresh_not_before_unix(),
            Some(1_799_000_000)
        );
        let values = backend.0.lock().map_err(|_| anyhow::anyhow!("test lock"))?;
        let id = values
            .keys()
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing id"))?;
        assert!(!id.contains("access") && !id.contains("refresh"));
        Ok(())
    }

    #[test]
    fn corrupt_and_oversized_records_fail_closed() -> anyhow::Result<()> {
        let backend = MemoryBackend::default();
        let record_ids = CredentialRecordIds::new(
            "5d98d375-4ac4-4507-be4b-c435e373f042",
            CredentialVaultNamespace::Production,
        )?;
        let kind = CredentialRecordKind::InstallationSigningKey;
        let record_id = record_ids.get(kind);
        let first = backend.load_or_store(record_id, &[1; 32])?;
        let second = backend.load_or_store(record_id, &[2; 32])?;
        assert_eq!(first.as_slice(), second.as_slice());
        backend.delete(record_id)?;
        backend
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("test lock"))?
            .insert(record_id.to_owned(), vec![7; 31]);
        assert_eq!(
            load_record(&backend, record_id, kind).unwrap_err(),
            CredentialVaultError::Corrupt
        );
        assert_eq!(
            SecretBytes::new(vec![0; super::super::MAX_STORED_SECRET_BYTES + 1]).unwrap_err(),
            CredentialVaultError::SecretTooLarge {
                max: super::super::MAX_STORED_SECRET_BYTES,
                actual: super::super::MAX_STORED_SECRET_BYTES + 1,
            }
        );
        Ok(())
    }

    #[test]
    fn entitlement_is_bounded_before_storage() -> anyhow::Result<()> {
        let mut entitlement = fixture_entitlement();
        BoundedSignedEntitlement::new(entitlement.clone())?;
        entitlement.grant.key_id = "x".repeat(MAX_ID_BYTES + 1);
        assert_eq!(
            BoundedSignedEntitlement::new(entitlement).unwrap_err(),
            CredentialVaultError::Corrupt
        );
        Ok(())
    }

    fn fixture_entitlement() -> SignedEntitlement {
        let public_key =
            ed25519_dalek::SigningKey::from_bytes(&[23; INSTALLATION_PUBLIC_KEY_BYTES])
                .verifying_key()
                .to_bytes();
        SignedEntitlement {
            grant: EntitlementGrant {
                schema_version: ENTITLEMENT_SCHEMA_VERSION,
                issuer: "https://pro.ctx.rs".to_owned(),
                key_id: "key-1".to_owned(),
                grant_id: "grant-1".to_owned(),
                subject: "subject-1".to_owned(),
                account_id: "account-1".to_owned(),
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
        }
    }

    #[test]
    fn default_alias_must_not_be_the_session_collection() {
        let persistent = "/org/freedesktop/secrets/collection/login";
        let volatile = "/org/freedesktop/secrets/collection/session/key";
        assert!(!belongs_to_collection(volatile, persistent));
        assert!(belongs_to_collection(
            "/org/freedesktop/secrets/collection/login/key",
            persistent
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn library_unavailable_fails_closed_but_missing_persistent_collection_allows_initial_fallback()
    {
        assert_eq!(
            map_secret_service_error(SecretServiceError::Unavailable),
            CredentialVaultError::Backend
        );
        assert_eq!(
            map_secret_service_error(SecretServiceError::NoResult),
            CredentialVaultError::Backend
        );
        assert_eq!(
            map_persistent_collection_error(SecretServiceError::NoResult),
            unavailable()
        );
        assert_eq!(
            map_secret_service_error(SecretServiceError::Locked),
            CredentialVaultError::Locked
        );
        assert_eq!(
            map_secret_service_error(SecretServiceError::Prompt),
            CredentialVaultError::Locked
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_persistent_collection_object_allows_initial_fallback_but_denial_does_not() {
        for error in [
            zbus::fdo::Error::UnknownMethod("collection does not exist".to_owned()),
            zbus::fdo::Error::UnknownObject("collection does not exist".to_owned()),
        ] {
            assert_eq!(
                map_persistent_collection_object_error(SecretServiceError::ZbusFdo(error)),
                unavailable()
            );
        }
        assert_eq!(
            map_persistent_collection_object_error(SecretServiceError::ZbusFdo(
                zbus::fdo::Error::AccessDenied("collection access denied".to_owned())
            )),
            CredentialVaultError::Backend
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_live_bus_without_owned_or_activatable_provider_is_unavailable() {
        assert!(classify_secret_service_provider(true, false).is_ok());
        assert!(classify_secret_service_provider(false, true).is_ok());
        assert_eq!(
            classify_secret_service_provider(false, false),
            Err(unavailable())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_deterministic_missing_session_bus_connection_errors_are_unavailable() {
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::ConnectionRefused,
        ] {
            assert_eq!(
                map_session_bus_connection_error(
                    std::io::Error::new(kind, "deterministically absent session bus").into()
                ),
                unavailable()
            );
        }
        assert_eq!(
            map_session_bus_connection_error(
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "session bus access denied"
                )
                .into()
            ),
            CredentialVaultError::Backend
        );
        assert_eq!(
            map_session_bus_connection_error(zbus::Error::Address(
                "malformed explicit session bus address".to_owned()
            )),
            CredentialVaultError::Backend
        );
    }

    #[test]
    fn lock_requires_private_absolute_owned_runtime_directory() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
        assert!(lock_path_in_runtime_dir(root.path())?.ends_with(LOCK_FILE));
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o750))?;
        assert_eq!(
            lock_path_in_runtime_dir(root.path()).unwrap_err(),
            unavailable()
        );

        let target = tempfile::tempdir()?;
        let links = tempfile::tempdir()?;
        let link = links.path().join("runtime-link");
        symlink(target.path(), &link)?;
        assert_eq!(lock_path_in_runtime_dir(&link).unwrap_err(), unavailable());
        Ok(())
    }

    #[test]
    fn lock_file_rejects_symlinks_and_permissive_existing_files() -> anyhow::Result<()> {
        let runtime = tempfile::tempdir()?;
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))?;
        let lock_path = runtime.path().join(LOCK_FILE);
        let target = runtime.path().join("target");
        std::fs::write(&target, b"")?;
        symlink(&target, &lock_path)?;
        assert_eq!(open_lock_file(&lock_path).unwrap_err(), unavailable());
        std::fs::remove_file(&lock_path)?;
        std::fs::write(&lock_path, b"")?;
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o644))?;
        assert_eq!(open_lock_file(&lock_path).unwrap_err(), unavailable());
        Ok(())
    }

    #[test]
    fn live_persistent_secret_service_round_trip_when_enabled() -> anyhow::Result<()> {
        let Ok(mode) = std::env::var("CTX_TEST_LIVE_COMMERCIAL_SECRET_SERVICE") else {
            return Ok(());
        };
        let backend = PlatformBackend::production();
        let record_ids = CredentialRecordIds::new(
            "3aac3692-4fee-4d18-9ca4-4549a0b70a13",
            CredentialVaultNamespace::Production,
        )?;
        let record_id = record_ids.get(CredentialRecordKind::InstallationSigningKey);
        if mode == "load" {
            let loaded = backend.load(record_id)?;
            assert_eq!(loaded.as_slice(), [0x5a; INSTALLATION_PUBLIC_KEY_BYTES]);
            backend.delete(record_id)?;
            return Ok(());
        }
        if mode != "1" && mode != "store" {
            return Err(anyhow::anyhow!("invalid live-test mode"));
        }
        match backend.load(record_id) {
            Err(CredentialVaultError::NotFound) => {}
            other => return Err(anyhow::anyhow!("unexpected initial vault state: {other:?}")),
        }
        let loaded = backend.load_or_store(record_id, &[0x5a; INSTALLATION_PUBLIC_KEY_BYTES])?;
        if loaded.as_slice() != [0x5a; INSTALLATION_PUBLIC_KEY_BYTES] {
            return Err(CredentialVaultError::Corrupt.into());
        }
        if mode == "store" {
            Ok(())
        } else {
            backend.delete(record_id).map_err(Into::into)
        }
    }
}

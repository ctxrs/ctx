use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use secret_service::blocking::{Collection, Item, SecretService};
use secret_service::{EncryptionType, Error as SecretServiceError};

use crate::pro::credential_vault::CredentialVaultError;

const SERVICE: &str = "com.ctx.pro.work-graph-key.v1";
const KIND: &str = "ctx-pro-graph-key-v1";
const LOCK_FILE: &str = ".ctx-pro-graph-key.lock";
const PLATFORM: &str = std::env::consts::OS;

pub(super) fn delete(account: &str) -> Result<(), CredentialVaultError> {
    with_lock(|| {
        with_unique_item(account, |item| {
            item.delete().map_err(map_secret_service_error)
        })
    })
}

fn connect() -> Result<SecretService<'static>, CredentialVaultError> {
    SecretService::connect(EncryptionType::Dh).map_err(map_secret_service_error)
}

fn with_unique_item<T>(
    account: &str,
    operation: impl FnOnce(&Item<'_>) -> Result<T, CredentialVaultError>,
) -> Result<T, CredentialVaultError> {
    let service = connect()?;
    let matches = find_items(&service, account)?;
    match matches.as_slice() {
        [] => Err(CredentialVaultError::NotFound),
        [item] => operation(item),
        _ => Err(CredentialVaultError::Ambiguous),
    }
}

fn find_items<'a>(
    service: &'a SecretService<'a>,
    account: &str,
) -> Result<Vec<Item<'a>>, CredentialVaultError> {
    let collection = persistent_default_collection(service)?;
    let collection_path = collection.collection_path.as_str();
    let matches = service
        .search_items(attributes(account))
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
            return Err(unavailable());
        }
        Ok(_) | Err(SecretServiceError::NoResult) => {}
        Err(error) => return Err(map_secret_service_error(error)),
    }
    if collection.is_locked().map_err(map_secret_service_error)? {
        return Err(CredentialVaultError::Locked);
    }
    Ok(collection)
}

fn attributes(account: &str) -> HashMap<&str, &str> {
    HashMap::from([
        ("service", SERVICE),
        ("username", account),
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
        SecretServiceError::Unavailable => unavailable(),
        SecretServiceError::Locked | SecretServiceError::Prompt => CredentialVaultError::Locked,
        _ => CredentialVaultError::Backend,
    }
}

fn map_persistent_collection_error(error: SecretServiceError) -> CredentialVaultError {
    if matches!(error, SecretServiceError::NoResult) {
        unavailable()
    } else {
        map_secret_service_error(error)
    }
}

const fn unavailable() -> CredentialVaultError {
    CredentialVaultError::Unavailable { platform: PLATFORM }
}

fn with_lock<T>(
    operation: impl FnOnce() -> Result<T, CredentialVaultError>,
) -> Result<T, CredentialVaultError> {
    let path = lock_path()?;
    let file = open_lock_file(&path)?;
    fs2::FileExt::lock_exclusive(&file).map_err(|_| CredentialVaultError::Backend)?;
    let result = operation();
    let unlock = fs2::FileExt::unlock(&file).map_err(|_| CredentialVaultError::Backend);
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn lock_path() -> Result<PathBuf, CredentialVaultError> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(unavailable)?;
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
        .map_err(|_| CredentialVaultError::Backend)?;
    let opened = file.metadata().map_err(|_| CredentialVaultError::Backend)?;
    let named = path
        .symlink_metadata()
        .map_err(|_| CredentialVaultError::Backend)?;
    if !opened.file_type().is_file()
        || named.file_type().is_symlink()
        || opened.dev() != named.dev()
        || opened.ino() != named.ino()
        || opened.permissions().mode() & 0o077 != 0
        || opened.uid() != effective_uid()
        || opened.nlink() != 1
    {
        return Err(CredentialVaultError::Backend);
    }
    Ok(file)
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no arguments and no failure mode on supported Unix.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt as _};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn graph_key_lock_rejects_symlinks_and_permissive_files() {
        let root = tempdir().unwrap();
        let target = root.path().join("target");
        let link = root.path().join("link");
        std::fs::write(&target, "lock").unwrap();
        symlink(&target, &link).unwrap();
        assert!(open_lock_file(&link).is_err());

        let permissive = root.path().join("permissive");
        std::fs::write(&permissive, "").unwrap();
        std::fs::set_permissions(&permissive, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(open_lock_file(&permissive).is_err());
    }

    #[test]
    fn live_delete_removes_corrupt_key_bytes_and_is_idempotent_when_enabled() {
        if std::env::var("CTX_TEST_LIVE_PRO_GRAPH_KEY_DELETION").as_deref() != Ok("1") {
            return;
        }
        let graph_id = format!("ctx-pro-delete-live-test-{}", uuid::Uuid::new_v4());
        let account = super::super::native_graph_record_id(&graph_id);
        let service = connect().unwrap();
        let collection = persistent_default_collection(&service).unwrap();
        collection
            .create_item(
                "ctx Pro delete-only live test",
                attributes(&account),
                b"corrupt-not-a-graph-key",
                true,
                "application/octet-stream",
            )
            .unwrap();
        collection
            .create_item(
                "ctx Pro stale raw-ID negative fixture",
                attributes(&graph_id),
                b"stale-raw-record",
                true,
                "application/octet-stream",
            )
            .unwrap();
        assert_eq!(find_items(&service, &account).unwrap().len(), 1);
        assert_eq!(find_items(&service, &graph_id).unwrap().len(), 1);
        let data_root = tempdir().unwrap();
        super::super::delete(data_root.path(), &graph_id).unwrap();
        assert!(find_items(&service, &account).unwrap().is_empty());
        assert_eq!(find_items(&service, &graph_id).unwrap().len(), 1);
        assert!(matches!(
            super::super::delete(data_root.path(), &graph_id),
            Err(CredentialVaultError::NotFound)
        ));
        with_unique_item(&graph_id, |item| {
            item.delete().map_err(map_secret_service_error)
        })
        .unwrap();
    }
}

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
};

use fs2::FileExt as _;
use security_framework::base::Error as SecurityFrameworkError;
use security_framework::passwords::delete_generic_password;
use security_framework_sys::base::{
    errSecAuthFailed as ERR_SEC_AUTH_FAILED, errSecItemNotFound as ERR_SEC_ITEM_NOT_FOUND,
};

use crate::pro::credential_vault::CredentialVaultError;

const SERVICE: &str = "com.ctx.pro.work-graph-key.v1";
const LOCK_FILE: &str = ".ctx-pro-graph-key-v1.lock";
const ERR_SEC_NOT_AVAILABLE: i32 = -25_291;
const ERR_SEC_NO_ACCESS_FOR_ITEM: i32 = -25_243;
const ERR_SEC_NO_SUCH_KEYCHAIN: i32 = -25_294;
const ERR_SEC_NO_DEFAULT_KEYCHAIN: i32 = -25_307;
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25_308;
const ERR_SEC_INTERACTION_REQUIRED: i32 = -25_315;
const ERR_SEC_MISSING_ENTITLEMENT: i32 = -34_018;
const ERR_SEC_USER_CANCELED: i32 = -128;

pub(super) fn delete(account: &str) -> Result<(), CredentialVaultError> {
    let file = open_lock_file()?;
    file.lock_exclusive()
        .map_err(|_| CredentialVaultError::Backend)?;
    let result = delete_generic_password(SERVICE, account).map_err(map_keychain_error);
    let unlock = file.unlock().map_err(|_| CredentialVaultError::Backend);
    match (result, unlock) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) | (Ok(()), Err(error)) => Err(error),
    }
}

fn map_keychain_error(error: SecurityFrameworkError) -> CredentialVaultError {
    match error.code() {
        ERR_SEC_ITEM_NOT_FOUND => CredentialVaultError::NotFound,
        ERR_SEC_INTERACTION_NOT_ALLOWED
        | ERR_SEC_INTERACTION_REQUIRED
        | ERR_SEC_AUTH_FAILED
        | ERR_SEC_NO_ACCESS_FOR_ITEM
        | ERR_SEC_MISSING_ENTITLEMENT
        | ERR_SEC_USER_CANCELED => CredentialVaultError::Locked,
        ERR_SEC_NOT_AVAILABLE | ERR_SEC_NO_SUCH_KEYCHAIN | ERR_SEC_NO_DEFAULT_KEYCHAIN => {
            CredentialVaultError::Unavailable { platform: "macos" }
        }
        _ => CredentialVaultError::Backend,
    }
}

fn lock_path() -> Result<PathBuf, CredentialVaultError> {
    let directory =
        fs::canonicalize(std::env::temp_dir()).map_err(|_| CredentialVaultError::Backend)?;
    let metadata = directory
        .symlink_metadata()
        .map_err(|_| CredentialVaultError::Backend)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.mode() & 0o077 != 0 {
        return Err(CredentialVaultError::Backend);
    }
    Ok(directory.join(LOCK_FILE))
}

fn open_lock_file() -> Result<File, CredentialVaultError> {
    open_lock_file_at(&lock_path()?)
}

fn open_lock_file_at(path: &Path) -> Result<File, CredentialVaultError> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            validate_lock_path(path)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .truncate(false)
                .open(path)
                .map_err(|_| CredentialVaultError::Backend)?
        }
        Err(_) => return Err(CredentialVaultError::Backend),
    };
    validate_lock_path(path)?;
    let opened = file.metadata().map_err(|_| CredentialVaultError::Backend)?;
    let named = path
        .symlink_metadata()
        .map_err(|_| CredentialVaultError::Backend)?;
    if opened.dev() != named.dev() || opened.ino() != named.ino() {
        return Err(CredentialVaultError::Backend);
    }
    Ok(file)
}

fn validate_lock_path(path: &Path) -> Result<(), CredentialVaultError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|_| CredentialVaultError::Backend)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.mode() & 0o177 != 0
    {
        return Err(CredentialVaultError::Backend);
    }
    let parent = path.parent().ok_or(CredentialVaultError::Backend)?;
    let parent_metadata = parent
        .symlink_metadata()
        .map_err(|_| CredentialVaultError::Backend)?;
    if metadata.uid() != parent_metadata.uid() {
        return Err(CredentialVaultError::Backend);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt as _};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn graph_key_lock_rejects_path_attacks() {
        let root = tempdir().unwrap();
        let target = root.path().join("target");
        let link = root.path().join("link");
        fs::write(&target, "lock").unwrap();
        symlink(&target, &link).unwrap();
        assert!(open_lock_file_at(&link).is_err());

        let permissive = root.path().join("permissive");
        fs::write(&permissive, "").unwrap();
        fs::set_permissions(&permissive, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(open_lock_file_at(&permissive).is_err());
    }
}

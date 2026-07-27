#![allow(unsafe_code)]

use std::ptr::null;

use sha2::{Digest as _, Sha256};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_NOT_FOUND, ERROR_NO_SUCH_LOGON_SESSION,
    HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

use crate::pro::credential_vault::CredentialVaultError;

const TARGET_PREFIX: &str = "ctx-pro/work-graph-key/v1/";
const MUTEX_PREFIX: &str = "Local\\ctx-pro-work-graph-key-v1-";
const LOCK_WAIT_MILLIS: u32 = 30_000;

pub(super) fn delete(graph_id: &str) -> Result<(), CredentialVaultError> {
    let target = credential_target(graph_id);
    with_lock(&target, || {
        let target = wide_string(&target);
        // SAFETY: target is a valid NUL-terminated UTF-16 string for the call.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            return Err(map_error(last_error()));
        }
        Ok(())
    })
}

fn with_lock<T>(
    target: &str,
    operation: impl FnOnce() -> Result<T, CredentialVaultError>,
) -> Result<T, CredentialVaultError> {
    let name = wide_string(&mutex_name(target));
    // SAFETY: default security attributes and a valid NUL-terminated name are used.
    let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(map_error(last_error()));
    }
    let mut mutex = MutexHandle::new(handle);
    // SAFETY: handle is a live mutex handle owned by the guard.
    let wait = unsafe { WaitForSingleObject(handle, LOCK_WAIT_MILLIS) };
    if !matches!(wait, WAIT_OBJECT_0 | WAIT_ABANDONED) {
        return Err(CredentialVaultError::Backend);
    }
    mutex.owned = true;
    let result = operation();
    let release = mutex.release();
    match (result, release) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

struct MutexHandle {
    handle: HANDLE,
    owned: bool,
}

impl MutexHandle {
    const fn new(handle: HANDLE) -> Self {
        Self {
            handle,
            owned: false,
        }
    }

    fn release(&mut self) -> Result<(), CredentialVaultError> {
        if self.owned {
            // SAFETY: this guard owns the mutex after a successful wait.
            if unsafe { ReleaseMutex(self.handle) } == 0 {
                return Err(CredentialVaultError::Backend);
            }
            self.owned = false;
        }
        Ok(())
    }
}

impl Drop for MutexHandle {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: best-effort release of the mutex owned by this guard.
            unsafe { ReleaseMutex(self.handle) };
        }
        // SAFETY: this guard owns this handle and closes it exactly once.
        unsafe { CloseHandle(self.handle) };
    }
}

fn credential_target(graph_id: &str) -> String {
    format!("{TARGET_PREFIX}{}", identity_digest(graph_id))
}

fn mutex_name(target: &str) -> String {
    format!("{MUTEX_PREFIX}{}", identity_digest(target))
}

fn identity_digest(value: &str) -> String {
    let bytes = Sha256::digest(value.as_bytes());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: GetLastError has no preconditions and immediately follows failure.
    unsafe { GetLastError() }
}

const fn map_error(code: u32) -> CredentialVaultError {
    match code {
        ERROR_NOT_FOUND => CredentialVaultError::NotFound,
        ERROR_ACCESS_DENIED => CredentialVaultError::Locked,
        ERROR_NO_SUCH_LOGON_SESSION => CredentialVaultError::Unavailable {
            platform: "windows",
        },
        _ => CredentialVaultError::Backend,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_target_and_lock_match_the_private_locator_contract() {
        let graph_id = "ctx-pro-installation-graph-v1:2f746d70:thumbprint";
        let target = credential_target(graph_id);
        assert!(target.starts_with(TARGET_PREFIX));
        assert_eq!(target.len(), TARGET_PREFIX.len() + 64);
        let mutex = mutex_name(&target);
        assert!(mutex.starts_with(MUTEX_PREFIX));
        assert_eq!(mutex.len(), MUTEX_PREFIX.len() + 64);
    }
}

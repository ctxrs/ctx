//! Delete-only access to the private helper's native graph-key record.
//!
//! This module deliberately knows only the stable key-store locator. It cannot
//! read key material, open the encrypted graph, or run private detectors.

use std::path::Path;

use anyhow::Result;
use ctx_pro_host_protocol::ProFilesystemLayout;
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos",
    target_os = "windows"
))]
use sha2::{Digest as _, Sha256};

use super::credential_vault::{CredentialVaultError, CredentialVaultNamespace};

#[cfg(target_os = "macos")]
#[path = "graph_key_deletion/macos.rs"]
mod platform;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[path = "graph_key_deletion/secret_service.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "graph_key_deletion/windows.rs"]
mod platform;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "graph_key_deletion/unix_file.rs"]
mod unix_file;
#[cfg(target_os = "windows")]
#[path = "graph_key_deletion/windows_file.rs"]
mod windows_file;

const MAX_GRAPH_ID_BYTES: usize = 16 * 1024;
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos",
    target_os = "windows"
))]
const NATIVE_RECORD_ID_DOMAIN: &[u8] = b"ctx\0local-pro\0native-vault-record-id\0v1\0";
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos",
    target_os = "windows"
))]
const GRAPH_RECORD_DOMAIN: &[u8] = b"graph-key";
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos",
    target_os = "windows"
))]
const GRAPH_RECORD_PREFIX: &str = "nvr1-g-";
#[cfg(target_os = "linux")]
const NATIVE_SELECTION: &[u8; 12] = b"CTXKSB01SECR";
#[cfg(target_os = "macos")]
const NATIVE_SELECTION: &[u8; 12] = b"CTXKSB01KEYC";
#[cfg(target_os = "windows")]
const NATIVE_SELECTION: &[u8; 12] = b"CTXKSB01WCRM";

pub(super) fn delete_selected(
    data_root: &Path,
    namespace: CredentialVaultNamespace,
    installation_key_thumbprint: &str,
) -> Result<()> {
    super::client::delete_graph_key(data_root, namespace, installation_key_thumbprint)
}

pub(super) fn delete(data_root: &Path, graph_id: &str) -> Result<(), CredentialVaultError> {
    validate_graph_id(graph_id)?;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let account = native_graph_record_id(graph_id);
        unix_file::delete(
            &ProFilesystemLayout::new(data_root).pro_root(),
            &account,
            NATIVE_SELECTION,
            || platform::delete(&account),
        )
    }
    #[cfg(target_os = "freebsd")]
    {
        delete_opaque_native_record(graph_id, platform::delete)
    }
    #[cfg(target_os = "windows")]
    {
        let account = native_graph_record_id(graph_id);
        windows_file::delete(
            &ProFilesystemLayout::new(data_root).pro_root(),
            &account,
            NATIVE_SELECTION,
            || platform::delete(graph_id),
        )
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "macos",
        target_os = "windows"
    )))]
    {
        let _ = data_root;
        let _ = graph_id;
        Err(CredentialVaultError::Unavailable {
            platform: std::env::consts::OS,
        })
    }
}

fn validate_graph_id(graph_id: &str) -> Result<(), CredentialVaultError> {
    if graph_id.is_empty()
        || graph_id.len() > MAX_GRAPH_ID_BYTES
        || graph_id.as_bytes().contains(&0)
    {
        return Err(CredentialVaultError::Backend);
    }
    Ok(())
}

#[cfg(target_os = "freebsd")]
fn delete_opaque_native_record(
    graph_id: &str,
    delete_record: impl FnOnce(&str) -> Result<(), CredentialVaultError>,
) -> Result<(), CredentialVaultError> {
    let account = native_graph_record_id(graph_id);
    delete_record(&account)
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos",
    target_os = "windows"
))]
fn native_graph_record_id(graph_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(NATIVE_RECORD_ID_DOMAIN);
    hash.update(GRAPH_RECORD_DOMAIN);
    hash.update((graph_id.len() as u64).to_be_bytes());
    hash.update(graph_id.as_bytes());
    format!("{GRAPH_RECORD_PREFIX}{:x}", hash.finalize())
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "freebsd")]
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn rejects_unbounded_or_ambiguous_internal_ids() {
        let root = Path::new("/unused");
        assert!(matches!(
            delete(root, ""),
            Err(CredentialVaultError::Backend)
        ));
        assert!(matches!(
            delete(root, &"x".repeat(MAX_GRAPH_ID_BYTES + 1)),
            Err(CredentialVaultError::Backend)
        ));
        assert!(matches!(
            delete(root, "graph\0suffix"),
            Err(CredentialVaultError::Backend)
        ));
    }

    #[cfg(target_os = "freebsd")]
    #[test]
    fn opaque_lookup_matches_private_golden_and_never_falls_back_to_raw_id() {
        const LOGICAL: &str = "ctx-pro-installation-graph-v1:/Users/Alice/secret/repo";
        const GOLDEN: &str =
            "nvr1-g-12c2fbc8efe95366e7da4511ebe8b5c7e17a38321f4d92831d3a520ee5c7dc07";
        let calls = RefCell::new(Vec::new());
        let result = delete_opaque_native_record(LOGICAL, |account| {
            calls.borrow_mut().push(account.to_owned());
            Err(CredentialVaultError::NotFound)
        });
        assert!(matches!(result, Err(CredentialVaultError::NotFound)));
        assert_eq!(calls.into_inner(), [GOLDEN]);
        assert_eq!(native_graph_record_id(LOGICAL), GOLDEN);
        assert!(!GOLDEN.contains("Alice"));
        assert!(!GOLDEN.contains("secret"));
        assert_ne!(GOLDEN, LOGICAL);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn opaque_file_record_lookup_matches_the_private_golden() {
        const LOGICAL: &str = "ctx-pro-installation-graph-v1:/Users/Alice/secret/repo";
        const GOLDEN: &str =
            "nvr1-g-12c2fbc8efe95366e7da4511ebe8b5c7e17a38321f4d92831d3a520ee5c7dc07";
        assert_eq!(native_graph_record_id(LOGICAL), GOLDEN);
        assert!(!GOLDEN.contains("Alice"));
        assert!(!GOLDEN.contains("secret"));
    }
}

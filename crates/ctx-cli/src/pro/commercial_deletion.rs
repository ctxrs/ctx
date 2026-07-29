use std::path::Path;

use anyhow::{bail, Result};

use super::{
    credential_vault::{
        CredentialRecordKind, CredentialVaultError, CredentialVaultNamespace,
        PlatformCredentialVault,
    },
    graph_key_deletion,
    local_deletion::vault_error,
};

const ANONYMOUS_TRIAL_ANCHOR_ID: &str = "device-evidence-anchor/3d5657e0/v1";

pub(super) fn delete_credentials(data_root: &Path) -> Result<()> {
    delete_anonymous_trial_anchor(data_root)?;
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
                        "key_store_unavailable: local Pro credential deletion could not be verified"
                    )
                }
                Err(error) => return Err(vault_error(error)),
            }
        }
    }
    PlatformCredentialVault::cleanup_backend_state(data_root).map_err(vault_error)?;
    Ok(())
}

fn delete_anonymous_trial_anchor(data_root: &Path) -> Result<()> {
    match graph_key_deletion::delete(data_root, ANONYMOUS_TRIAL_ANCHOR_ID) {
        Ok(()) | Err(CredentialVaultError::NotFound) => {}
        Err(error) => return Err(vault_error(error)),
    }
    match graph_key_deletion::delete(data_root, ANONYMOUS_TRIAL_ANCHOR_ID) {
        Err(CredentialVaultError::NotFound) => Ok(()),
        Ok(()) | Err(CredentialVaultError::Corrupt) => {
            bail!("key_store_unavailable: Pro activation-anchor deletion could not be verified")
        }
        Err(error) => Err(vault_error(error)),
    }
}

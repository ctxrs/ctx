use std::path::Path;

use anyhow::{anyhow, bail, Result};
use ctx_pro_host_protocol::{
    base64url, installation_key_thumbprint, installation_proof_bytes, AuthorizationRequest,
    SignedEntitlement, AUTHORIZATION_CHALLENGE_BYTES, ED25519_SIGNATURE_BYTES,
    INSTALLATION_PUBLIC_KEY_BYTES,
};

use super::commercial_config::CommercialConfig;
use super::commercial_lifecycle::{vault_error, CommercialLifecycleService};
use super::credential_vault::{
    CredentialRecord, CredentialRecordKind, CredentialVaultError, CredentialVaultNamespace,
    PlatformCredentialVault, VaultInstallationChallengeSigner,
};

/// Supplies a challenge-bound authorization request to the Pro helper.
///
/// Implementations are expected to obtain the signed grant and installation
/// key through the platform key store. The OSS client deliberately has
/// no environment-variable or plaintext-file fallback for installation keys.
pub(crate) trait AuthorizationProvider {
    fn authorization_for_challenge(
        &self,
        challenge: &[u8; AUTHORIZATION_CHALLENGE_BYTES],
    ) -> Result<AuthorizationRequest>;
}

/// Signs one helper challenge with an installation-bound platform key.
pub(crate) trait InstallationChallengeSigner {
    fn public_key(&self) -> Result<[u8; INSTALLATION_PUBLIC_KEY_BYTES]>;

    fn sign_installation_proof(&self, proof: &[u8]) -> Result<[u8; ED25519_SIGNATURE_BYTES]>;
}

/// Composes a stored issuer-signed grant with a platform-vault key signer.
pub(crate) struct SignedGrantAuthorizationProvider<'a, S> {
    entitlement: &'a SignedEntitlement,
    signer: &'a S,
}

impl<'a, S> SignedGrantAuthorizationProvider<'a, S> {
    pub(crate) const fn new(entitlement: &'a SignedEntitlement, signer: &'a S) -> Self {
        Self {
            entitlement,
            signer,
        }
    }
}

impl<S: InstallationChallengeSigner> AuthorizationProvider
    for SignedGrantAuthorizationProvider<'_, S>
{
    fn authorization_for_challenge(
        &self,
        challenge: &[u8; AUTHORIZATION_CHALLENGE_BYTES],
    ) -> Result<AuthorizationRequest> {
        let public_key = self.signer.public_key()?;
        let thumbprint = installation_key_thumbprint(&public_key);
        if thumbprint != self.entitlement.grant.installation_key_thumbprint {
            bail!("entitlement_invalid: signed grant is bound to a different installation key");
        }
        let proof = installation_proof_bytes(&self.entitlement.grant, challenge);
        let signature = self
            .signer
            .sign_installation_proof(&proof)
            .map_err(|_| anyhow!("entitlement_invalid: installation proof signing failed"))?;
        Ok(AuthorizationRequest {
            entitlement: self.entitlement.clone(),
            installation_public_key_base64url: base64url(&public_key),
            challenge_base64url: base64url(challenge),
            proof_signature_base64url: base64url(&signature),
        })
    }
}

/// Loads the installation-bound grant and signer only from the native key store.
pub(crate) struct StoredAuthorizationProvider {
    vault: PlatformCredentialVault,
    entitlement: SignedEntitlement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntitlementSchedule {
    pub(crate) refresh_after_unix: i64,
    pub(crate) access_deadline_unix: i64,
    pub(crate) grace_deadline_unix: i64,
}

impl StoredAuthorizationProvider {
    pub(crate) fn load(data_root: &Path) -> Result<Self> {
        CommercialLifecycleService::refresh_entitlement_if_due(data_root)?;
        Self::load_without_refresh(data_root)
    }

    pub(crate) fn load_for_status(data_root: &Path) -> Result<Self> {
        Self::load_without_refresh(data_root)
    }

    pub(crate) fn load_for_graph_key_deletion(
        data_root: &Path,
        namespace: CredentialVaultNamespace,
        expected_thumbprint: &str,
    ) -> Result<Self> {
        let vault =
            PlatformCredentialVault::production(data_root, namespace).map_err(vault_error)?;
        let installation_key = match vault.load(CredentialRecordKind::InstallationSigningKey) {
            Ok(CredentialRecord::InstallationSigningKey(seed)) => seed,
            Ok(_) => bail!("key_store_unavailable: installation key record mismatch"),
            Err(CredentialVaultError::NotFound) => {
                bail!("key_store_unavailable: graph-key deletion installation key is missing")
            }
            Err(error) => return Err(vault_error(error)),
        };
        let entitlement = match vault.load(CredentialRecordKind::SignedEntitlement) {
            Ok(CredentialRecord::SignedEntitlement(entitlement)) => entitlement.as_inner().clone(),
            Ok(_) => bail!("key_store_unavailable: entitlement record mismatch"),
            Err(CredentialVaultError::NotFound) => {
                bail!("key_store_unavailable: graph-key deletion entitlement is missing")
            }
            Err(error) => return Err(vault_error(error)),
        };
        let public_key = ed25519_dalek::SigningKey::from_bytes(installation_key.expose())
            .verifying_key()
            .to_bytes();
        let installation_thumbprint = installation_key_thumbprint(&public_key);
        if installation_thumbprint != expected_thumbprint
            || entitlement.grant.installation_key_thumbprint != expected_thumbprint
        {
            bail!(
                "entitlement_invalid: graph-key deletion records do not match the requested installation key"
            );
        }
        Ok(Self { vault, entitlement })
    }

    fn load_without_refresh(data_root: &Path) -> Result<Self> {
        let config = CommercialConfig::production()?;
        let vault = PlatformCredentialVault::production(data_root, config.vault_namespace)
            .map_err(vault_error)?;
        let record = match vault.load(CredentialRecordKind::SignedEntitlement) {
            Ok(record) => record,
            Err(super::credential_vault::CredentialVaultError::NotFound) => {
                bail!("entitlement_required: run `ctx pro`")
            }
            Err(error) => return Err(vault_error(error)),
        };
        let CredentialRecord::SignedEntitlement(entitlement) = record else {
            bail!("key_store_unavailable: entitlement record mismatch");
        };
        config.entitlement_trust.validate_identity(
            &entitlement.as_inner().grant.issuer,
            &entitlement.as_inner().grant.key_id,
        )?;
        Ok(Self {
            vault,
            entitlement: entitlement.as_inner().clone(),
        })
    }
    pub(crate) fn entitlement_schedule(&self) -> EntitlementSchedule {
        EntitlementSchedule {
            refresh_after_unix: self.entitlement.grant.refresh_after_unix,
            access_deadline_unix: self.entitlement.grant.access_deadline_unix,
            grace_deadline_unix: self.entitlement.grant.grace_deadline_unix,
        }
    }
}

impl AuthorizationProvider for StoredAuthorizationProvider {
    fn authorization_for_challenge(
        &self,
        challenge: &[u8; AUTHORIZATION_CHALLENGE_BYTES],
    ) -> Result<AuthorizationRequest> {
        let signer = VaultInstallationChallengeSigner::new(&self.vault);
        SignedGrantAuthorizationProvider::new(&self.entitlement, &signer)
            .authorization_for_challenge(challenge)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ctx_pro_host_protocol::{
        EntitlementAccessKind, EntitlementCapability, EntitlementGrant, ENTITLEMENT_SCHEMA_VERSION,
    };

    use super::*;

    struct RecordingSigner {
        public_key: [u8; INSTALLATION_PUBLIC_KEY_BYTES],
    }

    impl InstallationChallengeSigner for RecordingSigner {
        fn public_key(&self) -> Result<[u8; INSTALLATION_PUBLIC_KEY_BYTES]> {
            Ok(self.public_key)
        }

        fn sign_installation_proof(&self, proof: &[u8]) -> Result<[u8; ED25519_SIGNATURE_BYTES]> {
            let mut signature = [0_u8; ED25519_SIGNATURE_BYTES];
            signature[..32].copy_from_slice(&proof[..32]);
            signature[32..].copy_from_slice(&proof[..32]);
            Ok(signature)
        }
    }

    fn entitlement(public_key: &[u8; INSTALLATION_PUBLIC_KEY_BYTES]) -> SignedEntitlement {
        SignedEntitlement {
            grant: EntitlementGrant {
                schema_version: ENTITLEMENT_SCHEMA_VERSION,
                issuer: "https://commercial.staging.ctx.rs".to_owned(),
                key_id: "staging-2026-07-v1".to_owned(),
                grant_id: "grant-1".to_owned(),
                subject: "user-1".to_owned(),
                account_id: "account-1".to_owned(),
                product: "ctx-pro".to_owned(),
                access_kind: EntitlementAccessKind::Active,
                issued_at_unix: 1_800_000_000,
                not_before_unix: 1_800_000_000,
                expires_at_unix: 1_800_604_800,
                refresh_after_unix: 1_800_345_600,
                access_deadline_unix: 1_802_592_000,
                grace_deadline_unix: 1_803_196_800,
                installation_key_thumbprint: installation_key_thumbprint(public_key),
                capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
                minimum_helper_protocol: ctx_pro_host_protocol::PROTOCOL_VERSION,
                revocation_epoch: 0,
            },
            signature_base64url: base64url(&[9_u8; ED25519_SIGNATURE_BYTES]),
        }
    }

    #[test]
    fn composes_challenge_bound_request_without_loading_key_material() {
        let public_key = [7_u8; INSTALLATION_PUBLIC_KEY_BYTES];
        let entitlement = entitlement(&public_key);
        let signer = RecordingSigner { public_key };
        let request = SignedGrantAuthorizationProvider::new(&entitlement, &signer)
            .authorization_for_challenge(&[11_u8; AUTHORIZATION_CHALLENGE_BYTES])
            .unwrap();
        assert_eq!(request.entitlement, entitlement);
        assert_eq!(
            request.installation_public_key_base64url,
            base64url(&public_key)
        );
    }

    #[test]
    fn rejects_a_grant_bound_to_another_installation() {
        let entitlement = entitlement(&[7_u8; INSTALLATION_PUBLIC_KEY_BYTES]);
        let signer = RecordingSigner {
            public_key: [8_u8; INSTALLATION_PUBLIC_KEY_BYTES],
        };
        let error = SignedGrantAuthorizationProvider::new(&entitlement, &signer)
            .authorization_for_challenge(&[11_u8; AUTHORIZATION_CHALLENGE_BYTES])
            .unwrap_err();
        assert!(error.to_string().starts_with("entitlement_invalid:"));
    }
}

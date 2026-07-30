use std::fmt;
use std::path::Path;

use anyhow::anyhow;
use ctx_pro_host_protocol::{
    decode_base64url, valid_pro_installation_id, EntitlementAccessKind, SignedEntitlement,
    ED25519_SIGNATURE_BYTES, ENTITLEMENT_CLOCK_SKEW_SECONDS, ENTITLEMENT_GRANT_SECONDS,
    ENTITLEMENT_MAX_GRACE_SECONDS, ENTITLEMENT_SCHEMA_VERSION, INSTALLATION_PUBLIC_KEY_BYTES,
    PROTOCOL_VERSION,
};
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::authorization::InstallationChallengeSigner;

mod anonymous_trial;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod secret_service;
#[cfg(target_os = "macos")]
#[path = "credential_vault/linux.rs"]
mod unix_file;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
mod windows_file;

#[cfg(target_os = "linux")]
use linux::PlatformBackend;
#[cfg(target_os = "macos")]
use macos::PlatformBackend;
#[cfg(target_os = "freebsd")]
use secret_service::PlatformBackend;
#[cfg(target_os = "windows")]
use windows::PlatformBackend;

const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_ID_BYTES: usize = 512;
pub(super) const MAX_STORED_SECRET_BYTES: usize = 2_560;

const RECORD_ID_PREFIX: &str = "cv2-";
const RECORD_ID_HEX_BYTES: usize = 64;
const RECORD_ID_DOMAIN: &[u8] = b"ctx\0credential-vault\0record-id\0v2\0";
const WORKOS_SCHEMA_VERSION: u16 = 1;

pub(crate) use anonymous_trial::AnonymousTrialMaterial;
use anonymous_trial::{decode, encode, store_state};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialVaultNamespace {
    Production,
    Staging,
}

impl CredentialVaultNamespace {
    const fn domain_label(self) -> &'static [u8] {
        match self {
            Self::Production => b"production",
            Self::Staging => b"staging",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialRecordKind {
    WorkOsSession,
    AnonymousTrial,
    InstallationSigningKey,
    SignedEntitlement,
}

impl CredentialRecordKind {
    const fn domain_label(self) -> &'static [u8] {
        match self {
            Self::WorkOsSession => b"workos-session",
            Self::AnonymousTrial => b"anonymous-trial",
            Self::InstallationSigningKey => b"installation-signing-key",
            Self::SignedEntitlement => b"signed-entitlement",
        }
    }
}

#[derive(Debug)]
struct CredentialRecordIds {
    workos_session: String,
    anonymous_trial: String,
    installation_signing_key: String,
    signed_entitlement: String,
}

impl CredentialRecordIds {
    fn new(
        installation_id: &str,
        namespace: CredentialVaultNamespace,
    ) -> Result<Self, CredentialVaultError> {
        validate_installation_id(installation_id)?;
        Ok(Self {
            workos_session: derive_record_id(
                installation_id,
                namespace,
                CredentialRecordKind::WorkOsSession,
            ),
            anonymous_trial: derive_record_id(
                installation_id,
                namespace,
                CredentialRecordKind::AnonymousTrial,
            ),
            installation_signing_key: derive_record_id(
                installation_id,
                namespace,
                CredentialRecordKind::InstallationSigningKey,
            ),
            signed_entitlement: derive_record_id(
                installation_id,
                namespace,
                CredentialRecordKind::SignedEntitlement,
            ),
        })
    }

    fn get(&self, kind: CredentialRecordKind) -> &str {
        match kind {
            CredentialRecordKind::WorkOsSession => &self.workos_session,
            CredentialRecordKind::AnonymousTrial => &self.anonymous_trial,
            CredentialRecordKind::InstallationSigningKey => &self.installation_signing_key,
            CredentialRecordKind::SignedEntitlement => &self.signed_entitlement,
        }
    }
}

fn validate_installation_id(installation_id: &str) -> Result<(), CredentialVaultError> {
    if !valid_pro_installation_id(installation_id) {
        return Err(CredentialVaultError::InvalidDataRoot);
    }
    Ok(())
}

fn derive_record_id(
    installation_id: &str,
    namespace: CredentialVaultNamespace,
    kind: CredentialRecordKind,
) -> String {
    let mut digest = Sha256::new();
    digest.update(RECORD_ID_DOMAIN);
    digest.update(namespace.domain_label());
    digest.update(b"\0");
    digest.update(installation_id.as_bytes());
    digest.update(b"\0");
    digest.update(kind.domain_label());
    format!("{RECORD_ID_PREFIX}{:x}", digest.finalize())
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkOsSessionMaterial {
    schema_version: u16,
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    access_expires_at_unix: i64,
    #[serde(default)]
    entitlement_refresh_not_before_unix: Option<i64>,
}

impl WorkOsSessionMaterial {
    pub(crate) fn new(
        access_token: String,
        refresh_token: Option<String>,
        access_expires_at_unix: i64,
    ) -> Result<Self, CredentialVaultError> {
        let value = Self {
            schema_version: WORKOS_SCHEMA_VERSION,
            access_token,
            refresh_token,
            access_expires_at_unix,
            entitlement_refresh_not_before_unix: None,
        };
        value.validate().map(|()| value)
    }

    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    pub(crate) fn refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }

    pub(crate) const fn access_expires_at_unix(&self) -> i64 {
        self.access_expires_at_unix
    }

    pub(crate) const fn entitlement_refresh_not_before_unix(&self) -> Option<i64> {
        self.entitlement_refresh_not_before_unix
    }

    pub(crate) fn with_entitlement_refresh_not_before_unix(
        mut self,
        value: Option<i64>,
    ) -> Result<Self, CredentialVaultError> {
        self.entitlement_refresh_not_before_unix = value;
        self.validate().map(|()| self)
    }

    fn validate(&self) -> Result<(), CredentialVaultError> {
        if self.schema_version != WORKOS_SCHEMA_VERSION
            || invalid_secret(&self.access_token)
            || self
                .refresh_token
                .as_ref()
                .is_some_and(|value| invalid_secret(value))
            || self.access_expires_at_unix <= 0
            || self
                .entitlement_refresh_not_before_unix
                .is_some_and(|value| value < 0)
        {
            return Err(CredentialVaultError::Corrupt);
        }
        Ok(())
    }
}

impl fmt::Debug for WorkOsSessionMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorkOsSessionMaterial([REDACTED])")
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct InstallationSigningKeySeed([u8; INSTALLATION_PUBLIC_KEY_BYTES]);

impl InstallationSigningKeySeed {
    pub(crate) const fn from_bytes(bytes: [u8; INSTALLATION_PUBLIC_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) fn generate() -> Result<Self, CredentialVaultError> {
        use ring::rand::SecureRandom as _;
        let mut bytes = [0_u8; INSTALLATION_PUBLIC_KEY_BYTES];
        ring::rand::SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| CredentialVaultError::EntropyUnavailable)?;
        Ok(Self(bytes))
    }

    pub(crate) const fn expose(&self) -> &[u8; INSTALLATION_PUBLIC_KEY_BYTES] {
        &self.0
    }
}

impl fmt::Debug for InstallationSigningKeySeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InstallationSigningKeySeed([REDACTED])")
    }
}

pub(crate) struct BoundedSignedEntitlement(SignedEntitlement);

impl BoundedSignedEntitlement {
    pub(crate) fn new(value: SignedEntitlement) -> Result<Self, CredentialVaultError> {
        validate_entitlement(&value)?;
        Ok(Self(value))
    }

    pub(crate) const fn as_inner(&self) -> &SignedEntitlement {
        &self.0
    }
}

impl fmt::Debug for BoundedSignedEntitlement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BoundedSignedEntitlement([REDACTED])")
    }
}

#[derive(Debug)]
pub(crate) enum CredentialRecord {
    WorkOsSession(WorkOsSessionMaterial),
    AnonymousTrial(AnonymousTrialMaterial),
    InstallationSigningKey(InstallationSigningKeySeed),
    SignedEntitlement(BoundedSignedEntitlement),
}

impl CredentialRecord {
    const fn kind(&self) -> CredentialRecordKind {
        match self {
            Self::WorkOsSession(_) => CredentialRecordKind::WorkOsSession,
            Self::AnonymousTrial(_) => CredentialRecordKind::AnonymousTrial,
            Self::InstallationSigningKey(_) => CredentialRecordKind::InstallationSigningKey,
            Self::SignedEntitlement(_) => CredentialRecordKind::SignedEntitlement,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum CredentialVaultError {
    #[error("credential was not found in the native vault")]
    NotFound,
    #[error("native key store is locked or access was denied")]
    Locked,
    #[error("native key store is unavailable for {platform}")]
    Unavailable { platform: &'static str },
    #[error("stored credential is corrupt")]
    Corrupt,
    #[error("credential exceeds the portable native-vault limit ({actual} > {max} bytes)")]
    SecretTooLarge { max: usize, actual: usize },
    #[error("multiple credentials matched one stable record identifier")]
    Ambiguous,
    #[error("key store data root must be a safe absolute path")]
    InvalidDataRoot,
    #[error("credential record identifier is invalid")]
    InvalidRecordId,
    #[error("operating-system entropy is unavailable")]
    EntropyUnavailable,
    #[error("native key store operation failed")]
    Backend,
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(super) struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub(super) fn new(mut value: Vec<u8>) -> Result<Self, CredentialVaultError> {
        if value.len() > MAX_STORED_SECRET_BYTES {
            let actual = value.len();
            value.zeroize();
            return Err(CredentialVaultError::SecretTooLarge {
                max: MAX_STORED_SECRET_BYTES,
                actual,
            });
        }
        Ok(Self(value))
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBytes([REDACTED])")
    }
}

pub(super) trait CredentialVaultBackend: Send + Sync {
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError>;
    fn load_or_store(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError>;
    fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError>;
    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError>;
}

pub(super) fn validate_record_id(record_id: &str) -> Result<(), CredentialVaultError> {
    let Some(digest) = record_id.strip_prefix(RECORD_ID_PREFIX) else {
        return Err(CredentialVaultError::InvalidRecordId);
    };
    if digest.len() == RECORD_ID_HEX_BYTES
        && digest
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(CredentialVaultError::InvalidRecordId)
    }
}

pub(crate) struct PlatformCredentialVault {
    backend: PlatformBackend,
    record_ids: CredentialRecordIds,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn production_backend(data_root: &Path) -> PlatformBackend {
    PlatformBackend::production(data_root)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn production_backend(_data_root: &Path) -> PlatformBackend {
    PlatformBackend::production()
}

impl PlatformCredentialVault {
    pub(crate) fn production(
        data_root: &Path,
        namespace: CredentialVaultNamespace,
    ) -> Result<Self, CredentialVaultError> {
        let installation_id = crate::identity::existing_installation_id(data_root)
            .map_err(|_| CredentialVaultError::InvalidDataRoot)?
            .ok_or(CredentialVaultError::NotFound)?;
        Ok(Self {
            backend: production_backend(data_root),
            record_ids: CredentialRecordIds::new(&installation_id, namespace)?,
        })
    }

    pub(crate) fn cleanup_backend_state(data_root: &Path) -> Result<(), CredentialVaultError> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            PlatformBackend::production(data_root).cleanup_if_empty()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = data_root;
            Ok(())
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn store_file_fallback_installation_key_for_test(
        data_root: &Path,
        namespace: CredentialVaultNamespace,
        seed: [u8; INSTALLATION_PUBLIC_KEY_BYTES],
    ) -> Result<(), CredentialVaultError> {
        let installation_id = crate::identity::existing_installation_id(data_root)
            .map_err(|_| CredentialVaultError::InvalidDataRoot)?
            .ok_or(CredentialVaultError::NotFound)?;
        let record_ids = CredentialRecordIds::new(&installation_id, namespace)?;
        linux::store_file_fallback_record_for_test(
            data_root,
            record_ids.get(CredentialRecordKind::InstallationSigningKey),
            &seed,
        )
    }

    pub(crate) fn load(
        &self,
        kind: CredentialRecordKind,
    ) -> Result<CredentialRecord, CredentialVaultError> {
        load_record(&self.backend, self.record_ids.get(kind), kind)
    }

    pub(crate) fn load_or_create_installation_signing_key(
        &self,
    ) -> Result<InstallationSigningKeySeed, CredentialVaultError> {
        let candidate = InstallationSigningKeySeed::generate()?;
        let persisted = self.backend.load_or_store(
            self.record_ids
                .get(CredentialRecordKind::InstallationSigningKey),
            candidate.expose(),
        )?;
        let seed = persisted
            .as_slice()
            .try_into()
            .map_err(|_| CredentialVaultError::Corrupt)?;
        Ok(InstallationSigningKeySeed::from_bytes(seed))
    }

    pub(crate) fn store(&self, record: &CredentialRecord) -> Result<(), CredentialVaultError> {
        store_record(&self.backend, &self.record_ids, record)
    }

    pub(crate) fn store_anonymous_trial_state(
        &self,
        entitlement: BoundedSignedEntitlement,
        trial: AnonymousTrialMaterial,
    ) -> Result<(), CredentialVaultError> {
        store_state(&self.backend, &self.record_ids, entitlement, trial)
    }

    pub(crate) fn delete(&self, kind: CredentialRecordKind) -> Result<(), CredentialVaultError> {
        self.backend.delete(self.record_ids.get(kind))
    }
}

pub(crate) struct VaultInstallationChallengeSigner<'a> {
    vault: &'a PlatformCredentialVault,
}

impl<'a> VaultInstallationChallengeSigner<'a> {
    pub(crate) const fn new(vault: &'a PlatformCredentialVault) -> Self {
        Self { vault }
    }

    fn signing_key(&self) -> anyhow::Result<SigningKey> {
        let record = self
            .vault
            .load(CredentialRecordKind::InstallationSigningKey)
            .map_err(|error| anyhow!(error))?;
        let CredentialRecord::InstallationSigningKey(seed) = record else {
            return Err(anyhow!(
                "key_store_unavailable: installation key record mismatch"
            ));
        };
        Ok(SigningKey::from_bytes(seed.expose()))
    }
}

impl InstallationChallengeSigner for VaultInstallationChallengeSigner<'_> {
    fn public_key(&self) -> anyhow::Result<[u8; INSTALLATION_PUBLIC_KEY_BYTES]> {
        Ok(self.signing_key()?.verifying_key().to_bytes())
    }

    fn sign_installation_proof(
        &self,
        proof: &[u8],
    ) -> anyhow::Result<[u8; ED25519_SIGNATURE_BYTES]> {
        Ok(self.signing_key()?.sign(proof).to_bytes())
    }
}

fn load_record<B: CredentialVaultBackend>(
    backend: &B,
    record_id: &str,
    kind: CredentialRecordKind,
) -> Result<CredentialRecord, CredentialVaultError> {
    let bytes = backend.load(record_id)?;
    match kind {
        CredentialRecordKind::WorkOsSession => {
            decode_workos(bytes.as_slice()).map(CredentialRecord::WorkOsSession)
        }
        CredentialRecordKind::AnonymousTrial => {
            decode(bytes.as_slice()).map(CredentialRecord::AnonymousTrial)
        }
        CredentialRecordKind::InstallationSigningKey => {
            let seed = bytes
                .as_slice()
                .try_into()
                .map_err(|_| CredentialVaultError::Corrupt)?;
            Ok(CredentialRecord::InstallationSigningKey(
                InstallationSigningKeySeed::from_bytes(seed),
            ))
        }
        CredentialRecordKind::SignedEntitlement => {
            let value = serde_json::from_slice(bytes.as_slice())
                .map_err(|_| CredentialVaultError::Corrupt)?;
            BoundedSignedEntitlement::new(value).map(CredentialRecord::SignedEntitlement)
        }
    }
}

fn store_record<B: CredentialVaultBackend>(
    backend: &B,
    record_ids: &CredentialRecordIds,
    record: &CredentialRecord,
) -> Result<(), CredentialVaultError> {
    let bytes = match record {
        CredentialRecord::WorkOsSession(value) => encode_workos(value)?,
        CredentialRecord::AnonymousTrial(value) => encode(value)?,
        CredentialRecord::InstallationSigningKey(value) => {
            SecretBytes::new(value.expose().to_vec())?
        }
        CredentialRecord::SignedEntitlement(value) => encode_entitlement(value)?,
    };
    backend.store(record_ids.get(record.kind()), bytes.as_slice())
}

fn invalid_secret(value: &str) -> bool {
    value.is_empty() || value.len() > MAX_TOKEN_BYTES || value.as_bytes().contains(&0)
}

fn encode_workos(value: &WorkOsSessionMaterial) -> Result<SecretBytes, CredentialVaultError> {
    value.validate()?;
    SecretBytes::new(serde_json::to_vec(value).map_err(|_| CredentialVaultError::Corrupt)?)
}

fn decode_workos(bytes: &[u8]) -> Result<WorkOsSessionMaterial, CredentialVaultError> {
    let value: WorkOsSessionMaterial =
        serde_json::from_slice(bytes).map_err(|_| CredentialVaultError::Corrupt)?;
    value.validate().map(|()| value)
}

fn encode_entitlement(
    value: &BoundedSignedEntitlement,
) -> Result<SecretBytes, CredentialVaultError> {
    validate_entitlement(value.as_inner())?;
    SecretBytes::new(
        serde_json::to_vec(value.as_inner()).map_err(|_| CredentialVaultError::Corrupt)?,
    )
}

fn validate_entitlement(value: &SignedEntitlement) -> Result<(), CredentialVaultError> {
    let grant = &value.grant;
    let identifiers = [
        grant.issuer.as_str(),
        grant.key_id.as_str(),
        grant.grant_id.as_str(),
        grant.subject.as_str(),
        grant.account_id.as_str(),
        grant.product.as_str(),
        grant.installation_key_thumbprint.as_str(),
    ];
    let deadlines_valid = grant.not_before_unix <= grant.issued_at_unix
        && grant.issued_at_unix <= grant.refresh_after_unix
        && grant.refresh_after_unix <= grant.expires_at_unix
        && grant.access_deadline_unix <= grant.grace_deadline_unix
        && grant.expires_at_unix <= grant.grace_deadline_unix
        && grant.expires_at_unix.saturating_sub(grant.issued_at_unix) <= ENTITLEMENT_GRANT_SECONDS
        && grant
            .grace_deadline_unix
            .saturating_sub(grant.access_deadline_unix)
            <= ENTITLEMENT_MAX_GRACE_SECONDS
        && (grant.access_kind != EntitlementAccessKind::Trial
            || grant.grace_deadline_unix == grant.access_deadline_unix)
        && grant.issued_at_unix.saturating_sub(grant.not_before_unix)
            <= ENTITLEMENT_CLOCK_SKEW_SECONDS;
    let signature_valid = decode_base64url(&value.signature_base64url)
        .is_some_and(|signature| signature.len() == ED25519_SIGNATURE_BYTES);
    let thumbprint_valid = decode_base64url(&grant.installation_key_thumbprint)
        .is_some_and(|thumbprint| thumbprint.len() == INSTALLATION_PUBLIC_KEY_BYTES);
    let encoded_len = serde_json::to_vec(value)
        .map_err(|_| CredentialVaultError::Corrupt)?
        .len();
    if grant.schema_version != ENTITLEMENT_SCHEMA_VERSION
        || grant.product != "ctx-local-pro"
        || grant.minimum_helper_protocol > PROTOCOL_VERSION
        || identifiers
            .iter()
            .any(|identifier| identifier.is_empty() || identifier.len() > MAX_ID_BYTES)
        || grant.capabilities.is_empty()
        || !deadlines_valid
        || !signature_valid
        || !thumbprint_valid
        || encoded_len > MAX_STORED_SECRET_BYTES
    {
        return Err(CredentialVaultError::Corrupt);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    use std::sync::{Arc, Barrier};

    use ctx_pro_host_protocol::{base64url, EntitlementCapability, EntitlementGrant};

    use super::*;

    #[test]
    fn record_ids_are_deterministic_opaque_and_scoped_by_installation_and_kind(
    ) -> anyhow::Result<()> {
        let first_id = "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8";
        let second_id = "5d98d375-4ac4-4507-be4b-c435e373f042";
        let first = CredentialRecordIds::new(first_id, CredentialVaultNamespace::Production)?;
        let again = CredentialRecordIds::new(first_id, CredentialVaultNamespace::Production)?;
        let second = CredentialRecordIds::new(second_id, CredentialVaultNamespace::Production)?;
        let kinds = [
            CredentialRecordKind::WorkOsSession,
            CredentialRecordKind::AnonymousTrial,
            CredentialRecordKind::InstallationSigningKey,
            CredentialRecordKind::SignedEntitlement,
        ];
        let mut independent = BTreeSet::new();
        for kind in kinds {
            let record_id = first.get(kind);
            assert_eq!(record_id, again.get(kind));
            assert_ne!(record_id, second.get(kind));
            assert!(validate_record_id(record_id).is_ok());
            assert_eq!(
                record_id.len(),
                RECORD_ID_PREFIX.len() + RECORD_ID_HEX_BYTES
            );
            assert!(!record_id.contains(first_id));
            independent.insert(record_id);
        }
        assert_eq!(independent.len(), kinds.len());
        Ok(())
    }

    #[test]
    fn production_and_staging_ids_are_independent_and_private() -> anyhow::Result<()> {
        let installation_id = "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8";
        let production =
            CredentialRecordIds::new(installation_id, CredentialVaultNamespace::Production)?;
        let staging = CredentialRecordIds::new(installation_id, CredentialVaultNamespace::Staging)?;
        for kind in [
            CredentialRecordKind::WorkOsSession,
            CredentialRecordKind::AnonymousTrial,
            CredentialRecordKind::InstallationSigningKey,
            CredentialRecordKind::SignedEntitlement,
        ] {
            let production_id = production.get(kind);
            let staging_id = staging.get(kind);
            assert_ne!(production_id, staging_id);
            for record_id in [production_id, staging_id] {
                assert!(!record_id.contains(installation_id));
                assert!(!record_id.contains("production"));
                assert!(!record_id.contains("staging"));
            }
        }
        let error = CredentialVaultError::InvalidDataRoot.to_string();
        assert!(!error.contains(installation_id));
        assert!(!error.contains("production"));
        assert!(!error.contains("staging"));
        Ok(())
    }

    #[test]
    fn production_rejects_relative_traversing_and_filesystem_roots() {
        for data_root in [
            PathBuf::from("relative/data"),
            std::env::temp_dir()
                .join("ctx-tests")
                .join("..")
                .join("other"),
            if cfg!(target_os = "windows") {
                PathBuf::from(r"C:\")
            } else {
                PathBuf::from("/")
            },
        ] {
            assert!(matches!(
                PlatformCredentialVault::production(
                    &data_root,
                    CredentialVaultNamespace::Production
                ),
                Err(CredentialVaultError::InvalidDataRoot)
            ));
        }
    }

    #[test]
    fn record_ids_reject_invalid_installation_id_forms() {
        for installation_id in [
            "not-a-uuid",
            "00000000-0000-0000-0000-000000000000",
            "6A1DE1AB-C732-45ED-B3F8-BBF6AB1048E8",
        ] {
            assert!(matches!(
                CredentialRecordIds::new(installation_id, CredentialVaultNamespace::Production),
                Err(CredentialVaultError::InvalidDataRoot)
            ));
        }
    }

    #[test]
    fn record_id_validator_accepts_only_generated_shape() {
        for invalid in [
            "cv2-",
            "cv1-0000000000000000000000000000000000000000000000000000000000000000",
            "cv2-000000000000000000000000000000000000000000000000000000000000000",
            "cv2-000000000000000000000000000000000000000000000000000000000000000G",
        ] {
            assert_eq!(
                validate_record_id(invalid),
                Err(CredentialVaultError::InvalidRecordId)
            );
        }
    }

    #[test]
    fn trial_entitlement_requires_its_absolute_deadline_as_grace_boundary() {
        const NOW: i64 = 1_800_000_000;
        let deadline = NOW + 14 * 24 * 60 * 60;
        let mut entitlement = SignedEntitlement {
            grant: EntitlementGrant {
                schema_version: ENTITLEMENT_SCHEMA_VERSION,
                issuer: "https://pro-staging.ctx.rs".to_owned(),
                key_id: "fixture-v1".to_owned(),
                grant_id: "019f85ff-0000-7000-8000-000000000001".to_owned(),
                subject: "user_01".to_owned(),
                account_id: "account_01".to_owned(),
                product: "ctx-local-pro".to_owned(),
                access_kind: EntitlementAccessKind::Trial,
                installation_key_thumbprint: base64url(&[7; INSTALLATION_PUBLIC_KEY_BYTES]),
                issued_at_unix: NOW,
                not_before_unix: NOW - ENTITLEMENT_CLOCK_SKEW_SECONDS,
                refresh_after_unix: NOW + 4 * 24 * 60 * 60,
                access_deadline_unix: deadline,
                grace_deadline_unix: deadline,
                expires_at_unix: NOW + ENTITLEMENT_GRANT_SECONDS,
                minimum_helper_protocol: PROTOCOL_VERSION,
                revocation_epoch: 0,
                capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
            },
            signature_base64url: base64url(&[3; ED25519_SIGNATURE_BYTES]),
        };
        assert_eq!(validate_entitlement(&entitlement), Ok(()));

        entitlement.grant.grace_deadline_unix += 1;
        assert_eq!(
            validate_entitlement(&entitlement),
            Err(CredentialVaultError::Corrupt)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn live_concurrent_first_use_returns_one_persisted_key() -> anyhow::Result<()> {
        if std::env::var("CTX_TEST_LIVE_COMMERCIAL_SECRET_SERVICE").as_deref() != Ok("concurrent") {
            return Ok(());
        }
        const WORKERS: usize = 8;
        let backend = Arc::new(secret_service::PlatformBackend::production());
        let record_ids = CredentialRecordIds::new(
            "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8",
            CredentialVaultNamespace::Production,
        )?;
        let record_id = record_ids
            .get(CredentialRecordKind::InstallationSigningKey)
            .to_owned();
        assert_eq!(
            backend.load(&record_id).unwrap_err(),
            CredentialVaultError::NotFound
        );
        let barrier = Arc::new(Barrier::new(WORKERS));
        let mut workers = Vec::new();
        for byte in 1..=WORKERS as u8 {
            let backend = Arc::clone(&backend);
            let barrier = Arc::clone(&barrier);
            let record_id = record_id.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                backend.load_or_store(&record_id, &[byte; 32])
            }));
        }
        let first = workers
            .pop()
            .ok_or_else(|| anyhow!("missing worker"))?
            .join()
            .map_err(|_| anyhow!("worker panicked"))??;
        for worker in workers {
            assert_eq!(
                worker
                    .join()
                    .map_err(|_| anyhow!("worker panicked"))??
                    .as_slice(),
                first.as_slice()
            );
        }
        backend.delete(&record_id)?;
        Ok(())
    }
}

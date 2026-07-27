use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::{
    encode_entitlement, invalid_secret, BoundedSignedEntitlement, CredentialRecordIds,
    CredentialRecordKind, CredentialVaultBackend, CredentialVaultError, SecretBytes,
    MAX_STORED_SECRET_BYTES,
};

const ANONYMOUS_TRIAL_SCHEMA_VERSION: u16 = 1;
const MAX_ANONYMOUS_TRIAL_TOKEN_BYTES: usize = 2 * 1024;
const MAX_REFERRAL_CLAIM_TOKEN_BYTES: usize = 1024;
const MAX_ANONYMOUS_TRIAL_STATE_BYTES: usize = MAX_STORED_SECRET_BYTES;

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnonymousTrialMaterial {
    schema_version: u16,
    access_token: String,
    trial_deadline_unix: i64,
    #[serde(default)]
    refresh_not_before_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    referral_claim_token: Option<String>,
}

impl AnonymousTrialMaterial {
    pub(crate) fn new(
        access_token: String,
        trial_deadline_unix: i64,
    ) -> Result<Self, CredentialVaultError> {
        let value = Self {
            schema_version: ANONYMOUS_TRIAL_SCHEMA_VERSION,
            access_token,
            trial_deadline_unix,
            refresh_not_before_unix: None,
            referral_claim_token: None,
        };
        value.validate().map(|()| value)
    }

    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    pub(crate) const fn trial_deadline_unix(&self) -> i64 {
        self.trial_deadline_unix
    }

    pub(crate) const fn refresh_not_before_unix(&self) -> Option<i64> {
        self.refresh_not_before_unix
    }

    pub(crate) fn referral_claim_token(&self) -> Option<&str> {
        self.referral_claim_token.as_deref()
    }

    pub(crate) fn with_access_token(
        mut self,
        access_token: String,
    ) -> Result<Self, CredentialVaultError> {
        self.access_token = access_token;
        self.refresh_not_before_unix = None;
        self.validate().map(|()| self)
    }

    pub(crate) fn with_refresh_not_before_unix(
        mut self,
        value: Option<i64>,
    ) -> Result<Self, CredentialVaultError> {
        self.refresh_not_before_unix = value;
        self.validate().map(|()| self)
    }

    pub(crate) fn with_referral_claim_token(
        mut self,
        value: Option<String>,
    ) -> Result<Self, CredentialVaultError> {
        self.referral_claim_token = value;
        self.validate().map(|()| self)
    }

    fn validate(&self) -> Result<(), CredentialVaultError> {
        if self.schema_version != ANONYMOUS_TRIAL_SCHEMA_VERSION
            || self.access_token.len() < 16
            || self.access_token.len() > MAX_ANONYMOUS_TRIAL_TOKEN_BYTES
            || invalid_secret(&self.access_token)
            || self.access_token.bytes().any(|byte| {
                !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
            })
            || self.trial_deadline_unix <= 0
            || self
                .refresh_not_before_unix
                .is_some_and(|value| value < 0 || value > self.trial_deadline_unix)
            || self
                .referral_claim_token
                .as_deref()
                .is_some_and(invalid_referral_claim_token)
        {
            return Err(CredentialVaultError::Corrupt);
        }
        Ok(())
    }
}

impl fmt::Debug for AnonymousTrialMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AnonymousTrialMaterial([REDACTED])")
    }
}

pub(super) fn encode(value: &AnonymousTrialMaterial) -> Result<SecretBytes, CredentialVaultError> {
    value.validate()?;
    SecretBytes::new(serde_json::to_vec(value).map_err(|_| CredentialVaultError::Corrupt)?)
}

pub(super) fn decode(bytes: &[u8]) -> Result<AnonymousTrialMaterial, CredentialVaultError> {
    let value: AnonymousTrialMaterial =
        serde_json::from_slice(bytes).map_err(|_| CredentialVaultError::Corrupt)?;
    value.validate().map(|()| value)
}

pub(super) fn store_state<B: CredentialVaultBackend>(
    backend: &B,
    record_ids: &CredentialRecordIds,
    entitlement: BoundedSignedEntitlement,
    trial: AnonymousTrialMaterial,
) -> Result<(), CredentialVaultError> {
    let entitlement = encode_entitlement(&entitlement)?;
    let trial = encode(&trial)?;
    let actual = entitlement
        .as_slice()
        .len()
        .checked_add(trial.as_slice().len())
        .ok_or(CredentialVaultError::SecretTooLarge {
            max: MAX_ANONYMOUS_TRIAL_STATE_BYTES,
            actual: usize::MAX,
        })?;
    if actual > MAX_ANONYMOUS_TRIAL_STATE_BYTES {
        return Err(CredentialVaultError::SecretTooLarge {
            max: MAX_ANONYMOUS_TRIAL_STATE_BYTES,
            actual,
        });
    }

    // A trial record can refresh its entitlement on the next setup. Store it
    // first so an interrupted second write never leaves entitlement-only state.
    backend.store(
        record_ids.get(CredentialRecordKind::AnonymousTrial),
        trial.as_slice(),
    )?;
    backend.store(
        record_ids.get(CredentialRecordKind::SignedEntitlement),
        entitlement.as_slice(),
    )
}

fn invalid_referral_claim_token(value: &str) -> bool {
    value.len() < 16
        || value.len() > MAX_REFERRAL_CLAIM_TOKEN_BYTES
        || value.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use ctx_pro_host_protocol::{
        base64url, installation_key_thumbprint, EntitlementAccessKind, EntitlementCapability,
        EntitlementGrant, SignedEntitlement, ED25519_SIGNATURE_BYTES, ENTITLEMENT_SCHEMA_VERSION,
        INSTALLATION_PUBLIC_KEY_BYTES, PROTOCOL_VERSION,
    };
    use ed25519_dalek::SigningKey;

    use super::super::CredentialVaultNamespace;
    use super::*;

    #[test]
    fn anonymous_trial_material_is_bounded_and_round_trips() {
        let value = AnonymousTrialMaterial::new("a".repeat(32), 2_000)
            .unwrap()
            .with_referral_claim_token(Some("claim.token_123456".to_owned()))
            .unwrap();
        let shape = serde_json::to_value(&value).unwrap();
        assert!(shape.get("referral_claim_token").is_some());
        assert!(shape.get("referral_codename").is_none());
        let encoded = encode(&value).unwrap();
        let decoded = decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded.access_token(), "a".repeat(32));
        assert_eq!(decoded.trial_deadline_unix(), 2_000);
        assert_eq!(decoded.refresh_not_before_unix(), None);
        assert_eq!(decoded.referral_claim_token(), Some("claim.token_123456"));
        assert!(AnonymousTrialMaterial::new("short".to_owned(), 2_000).is_err());
        assert!(AnonymousTrialMaterial::new("a".repeat(32), 0).is_err());
        assert!(AnonymousTrialMaterial::new("a".repeat(32), 2_000)
            .unwrap()
            .with_referral_claim_token(Some("short".to_owned()))
            .is_err());
    }

    #[test]
    fn legacy_anonymous_trial_record_remains_compatible_and_debug_is_secret_free() {
        let legacy = br#"{"schema_version":1,"access_token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trial_deadline_unix":2000,"refresh_not_before_unix":null}"#;
        let decoded = decode(legacy).unwrap();
        assert_eq!(decoded.referral_claim_token(), None);

        let claim = "claim.token_must_never_escape";
        let value = decoded
            .with_referral_claim_token(Some(claim.to_owned()))
            .unwrap();
        let debug = format!("{value:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(claim));
    }

    #[derive(Default)]
    struct RecordingBackend {
        stores: Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl CredentialVaultBackend for RecordingBackend {
        fn load(&self, _record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
            Err(CredentialVaultError::NotFound)
        }

        fn load_or_store(
            &self,
            _record_id: &str,
            candidate: &[u8],
        ) -> Result<SecretBytes, CredentialVaultError> {
            SecretBytes::new(candidate.to_vec())
        }

        fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
            self.stores
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?
                .push((record_id.to_owned(), value.to_vec()));
            Ok(())
        }

        fn delete(&self, _record_id: &str) -> Result<(), CredentialVaultError> {
            Ok(())
        }
    }

    fn fixture_trial_entitlement() -> BoundedSignedEntitlement {
        let public_key = SigningKey::from_bytes(&[23; INSTALLATION_PUBLIC_KEY_BYTES])
            .verifying_key()
            .to_bytes();
        BoundedSignedEntitlement::new(SignedEntitlement {
            grant: EntitlementGrant {
                schema_version: ENTITLEMENT_SCHEMA_VERSION,
                issuer: "https://commercial.ctx.rs".to_owned(),
                key_id: "key-1".to_owned(),
                grant_id: "grant-1".to_owned(),
                subject: "subject-1".to_owned(),
                account_id: "account-1".to_owned(),
                product: "ctx-local-pro".to_owned(),
                access_kind: EntitlementAccessKind::Trial,
                installation_key_thumbprint: installation_key_thumbprint(&public_key),
                issued_at_unix: 1_800_000_000,
                not_before_unix: 1_799_999_700,
                refresh_after_unix: 1_800_000_100,
                access_deadline_unix: 1_800_001_000,
                grace_deadline_unix: 1_800_001_000,
                expires_at_unix: 1_800_000_600,
                minimum_helper_protocol: PROTOCOL_VERSION,
                revocation_epoch: 0,
                capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
            },
            signature_base64url: base64url(&[7; ED25519_SIGNATURE_BYTES]),
        })
        .unwrap()
    }

    #[test]
    fn anonymous_trial_state_enforces_one_aggregate_bound_before_any_store() {
        let record_ids = CredentialRecordIds::new(
            "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8",
            CredentialVaultNamespace::Production,
        )
        .unwrap();
        let entitlement_bytes = encode_entitlement(&fixture_trial_entitlement()).unwrap();
        let minimum_trial = AnonymousTrialMaterial::new("a".repeat(16), 1_800_001_000).unwrap();
        let minimum_trial_bytes = encode(&minimum_trial).unwrap();
        let maximum_token_bytes = 16 + MAX_ANONYMOUS_TRIAL_STATE_BYTES
            - entitlement_bytes.as_slice().len()
            - minimum_trial_bytes.as_slice().len();
        assert!(maximum_token_bytes <= MAX_ANONYMOUS_TRIAL_TOKEN_BYTES);

        let maximum_trial =
            AnonymousTrialMaterial::new("a".repeat(maximum_token_bytes), 1_800_001_000).unwrap();
        let backend = RecordingBackend::default();
        store_state(
            &backend,
            &record_ids,
            fixture_trial_entitlement(),
            maximum_trial,
        )
        .unwrap();
        let stores = backend.stores.lock().unwrap();
        assert_eq!(stores.len(), 2);
        assert_eq!(
            stores[0].0,
            record_ids
                .get(CredentialRecordKind::AnonymousTrial)
                .to_owned()
        );
        assert_eq!(
            stores.iter().map(|(_, value)| value.len()).sum::<usize>(),
            MAX_ANONYMOUS_TRIAL_STATE_BYTES
        );
        drop(stores);

        let oversized_trial =
            AnonymousTrialMaterial::new("a".repeat(maximum_token_bytes + 1), 1_800_001_000)
                .unwrap();
        let oversized_backend = RecordingBackend::default();
        assert_eq!(
            store_state(
                &oversized_backend,
                &record_ids,
                fixture_trial_entitlement(),
                oversized_trial,
            )
            .unwrap_err(),
            CredentialVaultError::SecretTooLarge {
                max: MAX_ANONYMOUS_TRIAL_STATE_BYTES,
                actual: MAX_ANONYMOUS_TRIAL_STATE_BYTES + 1,
            }
        );
        assert!(oversized_backend.stores.lock().unwrap().is_empty());
    }

    #[test]
    fn legacy_trial_without_referral_field_fits_the_atomic_state_commit() {
        let legacy = br#"{"schema_version":1,"access_token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trial_deadline_unix":1800001000,"refresh_not_before_unix":null}"#;
        let trial = decode(legacy).unwrap();
        let record_ids = CredentialRecordIds::new(
            "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8",
            CredentialVaultNamespace::Production,
        )
        .unwrap();
        let backend = RecordingBackend::default();
        store_state(&backend, &record_ids, fixture_trial_entitlement(), trial).unwrap();
        assert_eq!(backend.stores.lock().unwrap().len(), 2);
    }
}

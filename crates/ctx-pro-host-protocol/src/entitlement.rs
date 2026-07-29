//! Public wire contract for locally verified Pro entitlements.
//!
//! Signing and detector policy remain private. These helpers only define the
//! canonical bytes that every issuer, host, and helper must agree on.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ENTITLEMENT_SCHEMA_VERSION: u16 = 1;
pub const ENTITLEMENT_GRANT_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const ENTITLEMENT_REFRESH_REMAINING_SECONDS: i64 = 3 * 24 * 60 * 60;
pub const ENTITLEMENT_MAX_GRACE_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const ENTITLEMENT_CLOCK_SKEW_SECONDS: i64 = 5 * 60;
pub const INSTALLATION_PUBLIC_KEY_BYTES: usize = 32;
pub const AUTHORIZATION_CHALLENGE_BYTES: usize = 32;
pub const ED25519_SIGNATURE_BYTES: usize = 64;

const GRANT_DOMAIN: &[u8] = b"ctx-pro-entitlement-grant-v1\0";
const INSTALLATION_THUMBPRINT_DOMAIN: &[u8] = b"ctx-pro-installation-key-v1\0";
const INSTALLATION_PROOF_DOMAIN: &[u8] = b"ctx-pro-installation-proof-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementCapability {
    GraphRead,
    GraphWrite,
    Export,
    Migrate,
    Update,
}

impl EntitlementCapability {
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::GraphRead => "graph_read",
            Self::GraphWrite => "graph_write",
            Self::Export => "export",
            Self::Migrate => "migrate",
            Self::Update => "update",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementAccessKind {
    Trial,
    Active,
    CancelingPaid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementAccessState {
    Trial,
    Active,
    CancelingPaid,
    OfflineGrace,
    Locked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntitlementGrant {
    pub schema_version: u16,
    pub issuer: String,
    pub key_id: String,
    pub grant_id: String,
    pub subject: String,
    pub account_id: String,
    pub product: String,
    pub access_kind: EntitlementAccessKind,
    pub installation_key_thumbprint: String,
    pub issued_at_unix: i64,
    pub not_before_unix: i64,
    pub refresh_after_unix: i64,
    pub access_deadline_unix: i64,
    pub grace_deadline_unix: i64,
    pub expires_at_unix: i64,
    pub minimum_helper_protocol: u16,
    pub revocation_epoch: u64,
    pub capabilities: BTreeSet<EntitlementCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedEntitlement {
    pub grant: EntitlementGrant,
    pub signature_base64url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationRequest {
    pub entitlement: SignedEntitlement,
    pub installation_public_key_base64url: String,
    pub challenge_base64url: String,
    pub proof_signature_base64url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationResult {
    pub state: EntitlementAccessState,
    pub refresh_required: bool,
    pub expires_at_unix: i64,
    pub access_deadline_unix: i64,
    pub grace_deadline_unix: i64,
    pub capabilities: BTreeSet<EntitlementCapability>,
}

/// Produces the language-independent bytes signed by the online issuer.
#[must_use]
pub fn canonical_grant_bytes(grant: &EntitlementGrant) -> Vec<u8> {
    let mut output = Vec::with_capacity(512);
    output.extend_from_slice(GRANT_DOMAIN);
    push_u16(&mut output, grant.schema_version);
    push_string(&mut output, &grant.issuer);
    push_string(&mut output, &grant.key_id);
    push_string(&mut output, &grant.grant_id);
    push_string(&mut output, &grant.subject);
    push_string(&mut output, &grant.account_id);
    push_string(&mut output, &grant.product);
    push_string(
        &mut output,
        match grant.access_kind {
            EntitlementAccessKind::Trial => "trial",
            EntitlementAccessKind::Active => "active",
            EntitlementAccessKind::CancelingPaid => "canceling_paid",
        },
    );
    push_string(&mut output, &grant.installation_key_thumbprint);
    for value in [
        grant.issued_at_unix,
        grant.not_before_unix,
        grant.refresh_after_unix,
        grant.access_deadline_unix,
        grant.grace_deadline_unix,
        grant.expires_at_unix,
    ] {
        output.extend_from_slice(&value.to_be_bytes());
    }
    push_u16(&mut output, grant.minimum_helper_protocol);
    output.extend_from_slice(&grant.revocation_epoch.to_be_bytes());
    push_u16(
        &mut output,
        u16::try_from(grant.capabilities.len()).unwrap_or(u16::MAX),
    );
    for capability in &grant.capabilities {
        push_string(&mut output, capability.wire_name());
    }
    output
}

/// Stable installation-key identity used in the signed grant.
#[must_use]
pub fn installation_key_thumbprint(public_key: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(INSTALLATION_THUMBPRINT_DOMAIN);
    hash.update(public_key);
    base64url(&hash.finalize())
}

/// Produces the bytes signed by an installation key for one helper challenge.
#[must_use]
pub fn installation_proof_bytes(grant: &EntitlementGrant, challenge: &[u8]) -> Vec<u8> {
    let grant_digest = Sha256::digest(canonical_grant_bytes(grant));
    let mut output =
        Vec::with_capacity(INSTALLATION_PROOF_DOMAIN.len() + grant_digest.len() + challenge.len());
    output.extend_from_slice(INSTALLATION_PROOF_DOMAIN);
    output.extend_from_slice(&grant_digest);
    output.extend_from_slice(challenge);
    output
}

#[must_use]
pub fn base64url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[must_use]
pub fn decode_base64url(value: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    if value.is_empty() || value.contains('=') {
        return None;
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .ok()
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn push_string(output: &mut Vec<u8>, value: &str) {
    let length = u32::try_from(value.len()).unwrap_or(u32::MAX);
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct GoldenVector {
        grant: EntitlementGrant,
        issuer_public_key_base64url: String,
        installation_public_key_base64url: String,
        challenge_base64url: String,
        canonical_grant_hex: String,
        canonical_grant_sha256: String,
        grant_signature_base64url: String,
        installation_proof_signature_base64url: String,
    }

    #[test]
    fn canonical_bytes_do_not_depend_on_capability_insertion_order() {
        let mut grant = fixture();
        let first = canonical_grant_bytes(&grant);
        grant.capabilities = [
            EntitlementCapability::Update,
            EntitlementCapability::GraphRead,
            EntitlementCapability::Export,
        ]
        .into_iter()
        .collect();
        assert_eq!(first, canonical_grant_bytes(&grant));
    }

    #[test]
    fn cross_repo_golden_vector_verifies_both_signatures() {
        let vector: GoldenVector =
            serde_json::from_str(include_str!("../testdata/entitlement/v1/golden.json"))
                .unwrap_or_else(|_| panic!("golden entitlement"));
        let canonical = canonical_grant_bytes(&vector.grant);
        assert_eq!(hex(&canonical), vector.canonical_grant_hex);
        assert_eq!(
            format!("{:x}", Sha256::digest(&canonical)),
            vector.canonical_grant_sha256
        );
        assert_eq!(vector.grant.issuer, "https://pro-staging.ctx.rs");

        let issuer = key(&vector.issuer_public_key_base64url);
        issuer
            .verify(&canonical, &signature(&vector.grant_signature_base64url))
            .unwrap_or_else(|_| panic!("issuer signature"));
        let installation_bytes: [u8; INSTALLATION_PUBLIC_KEY_BYTES] =
            decode_base64url(&vector.installation_public_key_base64url)
                .and_then(|bytes| bytes.try_into().ok())
                .unwrap_or_else(|| panic!("installation key"));
        assert_eq!(
            installation_key_thumbprint(&installation_bytes),
            vector.grant.installation_key_thumbprint
        );
        let challenge =
            decode_base64url(&vector.challenge_base64url).unwrap_or_else(|| panic!("challenge"));
        key(&vector.installation_public_key_base64url)
            .verify(
                &installation_proof_bytes(&vector.grant, &challenge),
                &signature(&vector.installation_proof_signature_base64url),
            )
            .unwrap_or_else(|_| panic!("installation proof"));
    }

    #[test]
    fn current_staging_key_fixture_excludes_the_retired_issuer() {
        let document: serde_json::Value = serde_json::from_str(include_str!(
            "../testdata/entitlement/v1/verification-keys.json"
        ))
        .unwrap_or_else(|_| panic!("staging verification keys"));
        let keys = document["keys"]
            .as_array()
            .unwrap_or_else(|| panic!("staging verification key list"));
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0]["key_id"], "staging-2026-07-v3");
        assert_eq!(keys[0]["issuer"], "https://pro-staging.ctx.rs");
        assert_ne!(keys[0]["issuer"], "https://commercial.staging.ctx.rs");
    }

    fn key(value: &str) -> VerifyingKey {
        let bytes = decode_base64url(value)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or_else(|| panic!("verification key"));
        VerifyingKey::from_bytes(&bytes).unwrap_or_else(|_| panic!("verification key"))
    }

    fn signature(value: &str) -> Signature {
        let bytes = decode_base64url(value)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or_else(|| panic!("signature"));
        Signature::from_bytes(&bytes)
    }

    fn hex(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            output.push(DIGITS[(byte >> 4) as usize] as char);
            output.push(DIGITS[(byte & 0xf) as usize] as char);
        }
        output
    }

    fn fixture() -> EntitlementGrant {
        EntitlementGrant {
            schema_version: ENTITLEMENT_SCHEMA_VERSION,
            issuer: "https://commercial.ctx.rs".to_owned(),
            key_id: "staging-2026-07-v1".to_owned(),
            grant_id: "019f85ff-0000-7000-8000-000000000001".to_owned(),
            subject: "user_01".to_owned(),
            account_id: "account_01".to_owned(),
            product: "ctx-local-pro".to_owned(),
            access_kind: EntitlementAccessKind::Trial,
            installation_key_thumbprint: "fixture-thumbprint".to_owned(),
            issued_at_unix: 1_800_000_000,
            not_before_unix: 1_799_999_700,
            refresh_after_unix: 1_800_345_600,
            access_deadline_unix: 1_801_209_600,
            grace_deadline_unix: 1_801_814_400,
            expires_at_unix: 1_800_604_800,
            minimum_helper_protocol: crate::PROTOCOL_VERSION,
            revocation_epoch: 0,
            capabilities: [
                EntitlementCapability::GraphRead,
                EntitlementCapability::Export,
                EntitlementCapability::Update,
            ]
            .into_iter()
            .collect(),
        }
    }
}

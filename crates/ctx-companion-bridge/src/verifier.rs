//! Detached managed-pair verification for installation and distribution only.
//!
//! Runtime launch does not call this module.

mod contract;
mod target;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ring::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{identity::Sha256Digest, BridgeError};

use self::{
    contract::{
        AuthorityChannel, AuthorityRegistry, BuildIdentityDocument, ComponentDocument, Envelope,
        Manifest,
    },
    target::TargetSpec,
};

const EMBEDDED_AUTHORITY: &[u8] =
    include_bytes!("../../../contracts/ctx-managed-pair-release-authority-v1.json");
const EMBEDDED_STATE_SCHEMA: &[u8] =
    include_bytes!("../../../contracts/ctx-managed-pair-state-v1.schema.json");
#[cfg(test)]
const EMBEDDED_TARGET_MATRIX: &[u8] = include_bytes!("../../../contracts/release-targets-v1.json");
const STATE_SCHEMA_SHA256: &str =
    "bc81eae66d02e436e3f97cbcc5e019508cf9591be05eb8e4bf86ad4659e7dbe1";
const TARGET_MATRIX_SHA256: &str =
    "718d2f364e10f57e3a98228d8feaea59c955db0fd7da309a7b0479a6296e18ef";
const MAX_ENVELOPE_BYTES: usize = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 16 * 1024;
const MAX_COMPONENT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ROLLBACK_GENERATION: u64 = 9_007_199_254_740_991;

pub const MANAGED_PAIR_ENVELOPE_FILENAME: &str = "managed-pair-envelope.json";
pub const MANAGED_PAIR_STATE_FILENAME: &str = "managed-pair-state.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignedManagedPairTarget {
    LinuxArm64,
    LinuxX64,
    MacosArm64,
    MacosX64,
    WindowsX64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignedManagedPairComponentIdentity {
    sha256: Sha256Digest,
    size_bytes: u64,
}

impl SignedManagedPairComponentIdentity {
    pub const fn sha256(self) -> Sha256Digest {
        self.sha256
    }

    pub const fn size_bytes(self) -> u64 {
        self.size_bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedManagedPairIdentity {
    release_name: String,
    target: SignedManagedPairTarget,
    rollback_generation: u64,
    manifest_sha256: Sha256Digest,
    core: SignedManagedPairComponentIdentity,
    companion: SignedManagedPairComponentIdentity,
}

impl SignedManagedPairIdentity {
    pub fn release_name(&self) -> &str {
        &self.release_name
    }

    pub const fn target(&self) -> SignedManagedPairTarget {
        self.target
    }

    pub const fn rollback_generation(&self) -> u64 {
        self.rollback_generation
    }

    pub const fn manifest_sha256(&self) -> Sha256Digest {
        self.manifest_sha256
    }

    pub const fn core(&self) -> SignedManagedPairComponentIdentity {
        self.core
    }

    pub const fn companion(&self) -> SignedManagedPairComponentIdentity {
        self.companion
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseChannel {
    Stable,
    Staging,
}

impl ReleaseChannel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Staging => "staging",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ManagedPairExpectations {
    channel: ReleaseChannel,
}

impl ManagedPairExpectations {
    pub const fn new(channel: ReleaseChannel) -> Self {
        Self { channel }
    }

    /// This identity is injected only after the signed manifest has been
    /// verified against the corresponding managed-pair expectation.
    pub const fn channel(&self) -> ReleaseChannel {
        self.channel
    }
}

struct VerifiedEnvelope {
    identity: SignedManagedPairIdentity,
}

pub fn verify_signed_managed_pair_envelope(
    expectations: &ManagedPairExpectations,
    envelope_bytes: &[u8],
) -> Result<SignedManagedPairIdentity, BridgeError> {
    #[cfg(ctx_release_qualification)]
    match std::env::var("CTX_RELEASE_MANAGED_PAIR_AUTHORITY_JSON") {
        Ok(authority) => {
            if authority.is_empty() || authority.len() > 16 * 1024 {
                return Err(verification("qualification authority exceeds its bound"));
            }
            return verify_envelope(expectations, authority.as_bytes(), envelope_bytes)
                .map(|value| value.identity);
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(_) => return Err(verification("qualification authority is not UTF-8")),
    }
    verify_envelope(expectations, EMBEDDED_AUTHORITY, envelope_bytes).map(|value| value.identity)
}

fn verify_envelope(
    expectations: &ManagedPairExpectations,
    authority_bytes: &[u8],
    envelope_bytes: &[u8],
) -> Result<VerifiedEnvelope, BridgeError> {
    if envelope_bytes.is_empty() || envelope_bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(verification("detached envelope exceeds its bound"));
    }
    if digest(EMBEDDED_STATE_SCHEMA).to_hex() != STATE_SCHEMA_SHA256 {
        return Err(verification(
            "compiled managed-pair state V1 schema identity is invalid",
        ));
    }
    let authorities = parse_authority(authority_bytes)?;
    let envelope: Envelope = parse_closed_json(envelope_bytes, "detached envelope")?;
    if envelope.schema_version != 1 {
        return Err(verification("detached envelope is not V1"));
    }
    let payload_bytes = strict_base64(
        &envelope.manifest_base64,
        MAX_MANIFEST_BYTES,
        "manifest payload",
    )?;
    let signature = strict_base64(
        &envelope.signature_base64,
        MAX_SIGNATURE_BYTES,
        "manifest signature",
    )?;
    let payload_value: Value = parse_closed_json(&payload_bytes, "manifest payload")?;
    let canonical = serde_json::to_vec(&payload_value)
        .map_err(|_| verification("manifest payload cannot be canonicalized"))?;
    if canonical != payload_bytes {
        return Err(verification(
            "manifest payload is not compact canonical JSON",
        ));
    }
    let manifest: Manifest = serde_json::from_value(payload_value)
        .map_err(|_| verification("managed-pair manifest is malformed"))?;
    if manifest.contract != "ctx-managed-pair-manifest" || manifest.schema_version != 1 {
        return Err(verification(
            "envelope does not contain a V1 target manifest",
        ));
    }
    let authority = select_authority(&authorities, &manifest.channel)?;
    if manifest.channel != expectations.channel.as_str()
        || manifest.release_authority_key_id != authority.key_id
    {
        return Err(verification(
            "manifest channel or release key does not match Core",
        ));
    }
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &authority.public_key_der)
        .verify(&payload_bytes, &signature)
        .map_err(|_| verification("detached manifest signature is invalid"))?;
    let target = TargetSpec::current()?;
    validate_manifest_identity(&manifest, target)?;
    let identity = signed_identity(&manifest, target, &payload_bytes)?;
    Ok(VerifiedEnvelope { identity })
}

struct VerifiedAuthority {
    id: String,
    key_id: String,
    public_key_der: Vec<u8>,
}

fn parse_authority(bytes: &[u8]) -> Result<Vec<VerifiedAuthority>, BridgeError> {
    let registry: AuthorityRegistry = parse_closed_json(bytes, "embedded release authority")?;
    if registry.contract != "ctx-managed-pair-release-authority"
        || registry.schema_version != 1
        || registry.channels.len() != 2
    {
        return Err(verification(
            "embedded release authority is not the exact V1 registry",
        ));
    }
    let mut result = Vec::with_capacity(2);
    for (expected_id, channel) in ["stable", "staging"].into_iter().zip(registry.channels) {
        result.push(validate_authority_channel(channel, expected_id)?);
    }
    Ok(result)
}

fn validate_authority_channel(
    channel: AuthorityChannel,
    expected_id: &str,
) -> Result<VerifiedAuthority, BridgeError> {
    if channel.id != expected_id
        || channel.signature_algorithm != "rsa-pkcs1v15-sha256"
        || !is_name(&channel.key_id)
    {
        return Err(verification(
            "embedded release authority channel is malformed",
        ));
    }
    let lines: Vec<_> = channel.public_key_pem.lines().collect();
    if lines.first() != Some(&"-----BEGIN RSA PUBLIC KEY-----")
        || lines.last() != Some(&"-----END RSA PUBLIC KEY-----")
    {
        return Err(verification(
            "embedded release authority key is not RSA public-key PEM",
        ));
    }
    let body = lines[1..lines.len() - 1].concat();
    let der = strict_base64(&body, 16 * 1024, "embedded authority key")?;
    let fingerprint = digest(&der);
    if fingerprint.to_hex() != channel.public_key_der_sha256 {
        return Err(verification(
            "embedded release authority key fingerprint is invalid",
        ));
    }
    Ok(VerifiedAuthority {
        id: channel.id,
        key_id: channel.key_id,
        public_key_der: der,
    })
}

fn select_authority<'a>(
    authorities: &'a [VerifiedAuthority],
    channel: &str,
) -> Result<&'a VerifiedAuthority, BridgeError> {
    authorities
        .iter()
        .find(|authority| authority.id == channel)
        .ok_or_else(|| verification("manifest channel is unsupported"))
}

fn validate_manifest_identity(manifest: &Manifest, target: TargetSpec) -> Result<(), BridgeError> {
    if !is_name(&manifest.release_name)
        || manifest.target_matrix_sha256 != TARGET_MATRIX_SHA256
        || manifest.rollback_generation == 0
        || manifest.rollback_generation > MAX_ROLLBACK_GENERATION
    {
        return Err(verification("manifest release identity is malformed"));
    }
    if manifest.target.id != target.id
        || manifest.target.os != target.os
        || manifest.target.arch != target.arch
        || manifest.target.core_rust_target != target.rust_target
        || manifest.target.companion_rust_target != target.rust_target
    {
        return Err(verification(
            "manifest target does not match this Core build",
        ));
    }
    if manifest.install_geometry.install_root != "<install-root>"
        || manifest.install_geometry.managed_bin_dir != "<install-root>/bin"
        || manifest.install_geometry.core_slot != target.core_slot
        || manifest.install_geometry.companion_slot != target.companion_slot
    {
        return Err(verification(
            "manifest does not use the fixed managed slots",
        ));
    }
    if manifest.snapshot.contract != "ctx-managed-pair-snapshot-v1"
        || parse_digest(&manifest.snapshot.fingerprint).is_err()
        || parse_digest(&manifest.compatibility.invocation_fingerprint).is_err()
        || parse_digest(&manifest.compatibility.core_capability_fingerprint).is_err()
    {
        return Err(verification("manifest compatibility metadata is malformed"));
    }
    validate_component_document(
        &manifest.components.core,
        "core",
        target.core_artifact,
        target.core_slot,
        target.rust_target,
    )?;
    validate_component_document(
        &manifest.components.companion,
        "companion",
        target.companion_artifact,
        target.companion_slot,
        target.rust_target,
    )
}

fn signed_identity(
    manifest: &Manifest,
    target: TargetSpec,
    payload_bytes: &[u8],
) -> Result<SignedManagedPairIdentity, BridgeError> {
    let target = match target.id {
        "linux-arm64" => SignedManagedPairTarget::LinuxArm64,
        "linux-x64" => SignedManagedPairTarget::LinuxX64,
        "macos-arm64" => SignedManagedPairTarget::MacosArm64,
        "macos-x64" => SignedManagedPairTarget::MacosX64,
        "windows-x64" => SignedManagedPairTarget::WindowsX64,
        _ => return Err(BridgeError::UnsupportedPlatform),
    };
    Ok(SignedManagedPairIdentity {
        release_name: manifest.release_name.clone(),
        target,
        rollback_generation: manifest.rollback_generation,
        manifest_sha256: digest(payload_bytes),
        core: SignedManagedPairComponentIdentity {
            sha256: parse_digest(&manifest.components.core.sha256)?,
            size_bytes: manifest.components.core.size_bytes,
        },
        companion: SignedManagedPairComponentIdentity {
            sha256: parse_digest(&manifest.components.companion.sha256)?,
            size_bytes: manifest.components.companion.size_bytes,
        },
    })
}

fn validate_component_document(
    component: &ComponentDocument,
    kind: &str,
    artifact: &str,
    slot: &str,
    rust_target: &str,
) -> Result<(), BridgeError> {
    let signed_digest = parse_digest(&component.sha256)?;
    if component.artifact_name != artifact
        || component.object_key != format!("sha256/{signed_digest}/{artifact}")
        || component.install_slot != slot
        || component.size_bytes == 0
        || component.size_bytes > MAX_COMPONENT_BYTES
    {
        return Err(verification("signed component identity is malformed"));
    }
    validate_build_identity(&component.build_identity, kind, rust_target)
}

fn validate_build_identity(
    identity: &BuildIdentityDocument,
    kind: &str,
    rust_target: &str,
) -> Result<(), BridgeError> {
    if identity.component != kind
        || identity.rust_target != rust_target
        || !is_lower_hex(&identity.source_revision, 40)
        || parse_digest(&identity.build_fingerprint).is_err()
    {
        return Err(verification("signed component build identity is malformed"));
    }
    Ok(())
}

fn strict_base64(
    value: &str,
    maximum_decoded_bytes: usize,
    label: &'static str,
) -> Result<Vec<u8>, BridgeError> {
    if value.is_empty() || value.len() > maximum_decoded_bytes.saturating_mul(4) / 3 + 8 {
        return Err(verification(label));
    }
    let decoded = BASE64.decode(value).map_err(|_| verification(label))?;
    if decoded.is_empty()
        || decoded.len() > maximum_decoded_bytes
        || BASE64.encode(&decoded) != value
    {
        return Err(verification(label));
    }
    Ok(decoded)
}

fn parse_closed_json<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    label: &'static str,
) -> Result<T, BridgeError> {
    serde_json::from_slice(bytes).map_err(|_| verification(label))
}

fn parse_digest(value: &str) -> Result<Sha256Digest, BridgeError> {
    Sha256Digest::from_hex(value)
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes).into())
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
}

fn verification(message: &'static str) -> BridgeError {
    BridgeError::Verification(message.to_owned())
}

#[cfg(test)]
pub(crate) fn embedded_authority_for_tests() -> &'static [u8] {
    EMBEDDED_AUTHORITY
}

#[cfg(test)]
pub(crate) fn embedded_state_schema_for_tests() -> &'static [u8] {
    EMBEDDED_STATE_SCHEMA
}

#[cfg(test)]
pub(crate) fn embedded_target_matrix_for_tests() -> &'static [u8] {
    EMBEDDED_TARGET_MATRIX
}

#[cfg(test)]
mod component_tests {
    use serde_json::{json, Value};

    use super::*;

    const CORE_SHA: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    #[test]
    fn signed_component_bound_is_exactly_256_mib() {
        let target = TargetSpec::current().unwrap();
        let component_at = |size_bytes| {
            serde_json::from_value::<ComponentDocument>(component(
                "core",
                target.core_artifact,
                target.core_slot,
                target.rust_target,
                CORE_SHA,
                size_bytes,
            ))
            .unwrap()
        };
        let accepted = component_at(MAX_COMPONENT_BYTES);
        assert!(validate_component_document(
            &accepted,
            "core",
            target.core_artifact,
            target.core_slot,
            target.rust_target,
        )
        .is_ok());

        let rejected = component_at(MAX_COMPONENT_BYTES + 1);
        assert!(validate_component_document(
            &rejected,
            "core",
            target.core_artifact,
            target.core_slot,
            target.rust_target,
        )
        .is_err());
    }

    fn component(
        kind: &str,
        artifact: &str,
        slot: &str,
        rust_target: &str,
        sha256: &str,
        size_bytes: u64,
    ) -> Value {
        json!({
            "artifact_name": artifact,
            "object_key": format!("sha256/{sha256}/{artifact}"),
            "sha256": sha256,
            "size_bytes": size_bytes,
            "install_slot": slot,
            "build_identity": {
                "component": kind,
                "rust_target": rust_target,
                "source_revision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "build_fingerprint": CORE_SHA,
            },
        })
    }
}

#[cfg(test)]
mod unified_tests;

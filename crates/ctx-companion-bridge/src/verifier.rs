mod contract;
mod target;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ring::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{
    identity::Sha256Digest,
    slot::{PreparedPair, ENVELOPE_RELATIVE_PATH, MAX_COMPONENT_BYTES, STATE_RELATIVE_PATH},
    BridgeError,
};

use self::{
    contract::{
        AuthorityChannel, AuthorityRegistry, BuildIdentityDocument, ComponentDocument, Envelope,
        ManagedPairComponentIdentityDocument, ManagedPairState, Manifest,
        VerifiedManagedPairIdentityDocument,
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
    "1cf089c8f494c9662428518ce07ff91a3ceb28fe4ac4d75b6a9d7dd3f16c75a5";
const MAX_ENVELOPE_BYTES: usize = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 16 * 1024;
const MAX_STATE_BYTES: usize = 64 * 1024;
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
pub struct CoreBuildIdentity {
    source_revision: String,
}

impl CoreBuildIdentity {
    pub fn new(source_revision: impl Into<String>) -> Result<Self, BridgeError> {
        let source_revision = source_revision.into();
        if !is_lower_hex(&source_revision, 40) {
            return Err(verification("current Core source revision is malformed"));
        }
        Ok(Self { source_revision })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CompatibilityIdentity {
    invocation_fingerprint: Sha256Digest,
    core_capability_fingerprint: Sha256Digest,
}

impl CompatibilityIdentity {
    pub const fn new(
        invocation_fingerprint: Sha256Digest,
        core_capability_fingerprint: Sha256Digest,
    ) -> Self {
        Self {
            invocation_fingerprint,
            core_capability_fingerprint,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ManagedPairExpectations {
    channel: ReleaseChannel,
    core: CoreBuildIdentity,
    compatibility: CompatibilityIdentity,
}

impl ManagedPairExpectations {
    pub const fn new(
        channel: ReleaseChannel,
        core: CoreBuildIdentity,
        compatibility: CompatibilityIdentity,
    ) -> Self {
        Self {
            channel,
            core,
            compatibility,
        }
    }

    /// This identity is injected only after the signed manifest has been
    /// verified against the corresponding managed-pair expectation.
    pub const fn channel(&self) -> ReleaseChannel {
        self.channel
    }
}

pub(crate) trait PairVerifier {
    fn verify(&self, pair: &PreparedPair) -> Result<(), BridgeError>;
}

pub(crate) struct ProductionVerifier<'a> {
    expectations: &'a ManagedPairExpectations,
    authority: &'static [u8],
}

impl<'a> ProductionVerifier<'a> {
    pub(crate) const fn new(expectations: &'a ManagedPairExpectations) -> Self {
        Self {
            expectations,
            authority: EMBEDDED_AUTHORITY,
        }
    }
}

impl PairVerifier for ProductionVerifier<'_> {
    fn verify(&self, pair: &PreparedPair) -> Result<(), BridgeError> {
        let envelope_bytes = pair.read_shared_file(&ENVELOPE_RELATIVE_PATH, MAX_ENVELOPE_BYTES)?;
        let verified = verify_envelope(
            self.expectations,
            self.authority,
            &envelope_bytes,
            CoreRevisionPolicy::Exact,
        )?;
        validate_installed_components(
            &verified.manifest,
            verified.target,
            pair,
            self.expectations,
        )?;
        validate_state(
            pair,
            &verified.manifest,
            verified.target,
            &verified.payload_bytes,
            &envelope_bytes,
        )?;
        pair.execution.verify_retained()?;
        Ok(())
    }
}

struct VerifiedEnvelope {
    manifest: Manifest,
    payload_bytes: Vec<u8>,
    target: TargetSpec,
    identity: SignedManagedPairIdentity,
}

pub fn verify_signed_managed_pair_envelope(
    expectations: &ManagedPairExpectations,
    envelope_bytes: &[u8],
) -> Result<SignedManagedPairIdentity, BridgeError> {
    verify_envelope(
        expectations,
        EMBEDDED_AUTHORITY,
        envelope_bytes,
        CoreRevisionPolicy::WellFormed,
    )
    .map(|value| value.identity)
}

#[derive(Clone, Copy)]
enum CoreRevisionPolicy {
    Exact,
    WellFormed,
}

fn verify_envelope(
    expectations: &ManagedPairExpectations,
    authority_bytes: &[u8],
    envelope_bytes: &[u8],
    core_revision: CoreRevisionPolicy,
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
    validate_manifest_identity(&manifest, target, expectations, core_revision)?;
    let identity = signed_identity(&manifest, target, &payload_bytes)?;
    Ok(VerifiedEnvelope {
        manifest,
        payload_bytes,
        target,
        identity,
    })
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

fn validate_manifest_identity(
    manifest: &Manifest,
    target: TargetSpec,
    expected: &ManagedPairExpectations,
    core_revision: CoreRevisionPolicy,
) -> Result<(), BridgeError> {
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
        || manifest.target.core_rust_target != target.core_rust_target
        || manifest.target.companion_rust_target != target.companion_rust_target
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
        || parse_digest(&manifest.compatibility.invocation_fingerprint)?
            != expected.compatibility.invocation_fingerprint
        || parse_digest(&manifest.compatibility.core_capability_fingerprint)?
            != expected.compatibility.core_capability_fingerprint
    {
        return Err(verification(
            "manifest compatibility does not match current Core",
        ));
    }
    validate_component_document(
        &manifest.components.core,
        "core",
        target.core_artifact,
        target.core_slot,
        target.core_rust_target,
        matches!(core_revision, CoreRevisionPolicy::Exact).then_some(&expected.core),
    )?;
    validate_component_document(
        &manifest.components.companion,
        "companion",
        target.companion_artifact,
        target.companion_slot,
        target.companion_rust_target,
        None,
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

fn validate_installed_components(
    manifest: &Manifest,
    target: TargetSpec,
    pair: &PreparedPair,
    expected: &ManagedPairExpectations,
) -> Result<(), BridgeError> {
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
        || manifest.target.core_rust_target != target.core_rust_target
        || manifest.target.companion_rust_target != target.companion_rust_target
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
        || parse_digest(&manifest.compatibility.invocation_fingerprint)?
            != expected.compatibility.invocation_fingerprint
        || parse_digest(&manifest.compatibility.core_capability_fingerprint)?
            != expected.compatibility.core_capability_fingerprint
    {
        return Err(verification(
            "manifest compatibility does not match current Core",
        ));
    }
    validate_component(
        &manifest.components.core,
        "core",
        target.core_artifact,
        target.core_slot,
        target.core_rust_target,
        pair.identity.launcher(),
        Some(&expected.core),
    )?;
    validate_component(
        &manifest.components.companion,
        "companion",
        target.companion_artifact,
        target.companion_slot,
        target.companion_rust_target,
        pair.identity.companion(),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_component(
    component: &ComponentDocument,
    kind: &str,
    artifact: &str,
    slot: &str,
    rust_target: &str,
    actual: &crate::FileIdentity,
    expected_core: Option<&CoreBuildIdentity>,
) -> Result<(), BridgeError> {
    validate_component_document(component, kind, artifact, slot, rust_target, expected_core)?;
    let signed_digest = parse_digest(&component.sha256)?;
    if component.size_bytes != actual.size_bytes() || signed_digest != actual.sha256() {
        return Err(verification(
            "signed component does not match fixed executable bytes",
        ));
    }
    Ok(())
}

fn validate_component_document(
    component: &ComponentDocument,
    kind: &str,
    artifact: &str,
    slot: &str,
    rust_target: &str,
    expected_core: Option<&CoreBuildIdentity>,
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
    validate_build_identity(&component.build_identity, kind, rust_target, expected_core)
}

fn validate_build_identity(
    identity: &BuildIdentityDocument,
    kind: &str,
    rust_target: &str,
    expected_core: Option<&CoreBuildIdentity>,
) -> Result<(), BridgeError> {
    if identity.component != kind
        || identity.rust_target != rust_target
        || !is_lower_hex(&identity.source_revision, 40)
        || parse_digest(&identity.build_fingerprint).is_err()
    {
        return Err(verification("signed component build identity is malformed"));
    }
    if let Some(expected) = expected_core {
        if identity.source_revision != expected.source_revision {
            return Err(verification(
                "signed Core build identity does not match this process",
            ));
        }
    }
    Ok(())
}

fn validate_state(
    pair: &PreparedPair,
    manifest: &Manifest,
    target: TargetSpec,
    payload_bytes: &[u8],
    envelope_bytes: &[u8],
) -> Result<(), BridgeError> {
    let bytes = pair.read_shared_file(&STATE_RELATIVE_PATH, MAX_STATE_BYTES)?;
    let state: ManagedPairState = parse_closed_json(&bytes, "managed-pair state")?;
    validate_state_document(&state, manifest, target, payload_bytes, envelope_bytes)
}

fn validate_state_document(
    state: &ManagedPairState,
    manifest: &Manifest,
    target: TargetSpec,
    payload_bytes: &[u8],
    envelope_bytes: &[u8],
) -> Result<(), BridgeError> {
    if state.contract != "ctx-managed-pair-state"
        || state.schema_version != 1
        || state.envelope_size_bytes == 0
        || state.envelope_size_bytes > MAX_ENVELOPE_BYTES as u64
        || state.envelope_size_bytes != envelope_bytes.len() as u64
        || parse_digest(&state.envelope_sha256)? != digest(envelope_bytes)
        || !state_identity_matches(&state.identity, manifest, target, payload_bytes)?
    {
        return Err(verification(
            "managed-pair state does not retain this verified signed pair",
        ));
    }
    Ok(())
}

fn state_identity_matches(
    state: &VerifiedManagedPairIdentityDocument,
    manifest: &Manifest,
    target: TargetSpec,
    payload_bytes: &[u8],
) -> Result<bool, BridgeError> {
    Ok(state.release_name == manifest.release_name
        && state.target == target.id
        && state.rollback_generation == manifest.rollback_generation
        && parse_digest(&state.manifest_sha256)? == digest(payload_bytes)
        && state_component_matches(&state.core, &manifest.components.core)?
        && state_component_matches(&state.companion, &manifest.components.companion)?)
}

fn state_component_matches(
    state: &ManagedPairComponentIdentityDocument,
    signed: &ComponentDocument,
) -> Result<bool, BridgeError> {
    Ok(state.size_bytes > 0
        && state.size_bytes <= MAX_COMPONENT_BYTES
        && state.size_bytes == signed.size_bytes
        && parse_digest(&state.sha256)? == parse_digest(&signed.sha256)?)
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
mod state_tests {
    use serde_json::{json, Value};

    use super::*;

    const CORE_SHA: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const COMPANION_SHA: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn managed_pair_state_binds_envelope_and_complete_verified_identity() {
        let target = TargetSpec::current().unwrap();
        let payload = b"canonical-manifest";
        let envelope = b"signed-envelope";
        let manifest = manifest(target);
        let valid = state_value(target, payload, envelope);
        let mut worker_bytes = serde_json::to_vec_pretty(&valid).unwrap();
        worker_bytes.push(b'\n');
        let state: ManagedPairState = serde_json::from_slice(&worker_bytes).unwrap();
        validate_state_document(&state, &manifest, target, payload, envelope).unwrap();

        for (pointer, replacement) in [
            ("/contract", json!("ctx-managed-pair-state-v2")),
            ("/schema_version", json!(2)),
            ("/identity/release_name", json!("v1.2.4")),
            ("/identity/target", json!("unsupported-target")),
            ("/identity/rollback_generation", json!(8)),
            ("/identity/manifest_sha256", json!(CORE_SHA)),
            (
                "/identity/core/sha256",
                json!("3333333333333333333333333333333333333333333333333333333333333333"),
            ),
            ("/identity/core/size_bytes", json!(18)),
            (
                "/identity/companion/sha256",
                json!("4444444444444444444444444444444444444444444444444444444444444444"),
            ),
            ("/identity/companion/size_bytes", json!(30)),
            ("/envelope_sha256", json!(CORE_SHA)),
            ("/envelope_size_bytes", json!(envelope.len() + 1)),
        ] {
            let mut changed = valid.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            let parsed: ManagedPairState = serde_json::from_value(changed).unwrap();
            assert!(
                validate_state_document(&parsed, &manifest, target, payload, envelope).is_err(),
                "state field {pointer} was not bound"
            );
        }
    }

    #[test]
    fn managed_pair_state_is_closed_and_has_no_legacy_receipt_shape() {
        let target = TargetSpec::current().unwrap();
        let mut unknown_root = state_value(target, b"manifest", b"envelope");
        unknown_root["unexpected"] = json!(true);
        assert!(serde_json::from_value::<ManagedPairState>(unknown_root).is_err());

        let mut unknown_identity = state_value(target, b"manifest", b"envelope");
        unknown_identity["identity"]["channel"] = json!("staging");
        assert!(serde_json::from_value::<ManagedPairState>(unknown_identity).is_err());

        let mut unknown_component = state_value(target, b"manifest", b"envelope");
        unknown_component["identity"]["core"]["artifact_name"] = json!("ctx-linux-x64");
        assert!(serde_json::from_value::<ManagedPairState>(unknown_component).is_err());

        let legacy = json!({
            "contract": "ctx-managed-pair-rollback-receipt",
            "schema_version": 1,
            "channel": "staging",
            "release_authority_key_id": "legacy",
            "rollback_generation": 7,
            "manifest_sha256": CORE_SHA,
        });
        assert!(serde_json::from_value::<ManagedPairState>(legacy).is_err());
    }

    #[test]
    fn signed_component_and_state_bounds_are_exactly_256_mib() {
        let target = TargetSpec::current().unwrap();
        let digest = parse_digest(CORE_SHA).unwrap();
        let component_at = |size_bytes| {
            serde_json::from_value::<ComponentDocument>(component(
                "core",
                target.core_artifact,
                target.core_slot,
                target.core_rust_target,
                CORE_SHA,
                size_bytes,
            ))
            .unwrap()
        };
        let identity_at = |size_bytes| test_file_identity(size_bytes, digest);

        let accepted = component_at(MAX_COMPONENT_BYTES);
        assert!(validate_component(
            &accepted,
            "core",
            target.core_artifact,
            target.core_slot,
            target.core_rust_target,
            &identity_at(MAX_COMPONENT_BYTES),
            None,
        )
        .is_ok());
        let accepted_state = ManagedPairComponentIdentityDocument {
            sha256: CORE_SHA.to_owned(),
            size_bytes: MAX_COMPONENT_BYTES,
        };
        assert!(state_component_matches(&accepted_state, &accepted).unwrap());

        let rejected = component_at(MAX_COMPONENT_BYTES + 1);
        assert!(validate_component(
            &rejected,
            "core",
            target.core_artifact,
            target.core_slot,
            target.core_rust_target,
            &identity_at(MAX_COMPONENT_BYTES + 1),
            None,
        )
        .is_err());
        let rejected_state = ManagedPairComponentIdentityDocument {
            sha256: CORE_SHA.to_owned(),
            size_bytes: MAX_COMPONENT_BYTES + 1,
        };
        assert!(!state_component_matches(&rejected_state, &rejected).unwrap());
    }

    #[cfg(unix)]
    fn test_file_identity(size_bytes: u64, sha256: Sha256Digest) -> crate::FileIdentity {
        crate::FileIdentity::unix(size_bytes, sha256, 1, 2, 3)
    }

    #[cfg(windows)]
    fn test_file_identity(size_bytes: u64, sha256: Sha256Digest) -> crate::FileIdentity {
        crate::FileIdentity::windows(size_bytes, sha256, 1, 2)
    }

    fn manifest(target: TargetSpec) -> Manifest {
        serde_json::from_value(json!({
            "contract": "ctx-managed-pair-manifest",
            "schema_version": 1,
            "channel": "staging",
            "release_authority_key_id": "ctx-pro-release-staging-test",
            "release_name": "v1.2.3",
            "target": {
                "id": target.id,
                "os": target.os,
                "arch": target.arch,
                "core_rust_target": target.core_rust_target,
                "companion_rust_target": target.companion_rust_target,
            },
            "install_geometry": {
                "install_root": "<install-root>",
                "managed_bin_dir": "<install-root>/bin",
                "core_slot": target.core_slot,
                "companion_slot": target.companion_slot,
            },
            "target_matrix_sha256": TARGET_MATRIX_SHA256,
            "rollback_generation": 7,
            "snapshot": {
                "contract": "ctx-managed-pair-snapshot-v1",
                "fingerprint": CORE_SHA,
            },
            "compatibility": {
                "invocation_fingerprint": CORE_SHA,
                "core_capability_fingerprint": CORE_SHA,
            },
            "components": {
                "core": component("core", target.core_artifact, target.core_slot, target.core_rust_target, CORE_SHA, 17),
                "companion": component("companion", target.companion_artifact, target.companion_slot, target.companion_rust_target, COMPANION_SHA, 29),
            },
        }))
        .unwrap()
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

    fn state_value(target: TargetSpec, payload: &[u8], envelope: &[u8]) -> Value {
        json!({
            "contract": "ctx-managed-pair-state",
            "schema_version": 1,
            "identity": {
                "release_name": "v1.2.3",
                "target": target.id,
                "rollback_generation": 7,
                "manifest_sha256": digest(payload).to_hex(),
                "core": {"sha256": CORE_SHA, "size_bytes": 17},
                "companion": {"sha256": COMPANION_SHA, "size_bytes": 29},
            },
            "envelope_sha256": digest(envelope).to_hex(),
            "envelope_size_bytes": envelope.len(),
        })
    }
}

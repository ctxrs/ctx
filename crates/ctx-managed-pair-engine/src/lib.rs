//! Neutral, verifier-driven publication of the fixed managed Core/companion pair.
//!
//! This crate owns only local fix-forward filesystem mechanics. It does not
//! acquire artifacts, interpret companion behavior, hold release credentials,
//! or make process/daemon policy.

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};

mod filesystem;
mod fix_forward;

pub use fix_forward::{
    apply_or_resume_managed_pair_under_installation_lock,
    cleanup_orphaned_managed_pair_candidate_under_installation_lock,
    inspect_managed_pair_under_installation_lock,
    managed_pair_evidence_present_under_installation_lock,
    resume_pending_managed_pair_under_installation_lock,
    stage_managed_pair_under_installation_lock, ManagedPairApplyInput, ManagedPairApplyOutcome,
    ManagedPairInstallationStatus, ManagedPairStageOutcome,
};

pub const MANAGED_PAIR_ENVELOPE_RELATIVE_PATH: &str = "share/ctx/managed-pair-envelope.json";
pub const MANAGED_PAIR_STATE_RELATIVE_PATH: &str = "share/ctx/managed-pair-state.json";
#[cfg(not(windows))]
pub const MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH: &str = "bin/ctx.install.json";
#[cfg(windows)]
pub const MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH: &str = "bin/ctx.exe.install.json";
#[cfg(not(windows))]
pub const MANAGED_PAIR_INSTALLATION_LOCK_RELATIVE_PATH: &str = "bin/.ctx.install.lock";
#[cfg(windows)]
pub const MANAGED_PAIR_INSTALLATION_LOCK_RELATIVE_PATH: &str = "bin/.ctx.exe.install.lock";
pub const MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH: &str =
    "bin/.ctx.upgrade-install-transaction.json";

const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_COMPONENT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENVELOPE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MARKER_BYTES: u64 = 64 * 1024;
const MAX_STATE_BYTES: u64 = 64 * 1024;

/// The fixed release target returned by a trusted signed-envelope verifier.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ManagedPairTarget {
    LinuxArm64,
    LinuxX64,
    MacosArm64,
    MacosX64,
    WindowsX64,
}

/// Exact bytes for one neutral managed-pair component.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedPairComponentIdentity {
    sha256: String,
    size_bytes: u64,
}

impl ManagedPairComponentIdentity {
    pub fn new(sha256: impl Into<String>, size_bytes: u64) -> Result<Self> {
        let identity = Self {
            sha256: sha256.into(),
            size_bytes,
        };
        identity.validate("managed-pair component")?;
        Ok(identity)
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    fn validate(&self, label: &str) -> Result<()> {
        validate_sha256(&self.sha256, label)?;
        if self.size_bytes == 0 || self.size_bytes > MAX_COMPONENT_BYTES {
            bail!("{label} size is outside the managed-pair bound");
        }
        Ok(())
    }
}

/// A trust-neutral identity returned only after a caller verifies the signed
/// envelope with its selected release authority.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedManagedPairIdentity {
    release_name: String,
    target: ManagedPairTarget,
    rollback_generation: u64,
    manifest_sha256: String,
    core: ManagedPairComponentIdentity,
    companion: ManagedPairComponentIdentity,
}

impl VerifiedManagedPairIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        release_name: impl Into<String>,
        target: ManagedPairTarget,
        rollback_generation: u64,
        manifest_sha256: impl Into<String>,
        core: ManagedPairComponentIdentity,
        companion: ManagedPairComponentIdentity,
    ) -> Result<Self> {
        let identity = Self {
            release_name: release_name.into(),
            target,
            rollback_generation,
            manifest_sha256: manifest_sha256.into(),
            core,
            companion,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn release_name(&self) -> &str {
        &self.release_name
    }

    pub fn target(&self) -> ManagedPairTarget {
        self.target
    }

    pub fn rollback_generation(&self) -> u64 {
        self.rollback_generation
    }

    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    pub fn core(&self) -> &ManagedPairComponentIdentity {
        &self.core
    }

    pub fn companion(&self) -> &ManagedPairComponentIdentity {
        &self.companion
    }

    fn validate(&self) -> Result<()> {
        if self.release_name.is_empty()
            || self.release_name.len() > 128
            || !self
                .release_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
            || !self.release_name.as_bytes()[0].is_ascii_alphanumeric()
        {
            bail!("managed-pair release name is invalid");
        }
        if self.rollback_generation == 0 || self.rollback_generation > 9_007_199_254_740_991 {
            bail!("managed-pair rollback generation is invalid");
        }
        validate_sha256(&self.manifest_sha256, "managed-pair manifest")?;
        self.core.validate("managed-pair Core component")?;
        self.companion.validate("managed-pair companion component")
    }
}

/// The only production trust input accepted by the managed-pair kernel.
///
/// Implementations authenticate the detached signature, validate the complete
/// neutral manifest contract, and return its exact target/component identity.
pub trait ManagedPairVerifier {
    fn verify_signed_envelope(&self, signed_envelope: &[u8])
        -> Result<VerifiedManagedPairIdentity>;
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ManagedPairState {
    contract: String,
    schema_version: u32,
    identity: VerifiedManagedPairIdentity,
    envelope_sha256: String,
    envelope_size_bytes: u64,
}

impl ManagedPairState {
    fn new(identity: VerifiedManagedPairIdentity, envelope: &filesystem::ObservedFile) -> Self {
        Self {
            contract: "ctx-managed-pair-state".to_owned(),
            schema_version: STATE_SCHEMA_VERSION,
            identity,
            envelope_sha256: envelope.stamp.sha256.clone(),
            envelope_size_bytes: envelope.stamp.size_bytes,
        }
    }

    fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn validate(&self) -> Result<()> {
        if self.contract != "ctx-managed-pair-state"
            || self.schema_version != STATE_SCHEMA_VERSION
            || self.envelope_size_bytes == 0
            || self.envelope_size_bytes > MAX_ENVELOPE_BYTES
        {
            bail!("managed-pair state contract is invalid");
        }
        self.identity.validate()?;
        validate_sha256(&self.envelope_sha256, "managed-pair envelope")
    }
}

fn validate_verified_identity(identity: &VerifiedManagedPairIdentity) -> Result<()> {
    identity.validate()?;
    if identity.target != current_target()? {
        bail!("managed-pair envelope target does not match this executable");
    }
    Ok(())
}

fn current_target() -> Result<ManagedPairTarget> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => Ok(ManagedPairTarget::LinuxArm64),
        ("linux", "x86_64") => Ok(ManagedPairTarget::LinuxX64),
        ("macos", "aarch64") => Ok(ManagedPairTarget::MacosArm64),
        ("macos", "x86_64") => Ok(ManagedPairTarget::MacosX64),
        ("windows", "x86_64") => Ok(ManagedPairTarget::WindowsX64),
        (os, arch) => bail!("managed-pair transactions are unsupported on {os}-{arch}"),
    }
}

fn validate_active(
    layout: &filesystem::Layout,
    verifier: &dyn ManagedPairVerifier,
) -> Result<(VerifiedManagedPairIdentity, String)> {
    use filesystem::Slot;

    layout.revalidate()?;
    let state_file = filesystem::read_regular(
        &layout.target(Slot::State),
        MAX_STATE_BYTES,
        Slot::State.label(),
    )?;
    let state: ManagedPairState =
        serde_json::from_slice(&state_file.bytes).context("parse managed-pair state")?;
    state.validate()?;
    validate_verified_identity(&state.identity)?;
    let envelope = filesystem::read_regular(
        &layout.target(Slot::Envelope),
        MAX_ENVELOPE_BYTES,
        Slot::Envelope.label(),
    )?;
    if envelope.stamp.sha256 != state.envelope_sha256
        || envelope.stamp.size_bytes != state.envelope_size_bytes
    {
        bail!("active managed-pair envelope does not match state");
    }
    let verified = verifier
        .verify_signed_envelope(&envelope.bytes)
        .context("reverify active managed-pair envelope")?;
    validate_verified_identity(&verified)?;
    if verified != state.identity {
        bail!("active managed-pair state does not match its signed envelope");
    }
    filesystem::verify_content(
        &layout.target(Slot::Core),
        verified.core(),
        Slot::Core.label(),
    )?;
    filesystem::verify_content(
        &layout.target(Slot::Companion),
        verified.companion(),
        Slot::Companion.label(),
    )?;
    filesystem::read_regular(
        &layout.target(Slot::Marker),
        MAX_MARKER_BYTES,
        Slot::Marker.label(),
    )?;
    layout.revalidate()?;
    Ok((verified, envelope.stamp.sha256))
}

fn max_bytes(slot: filesystem::Slot) -> u64 {
    match slot {
        filesystem::Slot::Core | filesystem::Slot::Companion => MAX_COMPONENT_BYTES,
        filesystem::Slot::Marker => MAX_MARKER_BYTES,
        filesystem::Slot::Envelope => MAX_ENVELOPE_BYTES,
        filesystem::Slot::State => MAX_STATE_BYTES,
    }
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} SHA-256 identity is invalid");
    }
    Ok(())
}

#[cfg(test)]
mod tests;

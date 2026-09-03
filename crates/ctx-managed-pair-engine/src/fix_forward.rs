use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::{
    filesystem::{self, Entry, FileStamp, Layout, ObservedFile, Slot},
    validate_verified_identity, ManagedPairState, ManagedPairVerifier, VerifiedManagedPairIdentity,
    MAX_COMPONENT_BYTES, MAX_ENVELOPE_BYTES, MAX_MARKER_BYTES, MAX_STATE_BYTES,
};

const PENDING_SCHEMA: &str = "ctx-managed-pair-apply-v1";
const MAX_PENDING_BYTES: u64 = 16 * 1024;

/// Already-downloaded inputs for one signed managed-pair candidate.
///
/// Every path must be absolute and identify an owner-safe, unique regular file.
/// The files may live outside `install_root`; the kernel copies them into its
/// same-filesystem attempt directory before publishing any fixed slot. Marker
/// semantics are verified by the caller; this kernel treats its bytes as opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedPairApplyInput {
    signed_envelope: PathBuf,
    core: PathBuf,
    companion: PathBuf,
    core_install_marker: PathBuf,
}

impl ManagedPairApplyInput {
    pub fn new(
        signed_envelope: impl Into<PathBuf>,
        core: impl Into<PathBuf>,
        companion: impl Into<PathBuf>,
        core_install_marker: impl Into<PathBuf>,
    ) -> Self {
        Self {
            signed_envelope: signed_envelope.into(),
            core: core.into(),
            companion: companion.into(),
            core_install_marker: core_install_marker.into(),
        }
    }

    pub fn signed_envelope(&self) -> &Path {
        &self.signed_envelope
    }

    pub fn core(&self) -> &Path {
        &self.core
    }

    pub fn companion(&self) -> &Path {
        &self.companion
    }

    pub fn core_install_marker(&self) -> &Path {
        &self.core_install_marker
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedPairApplyOutcome {
    AlreadyCurrent {
        identity: VerifiedManagedPairIdentity,
    },
    Applied {
        attempt_id: String,
        identity: VerifiedManagedPairIdentity,
    },
    Resumed {
        attempt_id: String,
        identity: VerifiedManagedPairIdentity,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedPairInstallationStatus {
    Absent,
    Healthy {
        identity: VerifiedManagedPairIdentity,
        envelope_sha256: String,
    },
    RepairRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedPairStageOutcome {
    AlreadyCurrent {
        identity: VerifiedManagedPairIdentity,
    },
    Staged {
        attempt_id: String,
        identity: VerifiedManagedPairIdentity,
        retained_core: PathBuf,
    },
}

impl ManagedPairApplyOutcome {
    pub fn identity(&self) -> &VerifiedManagedPairIdentity {
        match self {
            Self::AlreadyCurrent { identity }
            | Self::Applied { identity, .. }
            | Self::Resumed { identity, .. } => identity,
        }
    }

    pub fn attempt_id(&self) -> Option<&str> {
        match self {
            Self::AlreadyCurrent { .. } => None,
            Self::Applied { attempt_id, .. } | Self::Resumed { attempt_id, .. } => Some(attempt_id),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ContentIdentity {
    sha256: String,
    size_bytes: u64,
}

impl ContentIdentity {
    fn from_observed(observed: &ObservedFile) -> Self {
        Self {
            sha256: observed.stamp.sha256.clone(),
            size_bytes: observed.stamp.size_bytes,
        }
    }

    fn from_component(component: &super::ManagedPairComponentIdentity) -> Self {
        Self {
            sha256: component.sha256().to_owned(),
            size_bytes: component.size_bytes(),
        }
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            sha256: format!("{:x}", Sha256::digest(bytes)),
            size_bytes: u64::try_from(bytes.len())?,
        })
    }

    fn matches(&self, stamp: &FileStamp) -> bool {
        self.size_bytes == stamp.size_bytes && self.sha256 == stamp.sha256
    }

    fn validate(&self, max_bytes: u64, label: &str) -> Result<()> {
        super::validate_sha256(&self.sha256, label)?;
        if self.size_bytes == 0 || self.size_bytes > max_bytes {
            bail!("{label} size is outside its bound");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PendingApply {
    schema: String,
    attempt_id: String,
    candidate_envelope_identity: ContentIdentity,
    candidate_marker_identity: ContentIdentity,
}

impl PendingApply {
    fn validate(&self) -> Result<()> {
        if self.schema != PENDING_SCHEMA || !valid_attempt_id(&self.attempt_id) {
            bail!("managed-pair active transaction identity is invalid");
        }
        self.candidate_envelope_identity
            .validate(MAX_ENVELOPE_BYTES, "managed-pair pending envelope")?;
        self.candidate_marker_identity
            .validate(MAX_MARKER_BYTES, "managed-pair pending marker")
    }
}

/// Applies a verified pair or resumes the installation's retained pending pair.
///
/// The explicit installation root is the sole authority for every destination
/// and coordination path. If a pending attempt exists, `input` is not consulted:
/// recovery re-verifies and publishes only the retained same-filesystem copy.
/// The caller must hold the live exclusive OS lock at
/// `<install_root>/bin/.ctx.install.lock` for the complete call. The kernel does
/// not acquire or reacquire that lock.
pub fn apply_or_resume_managed_pair_under_installation_lock(
    install_root: &Path,
    input: &ManagedPairApplyInput,
    verifier: &dyn ManagedPairVerifier,
) -> Result<ManagedPairApplyOutcome> {
    apply_or_resume_with_fault(install_root, input, verifier, &|_| {})
}

/// Classifies the fixed pair image without exposing its filesystem DTOs.
/// Missing pair-only files are repairable; unsafe path identities remain hard
/// errors. The caller must hold the canonical installation lock.
pub fn inspect_managed_pair_under_installation_lock(
    install_root: &Path,
    verifier: &dyn ManagedPairVerifier,
) -> Result<ManagedPairInstallationStatus> {
    filesystem::validate_absolute_root(install_root, "managed-pair install root")?;
    let layout = Layout::open(install_root, true)?;
    if !managed_pair_evidence_present(&layout)? {
        return Ok(ManagedPairInstallationStatus::Absent);
    }
    for slot in Slot::ALL {
        if filesystem::stamp_optional(&layout.target(slot), super::max_bytes(slot), slot.label())?
            .is_none()
        {
            return Ok(ManagedPairInstallationStatus::RepairRequired);
        }
    }
    match super::validate_active(&layout, verifier) {
        Ok((identity, envelope_sha256)) => Ok(ManagedPairInstallationStatus::Healthy {
            identity,
            envelope_sha256,
        }),
        Err(_) => Ok(ManagedPairInstallationStatus::RepairRequired),
    }
}

/// Removes an apply candidate left before its pending record was published.
/// A published managed-pair transaction retains ownership of its candidate.
/// The caller must hold the canonical installation lock.
pub fn cleanup_orphaned_managed_pair_candidate_under_installation_lock(
    install_root: &Path,
) -> Result<()> {
    filesystem::validate_absolute_root(install_root, "managed-pair install root")?;
    let candidate_root = filesystem::apply_candidate_root(install_root);
    match candidate_root.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("inspect managed-pair apply candidate"),
    }
    let layout = Layout::open(install_root, false)?;
    if read_pending(&layout)?.is_none() {
        cleanup_pre_pending_attempt(&layout)?;
        layout.revalidate()?;
    }
    Ok(())
}

/// Retains a verified candidate and publishes the minimal pending record, but
/// does not replace any fixed slot. Windows callers use the returned retained
/// Core as their post-exit continuation. The caller must hold the canonical
/// installation lock.
pub fn stage_managed_pair_under_installation_lock(
    install_root: &Path,
    input: &ManagedPairApplyInput,
    verifier: &dyn ManagedPairVerifier,
) -> Result<ManagedPairStageOutcome> {
    stage_with_fault(install_root, input, verifier, &|_| {}).map(|(outcome, _)| outcome)
}

/// Resumes only the managed-pair pending schema. `None` leaves a generic Core
/// upgrade transaction for its existing parser. The caller must hold the
/// canonical installation lock.
pub fn resume_pending_managed_pair_under_installation_lock(
    install_root: &Path,
    verifier: &dyn ManagedPairVerifier,
) -> Result<Option<ManagedPairApplyOutcome>> {
    resume_with_fault(install_root, verifier, true, &|_| {})
}

pub(super) fn apply_or_resume_with_fault(
    install_root: &Path,
    input: &ManagedPairApplyInput,
    verifier: &dyn ManagedPairVerifier,
    fault: &dyn Fn(&str),
) -> Result<ManagedPairApplyOutcome> {
    let (staged, resumed) = stage_with_fault(install_root, input, verifier, fault)?;
    if let ManagedPairStageOutcome::AlreadyCurrent { identity } = staged {
        return Ok(ManagedPairApplyOutcome::AlreadyCurrent { identity });
    }
    resume_with_fault(install_root, verifier, resumed, fault)?
        .ok_or_else(|| anyhow!("managed-pair pending transaction disappeared before publication"))
}

fn stage_with_fault(
    install_root: &Path,
    input: &ManagedPairApplyInput,
    verifier: &dyn ManagedPairVerifier,
    fault: &dyn Fn(&str),
) -> Result<(ManagedPairStageOutcome, bool)> {
    filesystem::validate_absolute_root(install_root, "managed-pair install root")?;
    let layout = Layout::open(install_root, true)?;
    if let Some(pending) = read_pending(&layout)? {
        let retained = verify_retained_candidate(&layout, &pending, verifier)?;
        return Ok((
            ManagedPairStageOutcome::Staged {
                attempt_id: pending.attempt_id,
                identity: retained.identity,
                retained_core: retained_core_path(&layout),
            },
            true,
        ));
    }

    cleanup_pre_pending_attempt(&layout)?;
    let verified = verify_input(input, verifier)?;
    if candidate_is_current(
        &layout,
        verifier,
        &verified.identity,
        &verified.envelope_identity,
        &verified.marker_identity,
    )? {
        return Ok((
            ManagedPairStageOutcome::AlreadyCurrent {
                identity: verified.identity,
            },
            false,
        ));
    }
    let pending = stage_candidate(&layout, &verified)?;
    if let Err(error) = write_pending(&layout, &pending) {
        if read_pending(&layout)?.is_none() {
            cleanup_pre_pending_attempt(&layout)?;
        }
        return Err(error);
    }
    fault("pending");
    Ok((
        ManagedPairStageOutcome::Staged {
            attempt_id: pending.attempt_id,
            identity: verified.identity,
            retained_core: retained_core_path(&layout),
        },
        false,
    ))
}

fn resume_with_fault(
    install_root: &Path,
    verifier: &dyn ManagedPairVerifier,
    resumed: bool,
    fault: &dyn Fn(&str),
) -> Result<Option<ManagedPairApplyOutcome>> {
    filesystem::validate_absolute_root(install_root, "managed-pair install root")?;
    let layout = Layout::open(install_root, true)?;
    let Some(pending) = read_pending(&layout)? else {
        return Ok(None);
    };
    let retained = verify_retained_candidate(&layout, &pending, verifier)?;
    let _ = candidate_is_current(
        &layout,
        verifier,
        &retained.identity,
        &pending.candidate_envelope_identity,
        &retained.marker_identity,
    )?;
    publish_candidate(&layout, &pending, &retained, fault)?;
    let (active, _) = super::validate_active(&layout, verifier)?;
    if active != retained.identity {
        bail!("published managed pair does not match the retained signed candidate");
    }
    remove_pending(&layout, &pending)?;
    filesystem::remove_apply_candidate(&layout)?;
    layout.revalidate()?;

    let outcome = if resumed {
        ManagedPairApplyOutcome::Resumed {
            attempt_id: pending.attempt_id,
            identity: retained.identity,
        }
    } else {
        ManagedPairApplyOutcome::Applied {
            attempt_id: pending.attempt_id,
            identity: retained.identity,
        }
    };
    Ok(Some(outcome))
}

fn retained_core_path(layout: &Layout) -> PathBuf {
    filesystem::apply_candidate_root(layout.root()).join(if cfg!(windows) {
        "bin/ctx.exe"
    } else {
        "bin/ctx"
    })
}

struct VerifiedInput {
    envelope: ObservedFile,
    envelope_identity: ContentIdentity,
    identity: VerifiedManagedPairIdentity,
    core: Entry,
    companion: Entry,
    marker: Entry,
    marker_stamp: FileStamp,
    marker_identity: ContentIdentity,
}

fn verify_input(
    input: &ManagedPairApplyInput,
    verifier: &dyn ManagedPairVerifier,
) -> Result<VerifiedInput> {
    let envelope_entry = filesystem::external_entry(
        input.signed_envelope(),
        "managed-pair signed envelope input",
    )?;
    let envelope = filesystem::read_regular(
        &envelope_entry,
        MAX_ENVELOPE_BYTES,
        "managed-pair signed envelope input",
    )?;
    let identity = verifier
        .verify_signed_envelope(&envelope.bytes)
        .context("verify managed-pair signed envelope")?;
    validate_verified_identity(&identity)?;
    let core = filesystem::external_entry(input.core(), "managed-pair Core input")?;
    let companion = filesystem::external_entry(input.companion(), "managed-pair companion input")?;
    filesystem::verify_content(&core, identity.core(), "managed-pair Core input")?;
    filesystem::verify_content(
        &companion,
        identity.companion(),
        "managed-pair companion input",
    )?;
    let marker = filesystem::external_entry(
        input.core_install_marker(),
        "managed Core install marker input",
    )?;
    let observed_marker = filesystem::read_regular(
        &marker,
        MAX_MARKER_BYTES,
        "managed Core install marker input",
    )?;
    let marker_stamp = observed_marker.stamp.clone();
    Ok(VerifiedInput {
        envelope_identity: ContentIdentity::from_observed(&envelope),
        envelope,
        identity,
        core,
        companion,
        marker,
        marker_stamp,
        marker_identity: ContentIdentity::from_observed(&observed_marker),
    })
}

fn stage_candidate(layout: &Layout, input: &VerifiedInput) -> Result<PendingApply> {
    let result = (|| {
        let root = filesystem::create_apply_candidate(layout.root())?;
        let candidate = Layout::open_candidate(&root)?;
        layout.revalidate()?;
        if layout.root_binding()?.0 != candidate.root_binding()?.0 {
            bail!("managed-pair attempt directory is not on the installation filesystem");
        }

        filesystem::write_new(
            &candidate.target(Slot::Envelope),
            &input.envelope.bytes,
            false,
            Slot::Envelope.label(),
        )?;
        filesystem::copy_verified(
            &input.companion,
            &candidate.target(Slot::Companion),
            input.identity.companion(),
            true,
            Slot::Companion.label(),
        )?;
        filesystem::copy_exact(
            &input.marker,
            &candidate.target(Slot::Marker),
            &input.marker_stamp,
            MAX_MARKER_BYTES,
            false,
            Slot::Marker.label(),
        )?;
        filesystem::copy_verified(
            &input.core,
            &candidate.target(Slot::Core),
            input.identity.core(),
            true,
            Slot::Core.label(),
        )?;
        candidate.revalidate()?;
        Ok(PendingApply {
            schema: PENDING_SCHEMA.to_owned(),
            attempt_id: Uuid::new_v4().simple().to_string(),
            candidate_envelope_identity: input.envelope_identity.clone(),
            candidate_marker_identity: input.marker_identity.clone(),
        })
    })();
    if result.is_err() && filesystem::apply_candidate_exists(layout)? {
        filesystem::remove_apply_candidate(layout)?;
    }
    result
}

struct RetainedCandidate {
    envelope: ObservedFile,
    identity: VerifiedManagedPairIdentity,
    marker_identity: ContentIdentity,
}

fn verify_retained_candidate(
    layout: &Layout,
    pending: &PendingApply,
    verifier: &dyn ManagedPairVerifier,
) -> Result<RetainedCandidate> {
    pending.validate()?;
    let candidate = layout.open_apply_candidate()?;
    if layout.root_binding()?.0 != candidate.root_binding()?.0 {
        bail!("managed-pair attempt directory is not on the installation filesystem");
    }
    let envelope = filesystem::read_regular(
        &candidate.target(Slot::Envelope),
        MAX_ENVELOPE_BYTES,
        Slot::Envelope.label(),
    )?;
    if !pending.candidate_envelope_identity.matches(&envelope.stamp) {
        bail!("managed-pair retained candidate envelope changed");
    }
    let identity = verifier
        .verify_signed_envelope(&envelope.bytes)
        .context("reverify retained managed-pair envelope")?;
    validate_verified_identity(&identity)?;
    filesystem::verify_content(
        &candidate.target(Slot::Companion),
        identity.companion(),
        Slot::Companion.label(),
    )?;
    filesystem::verify_content(
        &candidate.target(Slot::Core),
        identity.core(),
        Slot::Core.label(),
    )?;
    let marker = filesystem::read_regular(
        &candidate.target(Slot::Marker),
        MAX_MARKER_BYTES,
        Slot::Marker.label(),
    )?;
    if !pending.candidate_marker_identity.matches(&marker.stamp) {
        bail!("managed-pair retained Core install marker changed");
    }
    candidate.revalidate()?;
    Ok(RetainedCandidate {
        envelope,
        identity,
        marker_identity: pending.candidate_marker_identity.clone(),
    })
}

fn candidate_is_current(
    layout: &Layout,
    verifier: &dyn ManagedPairVerifier,
    candidate: &VerifiedManagedPairIdentity,
    candidate_envelope: &ContentIdentity,
    candidate_marker: &ContentIdentity,
) -> Result<bool> {
    layout.revalidate()?;
    let state = read_valid_state(layout)?;
    let installed_envelope = read_verified_envelope(layout, verifier)?;
    if state.is_none() && installed_envelope.is_none() && managed_pair_evidence_present(layout)? {
        bail!("managed-pair installation has no valid rollback-generation witness");
    }

    if let Some(state) = &state {
        enforce_rollback(
            candidate,
            candidate_envelope,
            &state.identity,
            Some(&ContentIdentity {
                sha256: state.envelope_sha256.clone(),
                size_bytes: state.envelope_size_bytes,
            }),
        )?;
    }
    if let Some((identity, envelope)) = &installed_envelope {
        enforce_rollback(candidate, candidate_envelope, identity, Some(envelope))?;
    }

    let state_matches = state.as_ref().is_some_and(|state| {
        state.identity == *candidate
            && state.envelope_sha256 == candidate_envelope.sha256
            && state.envelope_size_bytes == candidate_envelope.size_bytes
    });
    let envelope_matches = installed_envelope
        .as_ref()
        .is_some_and(|(identity, envelope)| {
            identity == candidate && envelope == candidate_envelope
        });
    let companion_matches = component_matches(
        &layout.target(Slot::Companion),
        candidate.companion(),
        Slot::Companion.label(),
    )?;
    let marker_matches = content_matches(
        &layout.target(Slot::Marker),
        candidate_marker,
        MAX_MARKER_BYTES,
        Slot::Marker.label(),
    )?;
    let core_matches = component_matches(
        &layout.target(Slot::Core),
        candidate.core(),
        Slot::Core.label(),
    )?;
    Ok(state_matches && envelope_matches && companion_matches && marker_matches && core_matches)
}

fn managed_pair_evidence_present(layout: &Layout) -> Result<bool> {
    for slot in [Slot::Envelope, Slot::Companion, Slot::State] {
        if filesystem::stamp_optional(&layout.target(slot), super::max_bytes(slot), slot.label())?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_valid_state(layout: &Layout) -> Result<Option<ManagedPairState>> {
    let Some(file) = read_optional(
        &layout.target(Slot::State),
        MAX_STATE_BYTES,
        Slot::State.label(),
    )?
    else {
        return Ok(None);
    };
    let Ok(state) = serde_json::from_slice::<ManagedPairState>(&file.bytes) else {
        return Ok(None);
    };
    if state.validate().is_err() || validate_verified_identity(&state.identity).is_err() {
        return Ok(None);
    }
    Ok(Some(state))
}

fn read_verified_envelope(
    layout: &Layout,
    verifier: &dyn ManagedPairVerifier,
) -> Result<Option<(VerifiedManagedPairIdentity, ContentIdentity)>> {
    let Some(envelope) = read_optional(
        &layout.target(Slot::Envelope),
        MAX_ENVELOPE_BYTES,
        Slot::Envelope.label(),
    )?
    else {
        return Ok(None);
    };
    let Ok(identity) = verifier.verify_signed_envelope(&envelope.bytes) else {
        return Ok(None);
    };
    if validate_verified_identity(&identity).is_err() {
        return Ok(None);
    }
    Ok(Some((identity, ContentIdentity::from_observed(&envelope))))
}

fn enforce_rollback(
    candidate: &VerifiedManagedPairIdentity,
    candidate_envelope: &ContentIdentity,
    installed: &VerifiedManagedPairIdentity,
    installed_envelope: Option<&ContentIdentity>,
) -> Result<()> {
    if candidate.rollback_generation() < installed.rollback_generation() {
        bail!("managed-pair rollback generation would downgrade the installation");
    }
    if candidate.rollback_generation() == installed.rollback_generation()
        && (candidate != installed
            || installed_envelope.is_some_and(|envelope| envelope != candidate_envelope))
    {
        bail!("managed-pair signed identity changed without advancing rollback generation");
    }
    Ok(())
}

fn publish_candidate(
    layout: &Layout,
    pending: &PendingApply,
    retained: &RetainedCandidate,
    fault: &dyn Fn(&str),
) -> Result<()> {
    let candidate = layout.open_apply_candidate()?;
    for (slot, expected, fault_name) in [
        (
            Slot::Envelope,
            pending.candidate_envelope_identity.clone(),
            "publish_envelope",
        ),
        (
            Slot::Companion,
            ContentIdentity::from_component(retained.identity.companion()),
            "publish_companion",
        ),
        (
            Slot::Marker,
            retained.marker_identity.clone(),
            "publish_marker",
        ),
        (
            Slot::Core,
            ContentIdentity::from_component(retained.identity.core()),
            "publish_core",
        ),
    ] {
        publish_slot(layout, &candidate, pending, slot, &expected)?;
        fault(fault_name);
    }
    let state = ManagedPairState::new(retained.identity.clone(), &retained.envelope).to_bytes()?;
    publish_derived_state(layout, pending, &state)?;
    fault("publish_state");
    Ok(())
}

fn publish_slot(
    layout: &Layout,
    candidate: &Layout,
    pending: &PendingApply,
    slot: Slot,
    expected: &ContentIdentity,
) -> Result<()> {
    layout.revalidate()?;
    candidate.revalidate()?;
    let max = super::max_bytes(slot);
    let candidate_entry = candidate.target(slot);
    let candidate_stamp = filesystem::stamp_optional(&candidate_entry, max, slot.label())?
        .ok_or_else(|| anyhow!("retained managed-pair {} is absent", slot.label()))?;
    if !expected.matches(&candidate_stamp) {
        bail!("retained managed-pair {} changed", slot.label());
    }
    publish_exact(
        layout,
        pending,
        slot,
        expected,
        Some((&candidate_entry, &candidate_stamp)),
        None,
    )
}

fn publish_derived_state(layout: &Layout, pending: &PendingApply, bytes: &[u8]) -> Result<()> {
    let expected = ContentIdentity::from_bytes(bytes)?;
    publish_exact(layout, pending, Slot::State, &expected, None, Some(bytes))
}

fn publish_exact(
    layout: &Layout,
    pending: &PendingApply,
    slot: Slot,
    expected: &ContentIdentity,
    source: Option<(&Entry, &FileStamp)>,
    derived_bytes: Option<&[u8]>,
) -> Result<()> {
    let executable = matches!(slot, Slot::Core | Slot::Companion);
    let max = super::max_bytes(slot);
    let target = layout.target(slot);
    let temporary = layout.staged(slot, &pending.attempt_id);
    if content_matches(&target, expected, max, slot.label())? {
        filesystem::protect_regular(&target, executable, slot.label())?;
        remove_matching_temporary(&temporary, expected, max, slot.label())?;
        return Ok(());
    }

    let temporary_stamp = match filesystem::stamp_optional(
        &temporary,
        max,
        &format!("{} publication temporary", slot.label()),
    )? {
        Some(stamp) => {
            if !expected.matches(&stamp) {
                bail!(
                    "managed-pair {} publication temporary changed",
                    slot.label()
                );
            }
            filesystem::protect_regular(&temporary, executable, slot.label())?;
            stamp
        }
        None => match (source, derived_bytes) {
            (Some((source, source_stamp)), None) => filesystem::copy_exact(
                source,
                &temporary,
                source_stamp,
                max,
                executable,
                slot.label(),
            )?,
            (None, Some(bytes)) => {
                filesystem::write_new(&temporary, bytes, executable, slot.label())?
            }
            _ => bail!("managed-pair publication source is invalid"),
        },
    };
    // This rejects symlink, reparse-point, and hardlink targets immediately
    // before the handle-relative atomic replacement.
    let _ = filesystem::stamp_optional(&target, max, slot.label())?;
    filesystem::durable_replace(&temporary, &target, &temporary_stamp, max, slot.label())?;
    filesystem::protect_regular(&target, executable, slot.label())?;
    if !content_matches(&target, expected, max, slot.label())? {
        bail!("published managed-pair {} changed", slot.label());
    }
    layout.revalidate()
}

fn remove_matching_temporary(
    temporary: &Entry,
    expected: &ContentIdentity,
    max: u64,
    label: &str,
) -> Result<()> {
    let Some(stamp) = filesystem::stamp_optional(temporary, max, label)? else {
        return Ok(());
    };
    if !expected.matches(&stamp) {
        bail!("managed-pair {label} publication temporary changed");
    }
    filesystem::remove_if_exact(temporary, &stamp, max, label)
}

fn write_pending(layout: &Layout, pending: &PendingApply) -> Result<()> {
    pending.validate()?;
    filesystem::require_absent(
        &layout.active_transaction(),
        "ctx active installation transaction",
    )?;
    filesystem::require_absent(
        &layout.active_transaction_temporary(),
        "ctx active installation transaction temporary",
    )?;
    let mut bytes = serde_json::to_vec_pretty(pending)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_PENDING_BYTES {
        bail!("managed-pair active transaction exceeds its bound");
    }
    let temporary = layout.active_transaction_temporary();
    let stamp = filesystem::write_new(
        &temporary,
        &bytes,
        false,
        "ctx active installation transaction temporary",
    )?;
    filesystem::durable_replace(
        &temporary,
        &layout.active_transaction(),
        &stamp,
        MAX_PENDING_BYTES,
        "ctx active installation transaction",
    )?;
    if read_pending(layout)?.as_ref() != Some(pending) {
        bail!("managed-pair active transaction changed while being published");
    }
    Ok(())
}

fn read_pending(layout: &Layout) -> Result<Option<PendingApply>> {
    let active = read_optional(
        &layout.active_transaction(),
        MAX_PENDING_BYTES,
        "ctx active installation transaction",
    )?;
    let temporary = filesystem::read_temporary(
        &layout.active_transaction_temporary(),
        MAX_PENDING_BYTES,
        "ctx active installation transaction temporary",
    )?;
    if let Some(active) = active.as_ref() {
        if !has_pair_pending_schema(&active.bytes) {
            return Ok(None);
        }
        if temporary.is_some() {
            bail!("ctx active installation transaction has an unexpected temporary");
        }
    } else if let Some(temporary) = temporary {
        if has_pair_pending_schema(&temporary.bytes) {
            filesystem::remove_temporary_exact(
                &layout.active_transaction_temporary(),
                &temporary.stamp,
                MAX_PENDING_BYTES,
                "incomplete ctx active installation transaction temporary",
            )?;
        }
    }
    let Some(active) = active else {
        return Ok(None);
    };
    let pending: PendingApply = serde_json::from_slice(&active.bytes)
        .context("parse ctx active installation transaction")?;
    pending.validate()?;
    Ok(Some(pending))
}

fn has_pair_pending_schema(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| value.get("schema")?.as_str().map(str::to_owned))
        .as_deref()
        == Some(PENDING_SCHEMA)
}

fn remove_pending(layout: &Layout, expected: &PendingApply) -> Result<()> {
    if read_pending(layout)?.as_ref() != Some(expected) {
        bail!("refusing to remove a replaced ctx active installation transaction");
    }
    let entry = layout.active_transaction();
    let stamp = filesystem::stamp_optional(
        &entry,
        MAX_PENDING_BYTES,
        "ctx active installation transaction",
    )?
    .ok_or_else(|| anyhow!("ctx active installation transaction disappeared"))?;
    filesystem::remove_if_exact(
        &entry,
        &stamp,
        MAX_PENDING_BYTES,
        "ctx active installation transaction",
    )
}

fn cleanup_pre_pending_attempt(layout: &Layout) -> Result<()> {
    if filesystem::apply_candidate_exists(layout)? {
        filesystem::remove_apply_candidate(layout)?;
    }
    Ok(())
}

fn component_matches(
    entry: &Entry,
    expected: &super::ManagedPairComponentIdentity,
    label: &str,
) -> Result<bool> {
    content_matches(
        entry,
        &ContentIdentity::from_component(expected),
        MAX_COMPONENT_BYTES,
        label,
    )
}

fn content_matches(
    entry: &Entry,
    expected: &ContentIdentity,
    max: u64,
    label: &str,
) -> Result<bool> {
    Ok(filesystem::stamp_optional(entry, max, label)?
        .as_ref()
        .is_some_and(|actual| expected.matches(actual)))
}

fn read_optional(entry: &Entry, max: u64, label: &str) -> Result<Option<ObservedFile>> {
    match filesystem::read_regular(entry, max, label) {
        Ok(observed) => Ok(Some(observed)),
        Err(error) if is_not_found(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn valid_attempt_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

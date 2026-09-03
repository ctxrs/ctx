use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_managed_pair_engine::{
    cleanup_orphaned_managed_pair_candidate_under_installation_lock,
    inspect_managed_pair_under_installation_lock,
    managed_pair_evidence_present_under_installation_lock, ManagedPairInstallationStatus,
    ManagedPairVerifier, MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH,
    MANAGED_PAIR_ENVELOPE_RELATIVE_PATH, MANAGED_PAIR_STATE_RELATIVE_PATH,
};
use serde_json::Value;

use super::{
    is_normalized_sha256, journal_path, path_entry_exists, read_journal, remove_durable,
    sha256_path, validate_journal, verify_file_digest, Journal, TransactionKind, MAX_BINARY_BYTES,
};
use crate::{
    install_marker::is_staging_dogfood_marker, upgrade::managed_pair::ReleaseManagedPairVerifier,
};

const MAX_PAIR_ENVELOPE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PAIR_STATE_BYTES: u64 = 64 * 1024;

/// Fails closed when the shared installation lock protects a pending hosted
/// install or uninstall transaction. Installation publication must not race a
/// post-exit uninstall after that helper releases the lock while waiting.
pub fn ensure_hosted_transaction_inactive_under_installation_lock(
    install_path: &Path,
) -> Result<()> {
    let Some(journal) = read_journal(&journal_path(install_path))? else {
        return Ok(());
    };
    validate_journal(&journal, install_path, journal.kind)?;
    bail!("finish the pending hosted installation transaction before changing the installation")
}

pub(super) fn reject_managed_pair_material_for_core_only_install(
    install_path: &Path,
) -> Result<()> {
    let Some((root, _, _, _)) = managed_pair_paths(install_path) else {
        return Ok(());
    };
    if path_entry_exists(&root.join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH))?
        || managed_pair_evidence_present_under_installation_lock(&root)?
    {
        bail!("Core-only hosted installation cannot replace a managed Core+Pro pair");
    }
    Ok(())
}

pub(super) fn snapshot_managed_pair_files(
    journal: &mut Journal,
    supplied_verifier: Option<&dyn ManagedPairVerifier>,
) -> Result<()> {
    let Some((install_root, state_path, envelope_path, companion_path)) =
        managed_pair_paths(&journal.install_path)
    else {
        return Ok(());
    };
    cleanup_orphaned_managed_pair_candidate_under_installation_lock(&install_root)?;
    if path_entry_exists(&install_root.join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH))? {
        bail!("finish the pending managed-pair upgrade before uninstalling")
    }
    if ![
        state_path.as_path(),
        envelope_path.as_path(),
        companion_path.as_path(),
    ]
    .into_iter()
    .try_fold(false, |present, path| {
        Ok::<_, anyhow::Error>(present || path_entry_exists(path)?)
    })? {
        return Ok(());
    }

    let snapshot = |verifier: &dyn ManagedPairVerifier| -> Result<(String, String, String)> {
        let identity = match inspect_managed_pair_under_installation_lock(&install_root, verifier)?
        {
            ManagedPairInstallationStatus::Healthy { identity, .. } => identity,
            ManagedPairInstallationStatus::Absent => {
                bail!("managed-pair material disappeared during hosted uninstall preparation")
            }
            ManagedPairInstallationStatus::RepairRequired => {
                bail!("refusing to uninstall a substituted or incomplete managed pair")
            }
        };
        if identity.core().sha256() != journal.binary_sha256 {
            bail!("authenticated managed-pair Core does not match the hosted executable")
        }
        let state = sha256_path(
            &state_path,
            MAX_PAIR_STATE_BYTES,
            "managed-pair state marker",
        )?;
        let envelope = sha256_path(
            &envelope_path,
            MAX_PAIR_ENVELOPE_BYTES,
            "managed-pair signed envelope",
        )?;
        let companion = sha256_path(&companion_path, MAX_BINARY_BYTES, "managed-pair companion")?;
        if companion != identity.companion().sha256() {
            bail!("authenticated managed-pair companion changed during uninstall preparation")
        }
        match inspect_managed_pair_under_installation_lock(&install_root, verifier)? {
            ManagedPairInstallationStatus::Healthy {
                identity: revalidated,
                ..
            } if revalidated == identity => {}
            _ => bail!("managed-pair identity changed during hosted uninstall preparation"),
        }
        verify_file_digest(
            &state_path,
            &state,
            MAX_PAIR_STATE_BYTES,
            "managed-pair state marker",
        )?;
        verify_file_digest(
            &envelope_path,
            &envelope,
            MAX_PAIR_ENVELOPE_BYTES,
            "managed-pair signed envelope",
        )?;
        verify_file_digest(
            &companion_path,
            &companion,
            MAX_BINARY_BYTES,
            "managed-pair companion",
        )?;
        Ok((state, envelope, companion))
    };

    let (state, envelope, companion) = if let Some(verifier) = supplied_verifier {
        snapshot(verifier)?
    } else {
        let marker: Value = serde_json::from_str(&journal.marker_body)?;
        let channel = if is_staging_dogfood_marker(&marker) {
            "staging"
        } else {
            marker
                .get("channel")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("managed marker has no release channel"))?
        };
        snapshot(&ReleaseManagedPairVerifier::for_channel(channel)?)?
    };
    journal.managed_pair_state_sha256 = Some(state);
    journal.managed_pair_envelope_sha256 = Some(envelope);
    journal.managed_pair_companion_sha256 = Some(companion);
    Ok(())
}

pub(super) fn verify_recorded_pair_files(journal: &Journal, require_present: bool) -> Result<()> {
    let Some((_root, state_path, envelope_path, companion_path)) =
        managed_pair_paths(&journal.install_path)
    else {
        if journal.managed_pair_state_sha256.is_some() {
            bail!("hosted uninstall journal has invalid managed-pair geometry");
        }
        return Ok(());
    };
    for (path, digest, max, label) in [
        (
            state_path.as_path(),
            journal.managed_pair_state_sha256.as_deref(),
            MAX_PAIR_STATE_BYTES,
            "managed-pair state marker",
        ),
        (
            envelope_path.as_path(),
            journal.managed_pair_envelope_sha256.as_deref(),
            MAX_PAIR_ENVELOPE_BYTES,
            "managed-pair signed envelope",
        ),
        (
            companion_path.as_path(),
            journal.managed_pair_companion_sha256.as_deref(),
            MAX_BINARY_BYTES,
            "managed-pair companion",
        ),
    ] {
        let Some(digest) = digest else { continue };
        if path_entry_exists(path)? {
            verify_file_digest(path, digest, max, label)
                .with_context(|| format!("refusing substituted {label} during hosted uninstall"))?;
        } else if require_present {
            bail!("{label} is absent before hosted uninstall is armed");
        }
    }
    Ok(())
}

pub(super) fn remove_recorded_pair_files(
    journal: &Journal,
    fault: &mut dyn FnMut(&'static str) -> Result<()>,
) -> Result<()> {
    let Some((_root, state_path, envelope_path, companion_path)) =
        managed_pair_paths(&journal.install_path)
    else {
        return Ok(());
    };
    for (path, digest, max, label, removing, removed) in [
        (
            state_path.as_path(),
            journal.managed_pair_state_sha256.as_deref(),
            MAX_PAIR_STATE_BYTES,
            "managed-pair state marker",
            "removing_pair_state",
            "pair_state_removed",
        ),
        (
            envelope_path.as_path(),
            journal.managed_pair_envelope_sha256.as_deref(),
            MAX_PAIR_ENVELOPE_BYTES,
            "managed-pair signed envelope",
            "removing_pair_envelope",
            "pair_envelope_removed",
        ),
        (
            companion_path.as_path(),
            journal.managed_pair_companion_sha256.as_deref(),
            MAX_BINARY_BYTES,
            "managed-pair companion",
            "removing_pair_companion",
            "pair_companion_removed",
        ),
    ] {
        let Some(digest) = digest else { continue };
        if path_entry_exists(path)? {
            verify_file_digest(path, digest, max, label)
                .with_context(|| format!("refusing substituted {label} during hosted uninstall"))?;
            fault(removing)?;
            remove_durable(path)?;
            fault(removed)?;
        }
    }
    Ok(())
}

pub(super) fn journal_fields_are_valid(
    journal: &Journal,
    install_path: &Path,
    kind: TransactionKind,
) -> bool {
    let digests = [
        journal.managed_pair_state_sha256.as_deref(),
        journal.managed_pair_envelope_sha256.as_deref(),
        journal.managed_pair_companion_sha256.as_deref(),
    ];
    let count = digests.iter().filter(|digest| digest.is_some()).count();
    matches!(count, 0 | 3)
        && (count == 0 || kind == TransactionKind::Uninstall)
        && digests.into_iter().flatten().all(is_normalized_sha256)
        && (count == 0 || managed_pair_paths(install_path).is_some())
}

pub(super) fn journal_binding_suffix(journal: &Journal) -> String {
    match (
        journal.managed_pair_state_sha256.as_deref(),
        journal.managed_pair_envelope_sha256.as_deref(),
        journal.managed_pair_companion_sha256.as_deref(),
    ) {
        (Some(state), Some(envelope), Some(companion)) => {
            format!("\0{state}\0{envelope}\0{companion}")
        }
        _ => String::new(),
    }
}

fn managed_pair_paths(install_path: &Path) -> Option<(PathBuf, PathBuf, PathBuf, PathBuf)> {
    let expected_core = if cfg!(windows) { "ctx.exe" } else { "ctx" };
    let expected_companion = if cfg!(windows) {
        "ctx-pro.exe"
    } else {
        "ctx-pro"
    };
    let bin = install_path
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("bin")))
        .filter(|_| install_path.file_name() == Some(OsStr::new(expected_core)))?;
    let root = bin.parent()?.to_path_buf();
    Some((
        root.clone(),
        root.join(MANAGED_PAIR_STATE_RELATIVE_PATH),
        root.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH),
        root.join("libexec").join(expected_companion),
    ))
}

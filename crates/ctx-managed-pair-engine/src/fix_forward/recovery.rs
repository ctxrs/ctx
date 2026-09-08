use std::io::Write as _;

use super::*;

/// Reads only the bounded active pair record, without creating paths or cleaning
/// temporary files. This is a routing hint; recovery must revalidate the retained
/// candidate under the canonical installation lock before publication.
pub fn pending_managed_pair_hint(install_root: &Path) -> Result<bool> {
    filesystem::validate_absolute_root(install_root, "managed-pair install root")?;
    let path = install_root.join(crate::MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
    let entry = match filesystem::external_entry(&path, "ctx active installation transaction") {
        Ok(entry) => entry,
        Err(error) if is_not_found(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    let Some(active) = read_optional(
        &entry,
        MAX_PENDING_BYTES,
        "ctx active installation transaction",
    )?
    else {
        return Ok(false);
    };
    if !has_pair_pending_schema(&active.bytes) {
        return Ok(false);
    }
    let pending: PendingApply = serde_json::from_slice(&active.bytes)
        .context("parse ctx active installation transaction")?;
    pending.validate()?;
    Ok(true)
}

pub(super) fn finish_publication(
    layout: &Layout,
    pending: &PendingApply,
    retained: &RetainedCandidate,
    verifier: &dyn ManagedPairVerifier,
) -> Result<()> {
    verify_published_candidate(layout, pending, retained, verifier)?;
    layout.open_apply_candidate()?.revalidate()?;
    let cleanup =
        remove_pending(layout, pending).and_then(|()| filesystem::remove_apply_candidate(layout));
    layout.revalidate()?;
    match cleanup {
        Ok(()) => Ok(()),
        Err(error) if error.downcast_ref::<filesystem::RemovalIo>().is_some() => {
            // An I/O failure is disposable only while the committed image and
            // any remaining pending record still match this attempt.
            verify_published_candidate(layout, pending, retained, verifier)?;
            if read_pending(layout)?.is_some_and(|current| current != *pending) {
                bail!("ctx active installation transaction changed during cleanup");
            }
            let _ = writeln!(
                ctx_terminal::output::stderr_writer(),
                "ctx managed pair is installed, but installation cleanup remains pending: {error:#}"
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Checks existing scheduler expectations before recovery state or lifecycle
/// handoff changes. An active scheduler may outlive its pending record after a
/// committed publication; that case remains for installed-pair confirmation.
/// The caller must hold the canonical installation lock.
pub fn preflight_pending_managed_pair_under_installation_lock(
    install_root: &Path,
    expected_core_sha256: &str,
    expected_envelope_sha256: &str,
    verifier: &dyn ManagedPairVerifier,
) -> Result<()> {
    if !pending_managed_pair_hint(install_root)? {
        return Ok(());
    }
    let layout = Layout::open(install_root, false)?;
    let pending = read_pending(&layout)?
        .ok_or_else(|| anyhow!("pending managed pair disappeared during preflight"))?;
    let retained = verify_retained_candidate(&layout, &pending, verifier)?;
    if !retained
        .identity
        .core()
        .sha256()
        .eq_ignore_ascii_case(expected_core_sha256)
        || !retained
            .envelope
            .stamp
            .sha256
            .eq_ignore_ascii_case(expected_envelope_sha256)
    {
        bail!("pending managed pair does not match the expected Core/envelope identity");
    }
    layout.revalidate()
}

fn verify_published_candidate(
    layout: &Layout,
    pending: &PendingApply,
    retained: &RetainedCandidate,
    verifier: &dyn ManagedPairVerifier,
) -> Result<()> {
    let (active, envelope_sha256) = crate::validate_active(layout, verifier)?;
    if active != retained.identity
        || envelope_sha256 != pending.candidate_envelope_identity.sha256
        || !content_matches(
            &layout.target(Slot::Marker),
            &retained.marker_identity,
            MAX_MARKER_BYTES,
            Slot::Marker.label(),
        )?
    {
        bail!("published managed pair does not match the retained signed candidate");
    }
    Ok(())
}

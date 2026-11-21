use super::*;
use ctx_history_index_generation::{
    acquire_generation_read_lease, acquire_retained_generation_read_lease,
    verify_physical_integrity_read_only, ActiveGenerationPointerFence, GenerationError,
};

/// The caller holds the writer lock. Retire only a previous delta whose base
/// manifest is missing; never select another generation or synthesize data.
pub(super) fn retire_missing_previous_base(
    root: &Path,
    authority: Option<&ActivePublicationAuthority>,
    predecessor_fence: &mut ActiveGenerationPointerFence,
    retention: Option<&GenerationRetentionLease>,
    failure: IndexError,
) -> Result<()> {
    let IndexError::MissingManifest(missing) = &failure else {
        return Err(failure);
    };
    let Some(authority) = authority else {
        return Err(failure);
    };
    let pointer = authority.pointer();
    let Some(previous) = pointer.previous() else {
        return Err(failure);
    };
    // A missing slot manifest, unauthenticated delta, denied access, or lease
    // conflict is not this recovery case. The lease reader authenticates the
    // delta before reporting its distinct missing base.
    if !matches!(
        acquire_generation_read_lease(root, previous.generation_id()),
        Err(GenerationError::MissingManifest(ref generation))
            if generation == missing && generation != previous.generation_id()
    ) {
        return Err(failure);
    }
    let OpenedPinnedPublication::Published(active) = open_pinned_publication(root, authority)?
    else {
        return Err(failure);
    };
    // A durable owner still needs its exact target and dependencies, even if
    // that target is the broken previous generation. Never discard its hold.
    let _durable_read = retention
        .map(|lease| acquire_retained_generation_read_lease(root, lease))
        .transpose()?;
    let next = ActiveGenerationPointer::new(pointer.active().clone(), None)?;
    let mut validation_failure = None;
    let validate = |fence: &ActiveGenerationPointerFence| {
        let result: Result<()> = (|| {
            fence.validate(root)?;
            verify_physical_integrity_read_only(root, pointer.active(), active.searcher().index())?;
            // A retained reader can validate payloads without the named manifest.
            // Recovery changes publication authority, so authenticate the current
            // manifest and its bases afresh before removing the previous slot.
            ctx_history_index_format::clear_manifest_cache_for_root(root)?;
            let publication = ctx_history_index_format::load_publication_for_metas(
                root,
                &active.searcher().index().load_metas()?,
            )?;
            if publication.generation_id() != pointer.active().generation_id() {
                return Err(IndexError::ConcurrentGenerationChange);
            }
            fence.validate(root)?;
            Ok(())
        })();
        result.map_err(|error| {
            validation_failure = Some(error);
            GenerationError::ChecksumMismatch
        })
    };
    #[cfg(windows)]
    let outcome =
        ctx_history_index_generation::publish_active_generation_pointer_validated_predecessor_fence(
            root,
            &next,
            predecessor_fence,
            validate,
        );
    #[cfg(not(windows))]
    let outcome = {
        let mut validate = validate;
        ctx_history_index_generation::publish_active_generation_pointer_validated(
            root,
            &next,
            || validate(predecessor_fence),
        )
    };
    if let Some(error) = validation_failure {
        return Err(error);
    }
    match outcome? {
        PointerPublicationOutcome::Durable => Ok(()),
        PointerPublicationOutcome::CommittedVisible { detail } => {
            Err(IndexError::CommittedGenerationNeedsRecovery {
                generation_id: pointer.active().generation_id().to_owned(),
                stage: "previous generation retirement durability",
                detail,
            })
        }
    }
}

/// The caller holds the writer lock. Before allocating a full upgrade rebuild
/// candidate from scratch, retire an unleased `previous` slot so peak disk
/// usage stays at two generations (`active` + `candidate`) instead of three
/// (`active` + `previous` + `candidate`). Active readers of `previous` and
/// durable `GenerationRetentionLease` holds always preserve `previous`.
pub(super) fn retire_unleased_previous_before_upgrade_rebuild(
    root: &Path,
    authority: Option<&ActivePublicationAuthority>,
    predecessor_fence: &mut ActiveGenerationPointerFence,
    retention: Option<&GenerationRetentionLease>,
) -> Result<bool> {
    let Some(authority) = authority else {
        return Ok(false);
    };
    let pointer = authority.pointer();
    let Some(previous) = pointer.previous() else {
        return Ok(false);
    };
    if previous.directory() == pointer.active().directory() {
        return Ok(false);
    }
    if retention.is_some_and(|lease| {
        lease.target().directory() == previous.directory()
            || lease.generation_id() == previous.generation_id()
    }) {
        return Ok(false);
    }
    // Acquire exclusive directory reclaim authority on `previous` before
    // updating `active-generation.json`. Any live reader of `previous` holds
    // a shared lease on this directory key; holding exclusive authority across
    // pointer publication also prevents a racing reader from acquiring
    // `previous` right before it is retired. A live reader of a flat-delta
    // `active` generation holds only `previous`'s manifest id key (not its
    // directory key), so retiring the directory remains safe while
    // `reclaim_unreferenced_manifests` preserves `previous.json` via the
    // retained manifest closure.
    let Some(_previous_directory_reclaim_guard) =
        ctx_history_index_generation::try_generation_directory_reclaim_authority(
            root,
            previous.directory(),
        )?
    else {
        return Ok(false);
    };
    let active_dir = ctx_history_index_generation::slot_path(root, pointer.active());
    let next = ActiveGenerationPointer::new(pointer.active().clone(), None)?;
    let mut validation_failure = None;
    let validate = |fence: &ActiveGenerationPointerFence| {
        let result: Result<()> = (|| {
            fence.validate(root)?;
            let metadata = fs::symlink_metadata(&active_dir)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(IndexError::ConcurrentGenerationChange);
            }
            fence.validate(root)?;
            Ok(())
        })();
        result.map_err(|error| {
            validation_failure = Some(error);
            GenerationError::ChecksumMismatch
        })
    };
    #[cfg(windows)]
    let outcome =
        ctx_history_index_generation::publish_active_generation_pointer_validated_predecessor_fence(
            root,
            &next,
            predecessor_fence,
            validate,
        );
    #[cfg(not(windows))]
    let outcome = {
        let mut validate = validate;
        ctx_history_index_generation::publish_active_generation_pointer_validated(
            root,
            &next,
            || validate(predecessor_fence),
        )
    };
    if let Some(error) = validation_failure {
        return Err(error);
    }
    match outcome? {
        PointerPublicationOutcome::Durable => Ok(true),
        PointerPublicationOutcome::CommittedVisible { detail } => {
            Err(IndexError::CommittedGenerationNeedsRecovery {
                generation_id: pointer.active().generation_id().to_owned(),
                stage: "previous generation upgrade retirement durability",
                detail,
            })
        }
    }
}

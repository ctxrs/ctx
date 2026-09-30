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

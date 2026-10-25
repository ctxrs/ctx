use ctx_history_index::VerifiedIndex;
use ctx_history_read_application::{
    ArchiveLabelAuthority, ArchiveProvenance, ArchiveProvenanceMap,
};

pub(crate) fn load(
    index: &VerifiedIndex,
) -> Result<ArchiveProvenanceMap, ctx_history_archive::ArchiveError> {
    Ok(ArchiveProvenanceMap(
        ctx_history_archive::source_identities(index)?
            .into_iter()
            .map(|(source, identity)| {
                (
                    source,
                    ArchiveProvenance {
                        origin: identity.origin,
                        view: identity.view,
                        label_authority: ArchiveLabelAuthority::ArchiveCreator,
                    },
                )
            })
            .collect(),
    ))
}

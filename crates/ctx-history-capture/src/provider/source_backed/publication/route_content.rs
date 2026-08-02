use super::*;

pub(super) fn source_route_content_fingerprint(
    manifest: Option<&GenerationManifest>,
    route_identity: &SourceRouteIdentity,
) -> [u8; 32] {
    let sources = manifest
        .and_then(|manifest| manifest.source_route(route_identity))
        .map(SourceRouteSnapshot::sources)
        .unwrap_or_default();
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-route-content-v1\0");
    digest.update((sources.len() as u64).to_be_bytes());
    for source in sources {
        digest.update(source.identity().digest());
        match manifest.and_then(|manifest| source_core_record_aggregate(manifest, source)) {
            Some(aggregate) => {
                digest.update([1]);
                digest.update(aggregate.indexed_documents().to_be_bytes());
                digest.update(aggregate.semantic_eligible_documents().to_be_bytes());
                digest.update(aggregate.core_record_accumulator().as_bytes());
            }
            None => digest.update([0]),
        }
    }
    digest.finalize().into()
}

fn source_core_record_aggregate<'manifest>(
    manifest: &'manifest GenerationManifest,
    source: &SourceKey,
) -> Option<&'manifest ctx_history_index::SourceCoreRecordAggregate> {
    manifest
        .sources
        .binary_search_by_key(&source.identity().digest(), |candidate| {
            candidate.observation().source().identity().digest()
        })
        .ok()
        .and_then(|index| manifest.core_record_aggregates.get(index))
}

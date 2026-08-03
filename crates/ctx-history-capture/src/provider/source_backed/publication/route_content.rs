use super::*;

pub(super) fn source_route_content_fingerprints(
    manifest: Option<&GenerationManifest>,
) -> HashMap<SourceRouteIdentity, [u8; 32]> {
    let Some(manifest) = manifest else {
        return HashMap::new();
    };
    let aggregates = manifest
        .sources
        .iter()
        .zip(&manifest.core_record_aggregates)
        .map(|(source, aggregate)| (source.observation().source().identity().digest(), aggregate))
        .collect::<HashMap<_, _>>();
    manifest
        .source_routes()
        .iter()
        .map(|route| {
            (
                route.route_identity().clone(),
                source_route_content_fingerprint(route.sources(), &aggregates),
            )
        })
        .collect()
}

pub(super) fn empty_source_route_content_fingerprint() -> [u8; 32] {
    source_route_content_fingerprint(&[], &HashMap::new())
}

fn source_route_content_fingerprint(
    sources: &[SourceKey],
    aggregates: &HashMap<[u8; 32], &ctx_history_index::SourceCoreRecordAggregate>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-route-content-v2\0");
    digest.update((sources.len() as u64).to_be_bytes());
    for source in sources {
        digest.update(source.identity().digest());
        match aggregates.get(&source.identity().digest()) {
            Some(aggregate) => {
                digest.update([1]);
                digest.update(aggregate.indexed_documents().to_be_bytes());
                digest.update(aggregate.core_record_accumulator().as_bytes());
            }
            None => digest.update([0]),
        }
    }
    digest.finalize().into()
}

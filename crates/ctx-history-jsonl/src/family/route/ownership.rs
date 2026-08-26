use super::*;

fn disposition_source_is_route_local<R: JsonlFamilyRuntime>(
    source: Option<&SourceKey>,
    sink: &SourceBackedGenerationSink<'_, R::Lifecycle>,
) -> bool {
    source.is_none_or(|source| !sink.source_owned_by_other_route(source))
}

pub(super) fn quarantined_member_is_route_local<R: JsonlFamilyRuntime>(
    rejected: &JsonlFamilyRejectedLeaf,
    sink: &SourceBackedGenerationSink<'_, R::Lifecycle>,
) -> bool {
    disposition_source_is_route_local::<R>(rejected.source(), sink)
        && disposition_source_is_route_local::<R>(
            rejected
                .logical_source_failure
                .as_ref()
                .map(|(source, _)| source),
            sink,
        )
}

pub(super) fn route_local_disposition_counts<R: JsonlFamilyRuntime>(
    opening: &JsonlFamilyInventory<JsonlRuntimeError<R>>,
    sink: &SourceBackedGenerationSink<'_, R::Lifecycle>,
) -> (usize, usize) {
    let quarantined = opening
        .quarantined_leaves()
        .filter(|rejected| quarantined_member_is_route_local::<R>(rejected, sink))
        .count();
    let pending = opening
        .pending_leaves()
        .filter(|pending| disposition_source_is_route_local::<R>(pending.source(), sink))
        .count();
    (quarantined, pending)
}

pub(super) fn base_sources_for_route<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    sink: &SourceBackedGenerationSink<'_, R::Lifecycle>,
) -> SourceBackedRouteResult<Vec<CertifiedSource>> {
    Ok(sink
        .base_route_sources()
        .map_err(route_internal)?
        .into_values()
        .filter(|source| adapter.owns(source.observation().source()))
        .collect())
}

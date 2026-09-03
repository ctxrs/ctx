use super::*;

pub(super) type RetainedGenerationState = (
    Option<ExplicitSourceCatalogAuthority>,
    Vec<ExplicitSourceCatalogRouteBinding>,
    BTreeMap<SourceRouteIdentity, Vec<u8>>,
);

pub(super) fn retained_generation_state(
    retained_generation: Option<&VerifiedIndex>,
) -> Result<RetainedGenerationState> {
    let Some(generation) = retained_generation else {
        return Ok((None, Vec::new(), BTreeMap::new()));
    };
    let state = SourceBackedGenerationState::decode_from_verified_index(generation)
        .context("decode retained source-backed generation state")?;
    Ok((
        state.applied_explicit_source_catalog().cloned(),
        state.catalog_route_bindings().to_vec(),
        state.route_controls().clone(),
    ))
}

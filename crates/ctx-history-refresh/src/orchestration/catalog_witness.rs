use super::*;
use ctx_history_index::VerifiedGenerationSnapshot;

pub(super) type RetainedGenerationState = (
    Option<ExplicitSourceCatalogAuthority>,
    Vec<ExplicitSourceCatalogRouteBinding>,
    BTreeMap<SourceRouteIdentity, Vec<u8>>,
);

pub(super) fn retained_generation_state(
    retained_generation: Option<&VerifiedGenerationSnapshot>,
    requires_rebuild: bool,
) -> Result<RetainedGenerationState> {
    let Some(generation) = retained_generation else {
        return Ok((None, Vec::new(), BTreeMap::new()));
    };
    if requires_rebuild && generation.manifest().generation_state().is_none() {
        return Ok((None, Vec::new(), BTreeMap::new()));
    }
    let state = SourceBackedGenerationState::decode_from_manifest(generation.manifest())
        .context("decode retained source-backed generation state")?;
    let route_controls = if requires_rebuild {
        BTreeMap::new()
    } else {
        state.route_controls().clone()
    };
    Ok((
        state.applied_explicit_source_catalog().cloned(),
        state.catalog_route_bindings().to_vec(),
        route_controls,
    ))
}

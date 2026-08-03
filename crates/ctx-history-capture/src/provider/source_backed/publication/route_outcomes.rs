use super::*;

use super::route_content::{
    empty_source_route_content_fingerprint, source_route_content_fingerprints,
};

pub(super) fn successful_route_outcomes_for_manifest(
    selected_route_ids: &BTreeSet<SourceRouteIdentity>,
    failed_routes: &BTreeMap<SourceRouteIdentity, SourceBackedFailedRoute>,
    logical_source_failures: &SourceBackedLogicalSourceFailures,
    base_route_content: &HashMap<SourceRouteIdentity, [u8; 32]>,
    manifest: &GenerationManifest,
) -> Vec<SourceBackedSuccessfulRouteOutcome> {
    let current_route_content = source_route_content_fingerprints(Some(manifest));
    let empty_route_content = empty_source_route_content_fingerprint();
    selected_route_ids
        .iter()
        .filter(|identity| !failed_routes.contains_key(*identity))
        .cloned()
        .map(|route_identity| SourceBackedSuccessfulRouteOutcome {
            logical_source_failure_total: logical_source_failures.route_total(&route_identity),
            changed: base_route_content
                .get(&route_identity)
                .unwrap_or(&empty_route_content)
                != current_route_content
                    .get(&route_identity)
                    .unwrap_or(&empty_route_content),
            route_identity,
        })
        .collect()
}

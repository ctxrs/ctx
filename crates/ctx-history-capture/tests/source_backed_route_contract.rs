use std::collections::BTreeSet;

use ctx_history_capture::{
    source_backed_route_constructor, source_backed_route_inventory, SourceBackedRouteConstructor,
    SourceBackedSelectorAuthority, SourceBackedWatchTargetKind,
};
use serde_json::{json, Value};

const ROUTE_REGISTRY_GOLDEN: &str = include_str!("goldens/source-backed-route-registry-v1.json");
const PUBLIC_SUPPORT_MATRIX: &str = include_str!("../../../docs/provider-support-matrix.json");
const EXPECTED_ROUTE_COUNT: usize = 52;
const EXPECTED_AUTOMATIC_ROUTE_COUNT: usize = 42;

fn selector_authority_label(authority: SourceBackedSelectorAuthority) -> &'static str {
    match authority {
        SourceBackedSelectorAuthority::DiscoveredWinner => "DiscoveredWinner",
        SourceBackedSelectorAuthority::ExplicitPath => "ExplicitPath",
        SourceBackedSelectorAuthority::CatalogLineage => "CatalogLineage",
        SourceBackedSelectorAuthority::ExactCwd => "ExactCwd",
        SourceBackedSelectorAuthority::NamedSurface => "NamedSurface",
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit => {
            "SelectedWithRetainedExplicit"
        }
    }
}

fn constructor_label(constructor: SourceBackedRouteConstructor) -> &'static str {
    match constructor {
        SourceBackedRouteConstructor::ProviderSource => "ProviderSource",
        SourceBackedRouteConstructor::CatalogLineage => "CatalogLineage",
        SourceBackedRouteConstructor::FiniteInventory => "FiniteInventory",
        SourceBackedRouteConstructor::DiscoveryContext => "DiscoveryContext",
        SourceBackedRouteConstructor::ExactCwd => "ExactCwd",
        SourceBackedRouteConstructor::NamedSurface => "NamedSurface",
        SourceBackedRouteConstructor::SelectedWithRetainedRoutes => "SelectedWithRetainedRoutes",
    }
}

fn watch_target_kind_label(kind: SourceBackedWatchTargetKind) -> &'static str {
    match kind {
        SourceBackedWatchTargetKind::Path => "Path",
        SourceBackedWatchTargetKind::SqliteDatabase => "SqliteDatabase",
    }
}

fn current_registry_contract() -> Value {
    let routes = source_backed_route_inventory()
        .iter()
        .map(|route| {
            json!({
                "provider": route.provider.as_str(),
                "source_format": route.source_format,
                "certified_source_format": route.certified_source_format,
                "automatic": route.automatic,
                "explicit_manual": route.explicit_manual,
                "selector_authority": selector_authority_label(route.selector_authority),
                "unsupported_reason": route.unsupported_reason,
                "constructor": constructor_label(route.constructor),
                "watch_target_kind": watch_target_kind_label(route.watch_target_kind),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "schema_version": 1,
        "routes": routes,
    })
}

#[test]
fn public_route_registry_matches_the_ordered_golden_contract() {
    let expected: Value =
        serde_json::from_str(ROUTE_REGISTRY_GOLDEN).expect("route registry golden must parse");
    let actual = current_registry_contract();

    assert_eq!(source_backed_route_inventory().len(), EXPECTED_ROUTE_COUNT);
    assert_eq!(actual, expected);
}

#[test]
fn public_support_matrix_is_exactly_the_automatic_route_projection() {
    let matrix: Value =
        serde_json::from_str(PUBLIC_SUPPORT_MATRIX).expect("public support matrix must parse");
    let providers = matrix["providers"]
        .as_array()
        .expect("public support matrix providers must be an array");
    let mut matrix_projection = BTreeSet::new();

    for provider in providers {
        assert_eq!(provider["status"].as_str(), Some("supported"));
        let capture_provider = provider["capture_provider"]
            .as_str()
            .expect("supported provider must name its capture provider");
        let implemented_paths = provider["implemented_paths"]
            .as_array()
            .expect("supported provider must list implemented paths");
        for path in implemented_paths {
            assert_eq!(path["kind"].as_str(), Some("native_import"));
            let source_format = path["source_format"]
                .as_str()
                .expect("native import path must name its source format");
            assert!(
                matrix_projection.insert((capture_provider.to_owned(), source_format.to_owned())),
                "duplicate public support route {capture_provider} {source_format}"
            );
        }
    }

    let automatic_routes = source_backed_route_inventory()
        .iter()
        .filter(|route| route.automatic)
        .collect::<Vec<_>>();
    let automatic_projection = automatic_routes
        .iter()
        .map(|route| {
            (
                route.provider.as_str().to_owned(),
                route.source_format.to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();

    assert_eq!(matrix_projection.len(), EXPECTED_AUTOMATIC_ROUTE_COUNT);
    assert_eq!(automatic_routes.len(), EXPECTED_AUTOMATIC_ROUTE_COUNT);
    assert_eq!(automatic_projection.len(), EXPECTED_AUTOMATIC_ROUTE_COUNT);
    assert_eq!(automatic_projection, matrix_projection);

    for route in automatic_routes {
        assert!(
            source_backed_route_constructor(route.provider).is_some(),
            "{} must expose a deterministic route constructor",
            route.provider.as_str()
        );
    }
}

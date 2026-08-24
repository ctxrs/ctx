use super::*;
use crate::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind, ProviderSourceStatus,
};

#[test]
fn codex_session_tree_registration_does_not_inventory_the_root() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let sessions = temp.path().join("sessions-not-created");
    let archived_sessions = temp.path().join("archived-sessions-not-created");
    let source = ProviderSource {
        provider: CaptureProvider::Codex,
        path: sessions.clone(),
        exists: true,
        source_format: "codex_session_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();

    register_codex_session_tree_routes(
        &mut registry,
        vec![
            source,
            ProviderSource {
                provider: CaptureProvider::Codex,
                path: archived_sessions.clone(),
                exists: true,
                source_format: "codex_session_jsonl_tree",
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
            },
        ],
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();

    assert_eq!(registry.routes().count(), 1);
    let route_identity = registry
        .routes()
        .next()
        .and_then(|route| route.route_identity.clone())
        .unwrap();
    let registration_roots = registry
        .catalog_coverage_route_registration_sources(&route_identity)
        .unwrap()
        .map(|source| source.path.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        registration_roots,
        BTreeSet::from([sessions, archived_sessions.clone()])
    );
    let catalog = registry.watch_catalog();
    let targets = catalog.route_targets().next().unwrap().1;
    assert!(targets.contains(&archived_sessions));
}

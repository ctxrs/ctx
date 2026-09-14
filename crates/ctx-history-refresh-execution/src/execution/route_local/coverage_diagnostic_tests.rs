use super::*;

#[test]
fn each_zero_source_blocker_preserves_its_typed_reason() {
    for expected in [
        "catalog_unavailable",
        "missing_terminal_authority",
        "route_failed",
        "invalid_route_identity",
        "missing_empty_authority",
    ] {
        let mut publication = crate::tests::test_publication("generation-1");
        publication.current.source_count = 0;
        let mut complete = BTreeSet::new();
        let mut blockers = RouteLessRegistryBlockers::default();
        match expected {
            "catalog_unavailable" => blockers.total = 1,
            "missing_terminal_authority" => {
                complete.insert(SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap());
            }
            "route_failed" => {
                publication
                    .route_results
                    .push(SourceBackedRefreshRouteResult::failed(
                        "ab".repeat(32),
                        "unavailable".to_owned(),
                        false,
                    ))
            }
            "invalid_route_identity" => {
                publication
                    .route_results
                    .push(SourceBackedRefreshRouteResult::succeeded(
                        "invalid".to_owned(),
                        false,
                    ))
            }
            "missing_empty_authority" => {
                publication
                    .route_results
                    .push(SourceBackedRefreshRouteResult::succeeded(
                        "ab".repeat(32),
                        false,
                    ))
            }
            _ => unreachable!(),
        }
        let SourceBackedInventoryDisposition::UnsupportedOrUnavailable(error) =
            classify_inventory_disposition(&publication, &complete, &BTreeSet::new(), &blockers)
        else {
            panic!("expected blocker {expected}");
        };
        assert_eq!(error.reason().unwrap().as_str(), expected);
        assert!(error
            .to_string()
            .starts_with("all_provider_terminal_coverage_unavailable:"));
        assert_eq!(
            ZeroSourcePublicationBlockReason::parse(expected),
            error.reason()
        );
    }
}

#[test]
fn diagnostics_do_not_block_authoritative_empty_or_content() {
    let mut publication = crate::tests::test_publication("generation-1");
    let empty = BTreeSet::new();
    let blockers = RouteLessRegistryBlockers::default();
    assert!(matches!(
        classify_inventory_disposition(&publication, &empty, &empty, &blockers),
        SourceBackedInventoryDisposition::AuthoritativeContent
    ));
    publication.current.source_count = 0;
    assert!(matches!(
        classify_inventory_disposition(&publication, &empty, &empty, &blockers),
        SourceBackedInventoryDisposition::AuthoritativeEmpty(_)
    ));
    let route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    publication
        .route_results
        .push(SourceBackedRefreshRouteResult::succeeded(
            route.as_str().to_owned(),
            false,
        ));
    assert!(matches!(
        classify_inventory_disposition(&publication, &BTreeSet::from([route]), &empty, &blockers),
        SourceBackedInventoryDisposition::AuthoritativeEmpty(_)
    ));
    assert!(
        ZeroSourcePublicationBlocked::new("unsafe_root /private/source")
            .reason()
            .is_none()
    );
    assert!(ZeroSourcePublicationBlockReason::parse("/private/source").is_none());
}

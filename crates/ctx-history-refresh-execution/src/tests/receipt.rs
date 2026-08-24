//! Receipt and timing coverage owned by physical refresh execution.

use super::*;

fn terminal_receipt_fixture() -> (tempfile::TempDir, Value, VerifiedIndex) {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_pin_source(temp.path(), publication_pin_source_with_anchor(0x91));
    let verified = VerifiedIndex::open(temp.path()).unwrap();
    let mut publication = test_publication(generation.clone());
    publication.current =
        SourceBackedRefreshCurrent::from_sources(&verified.manifest().sources, 0).unwrap();
    publication.certified_source_count = publication.current.source_count;
    publication.certified_source_bytes = publication.current.certified_source_bytes;
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        generation.clone(),
        &publication,
    )
    .unwrap();
    let response = json!({
        "previous_generation": Value::Null,
        "published_generation": generation,
        "generation_changed": true,
        "certified_source_count": publication.certified_source_count,
        "certified_source_bytes": publication.certified_source_bytes,
        "receipt": receipt.to_json(),
    });
    (temp, response, verified)
}

#[test]
fn recovery_receipt_requires_certified_current_facts() {
    let (_temp, response, _verified) = terminal_receipt_fixture();
    for (field, value) in [
        ("certified_source_count", json!(0)),
        ("certified_source_bytes", json!(0)),
    ] {
        let mut malformed = response.clone();
        malformed[field] = value;
        let error = published_refresh_receipt_for_recovery(&malformed).unwrap_err();
        assert!(
            format!("{error:#}").contains("certified current facts"),
            "{error:#}"
        );
    }
}

#[test]
fn recovery_receipt_catalog_binding_requires_a_receipt_route_and_terminal_failure() {
    let (_temp, mut response, _verified) = terminal_receipt_fixture();
    let route = "56".repeat(32);
    let lineage = "57".repeat(32);
    response["receipt"]["selected_route_total"] = json!(1);
    response["receipt"]["successful_route_total"] = json!(0);
    response["receipt"]["source_failure_total"] = json!(1);
    response["receipt"]["source_failures_omitted"] = json!(1);
    response["receipt"]["catalog_route_bindings"] = json!({
        (lineage): route.clone()
    });
    for carried_forward in [false, true] {
        response["receipt"]["route_results"] = json!({
            (route.clone()): ["f", "u", carried_forward, 1, []]
        });
        published_refresh_receipt_for_recovery(&response)
            .expect("a transient binding may use any consistent terminal route failure");
    }

    response["receipt"]["catalog_route_bindings"] = json!({
        ("58".repeat(32)): "59".repeat(32)
    });
    let error = published_refresh_receipt_for_recovery(&response).unwrap_err();
    assert!(format!("{error:#}").contains("absent from route_results"));
}

#[test]
fn verified_receipt_keeps_retryable_binding_for_a_retained_route_failure() {
    let (_temp, _response, verified) = terminal_receipt_fixture();
    let route = verified.manifest().source_routes()[0]
        .route_identity()
        .as_str()
        .to_owned();
    let lineage = "5a".repeat(32);
    let mut publication = test_publication(verified.generation_id());
    publication.current =
        SourceBackedRefreshCurrent::from_sources(&verified.manifest().sources, 0).unwrap();
    publication.certified_source_count = publication.current.source_count;
    publication.certified_source_bytes = publication.current.certified_source_bytes;
    publication.route_results = vec![SourceBackedRefreshRouteResult::failed(
        route.clone(),
        "source_changed".to_owned(),
        true,
    )];
    publication.catalog_route_bindings = vec![ExplicitSourceCatalogRouteBinding {
        catalog_lineage: lineage.clone(),
        route_identity: route,
    }];
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        Some(verified.generation_id().to_owned()),
        verified.generation_id().to_owned(),
        &publication,
    )
    .unwrap();
    let response = json!({
        "previous_generation": verified.generation_id(),
        "published_generation": verified.generation_id(),
        "generation_changed": false,
        "certified_source_count": publication.current.source_count,
        "certified_source_bytes": publication.current.certified_source_bytes,
        "receipt": receipt.to_json(),
    });

    let roundtrip = published_refresh_receipt_for_index(&response, &verified).unwrap();
    let outcome = roundtrip.catalog_route_outcome(&lineage).unwrap();
    assert_eq!(outcome.outcome, "failed");
    assert_eq!(outcome.failure_class.as_deref(), Some("source_changed"));
    assert_eq!(
        source_backed_route_retry_disposition(&roundtrip.route_results[0]),
        Some(true)
    );
}

#[test]
fn refresh_scan_timing_excludes_the_separately_reported_commit_interval() {
    assert_eq!(
        exclusive_scan_stage_duration(StdDuration::from_micros(700), StdDuration::from_micros(250)),
        StdDuration::from_micros(450),
    );
    assert_eq!(
        exclusive_scan_stage_duration(StdDuration::from_micros(100), StdDuration::from_micros(250)),
        StdDuration::ZERO,
    );
}

#[test]
fn receipt_source_count_intersects_request_routes_with_certified_generation_routes() {
    let temp = tempfile::tempdir().unwrap();
    let requested_route = SourceRouteIdentity::from_sha256("a1".repeat(32)).unwrap();
    let unrelated_route = SourceRouteIdentity::from_sha256("a2".repeat(32)).unwrap();
    let absent_route = SourceRouteIdentity::from_sha256("a3".repeat(32)).unwrap();
    let requested_source = publication_pin_source_with_anchor(0xa1);
    let unrelated_source = publication_pin_source_with_anchor(0xa2);
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for source in [&requested_source, &unrelated_source] {
        writer.begin_source(source.clone()).unwrap();
        writer
            .add_core_record(publication_pin_record(source))
            .unwrap();
        writer
            .certify_source(publication_pin_certificate(source))
            .unwrap();
    }
    writer
        .set_present_source_routes(vec![
            ctx_history_index::SourceRouteSnapshot::present(
                requested_route.clone(),
                vec![requested_source],
            )
            .unwrap(),
            ctx_history_index::SourceRouteSnapshot::present(
                unrelated_route,
                vec![unrelated_source],
            )
            .unwrap(),
        ])
        .unwrap();
    let generation = writer.commit(|_| true).unwrap().generation_id;
    let verified = VerifiedIndex::open(temp.path()).unwrap();
    let receipt = SourceBackedRefreshReceipt {
        zero_source_authority: Vec::new(),
        previous_generation: None,
        published_generation: generation,
        generation_changed: true,
        published_explicit_source_catalog: None,
        current: SourceBackedRefreshCurrent {
            source_count: 2,
            ..SourceBackedRefreshCurrent::default()
        },
        route_results: vec![
            SourceBackedRefreshRouteResult::succeeded(requested_route.as_str().to_owned(), true),
            SourceBackedRefreshRouteResult::succeeded(absent_route.as_str().to_owned(), false),
        ],
        catalog_route_bindings: Vec::new(),
    };

    assert_eq!(receipt.source_count(&verified), 1);
    let mut failed_carried = receipt;
    failed_carried.route_results = vec![SourceBackedRefreshRouteResult::failed(
        requested_route.as_str().to_owned(),
        "unavailable".to_owned(),
        true,
    )];
    assert_eq!(failed_carried.source_count(&verified), 0);
}

#[test]
fn maximum_route_authoritative_empty_receipt_fits_the_durable_bound() {
    let generation_id = "44".repeat(32);
    let routes = (0_u16..SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT as u16)
        .map(|index| SourceRouteIdentity::from_sha256(format!("{index:064x}")).unwrap())
        .collect::<Vec<_>>();
    let mut publication = test_publication(generation_id.clone());
    publication.certified_source_count = 0;
    publication.certified_source_bytes = 0;
    publication.current = SourceBackedRefreshCurrent::default();
    publication.route_results = routes
        .iter()
        .map(|route| SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), false))
        .collect();
    publication.zero_source_authority = routes
        .iter()
        .enumerate()
        .map(|(index, route)| SourceBackedZeroSourceAuthority {
            generation_id: generation_id.clone(),
            route_identity: route.clone(),
            kind: if index % 2 == 0 {
                SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory
            } else {
                SourceBackedZeroSourceAuthorityKind::ConfirmedDeletion
            },
        })
        .collect();

    let receipt =
        SourceBackedRefreshReceipt::from_verified_publication(None, generation_id, &publication)
            .unwrap();
    let value = receipt.to_json();
    assert!(serde_json::to_vec(&value).unwrap().len() <= SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES);
    let decoded_routes = required_route_results(value.get("route_results")).unwrap();
    assert_eq!(
        parse_zero_source_authority(value.get("zero_source_authority"), &decoded_routes).unwrap(),
        publication.zero_source_authority
    );
}

#[test]
fn terminal_receipt_requires_one_canonical_route_result_collection() {
    let (_temp, response, verified) = terminal_receipt_fixture();
    for field in [
        "selected_route_total",
        "successful_route_total",
        "source_failure_total",
        "source_failures_omitted",
        "rejected_record_total",
        "rejection_diagnostics_omitted",
        "route_results",
        "catalog_route_bindings",
    ] {
        let mut missing = response.clone();
        missing["receipt"].as_object_mut().unwrap().remove(field);
        let error = published_refresh_receipt_for_index(&missing, &verified).unwrap_err();
        assert!(format!("{error:#}").contains(field), "{field}: {error:#}");
    }
}

#[test]
fn terminal_publication_rejects_route_rejections_beyond_committed_core_total() {
    let route_identity = "46".repeat(32);
    let mut result = SourceBackedRefreshRouteResult::succeeded(route_identity, true);
    result.rejected_record_total = 2;
    let mut publication = test_publication("generation-with-impossible-rejections");
    publication.current.rejected_records = 1;
    publication.current.sources_with_rejections = 1;
    publication.route_results = vec![result];

    let error = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        publication.generation_id.clone(),
        &publication,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("route rejections exceed"));
}

#[test]
fn terminal_receipt_rejects_omitted_success_failure_and_inconsistent_totals() {
    let (_temp, response, verified) = terminal_receipt_fixture();
    let route = "22".repeat(32);
    let cases = [
        {
            let mut value = response.clone();
            value["receipt"]["selected_route_total"] = json!(1);
            value
        },
        {
            let mut value = response.clone();
            value["receipt"]["route_results"] = json!({ (route.clone()): ["s", false] });
            value
        },
        {
            let mut value = response.clone();
            value["receipt"]["route_results"] = json!({ (route): ["f", "u", false, 1, []] });
            value["receipt"]["selected_route_total"] = json!(1);
            value
        },
    ];
    for value in cases {
        let error = published_refresh_receipt_for_index(&value, &verified).unwrap_err();
        assert!(format!("{error:#}").contains("invalid route-result partition"));
    }
}

#[test]
fn terminal_receipt_rejects_malformed_or_untyped_route_results() {
    let (_temp, response, verified) = terminal_receipt_fixture();
    for outcome in [
        json!(["s"]),
        json!(["s", false, "extra"]),
        json!(["f", "unknown", false, 1, []]),
        json!(["f", "u"]),
    ] {
        let mut malformed = response.clone();
        malformed["receipt"]["route_results"] = json!({ ("33".repeat(32)): outcome });
        malformed["receipt"]["selected_route_total"] = json!(1);
        let detail = format!(
            "{:#}",
            published_refresh_receipt_for_index(&malformed, &verified).unwrap_err()
        );
        assert!(
            detail.contains("route result") || detail.contains("failure class"),
            "{detail}"
        );
    }
}

#[test]
fn terminal_receipt_preserves_legitimate_empty_route_results() {
    let (_temp, response, verified) = terminal_receipt_fixture();
    let receipt = published_refresh_receipt_for_index(&response, &verified).unwrap();
    assert_eq!(receipt.selected_route_total(), 0);
    assert_eq!(receipt.successful_route_total(), 0);
    assert!(receipt.route_results.is_empty());
    assert!(receipt.catalog_route_bindings.is_empty());
    assert_eq!(receipt.to_json()["outcome"], "completed");
}

#[test]
fn successful_route_with_rejection_is_canonical_and_keeps_location_and_payload_type() {
    let route_identity = "44".repeat(32);
    let source_identity = "55".repeat(32);
    let mut result = SourceBackedRefreshRouteResult::succeeded(route_identity.clone(), true);
    result.rejected_record_total = 1;
    result.rejection_diagnostics = vec![SourceBackedRefreshRecordRejection {
        route_identity: route_identity.clone(),
        source_identity: source_identity.clone(),
        provider: "opencode".to_owned(),
        source_selector: "/tmp/history.jsonl".to_owned(),
        line: 2,
        payload_type: "unsupported_record".to_owned(),
        class: "unsupported_record".to_owned(),
        detail: "unsupported record body was rejected".to_owned(),
    }];
    let mut publication = test_publication("generation-with-rejection");
    publication.current.rejected_records = 1;
    publication.current.sources_with_rejections = 1;
    publication.route_results = vec![result];
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        publication.generation_id.clone(),
        &publication,
    )
    .unwrap();

    assert_eq!(receipt.terminal_outcome(), "completed_with_rejections");
    assert_eq!(receipt.successful_route_total(), 1);
    assert_eq!(receipt.rejected_record_total(), 1);
    let wire = receipt.to_json();
    assert_eq!(wire["route_results"][&route_identity][5], 1);
    assert_eq!(
        wire["route_results"][&route_identity][6][0],
        json!([
            source_identity,
            "opencode",
            "/tmp/history.jsonl",
            2,
            "unsupported_record",
            "u",
            "unsupported record body was rejected"
        ])
    );
}

#[test]
fn bounded_failure_receipt_keeps_exact_nonretryable_disposition() {
    let route_identity = "46".repeat(32);
    let mut result = SourceBackedRefreshRouteResult::succeeded(route_identity.clone(), false);
    result.source_failure_total = 234;
    result.source_retryable_failure_total = 0;
    let mut publication = test_publication("generation-with-bounded-failures");
    publication.route_results = vec![result];
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        publication.generation_id.clone(),
        &publication,
    )
    .unwrap();
    let wire = receipt.to_json();

    assert_eq!(wire["route_results"][&route_identity][3], 0);
    let parsed = required_route_results(wire.get("route_results")).unwrap();
    assert_eq!(
        source_backed_route_retry_disposition(&parsed[0]),
        Some(false)
    );

    let legacy = json!({
        (route_identity): ["s", false, 234, [], 0, []]
    });
    let parsed_legacy = required_route_results(Some(&legacy)).unwrap();
    assert_eq!(
        source_backed_route_retry_disposition(&parsed_legacy[0]),
        Some(true),
        "old truncated receipts remain conservative"
    );
}

#[test]
fn bounded_receipt_keeps_every_route_result_exact() {
    let (_temp, _response, verified) = terminal_receipt_fixture();
    let mut publication = test_publication(verified.generation_id());
    publication.current =
        SourceBackedRefreshCurrent::from_sources(&verified.manifest().sources, 0).unwrap();
    publication.route_results = (0..256)
        .map(|index| {
            let route_identity = format!("{index:064x}");
            let mut result = SourceBackedRefreshRouteResult::failed(
                route_identity.clone(),
                "unreadable".to_owned(),
                false,
            );
            result.source_failures = vec![SourceBackedRefreshSourceFailure {
                route_identity,
                source_identity: "cd".repeat(32),
                provider: "opencode".to_owned(),
                class: "unreadable".to_owned(),
                carried_forward: false,
                source_selector: "s".repeat(512),
                detail: "d".repeat(512),
            }];
            result
        })
        .collect();
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        publication.generation_id.clone(),
        &publication,
    )
    .unwrap();
    let wire = receipt.to_json();
    let response = json!({
        "previous_generation": Value::Null,
        "published_generation": publication.generation_id,
        "generation_changed": true,
        "certified_source_count": publication.current.source_count,
        "certified_source_bytes": publication.current.certified_source_bytes,
        "receipt": wire.clone(),
    });
    let transmitted = published_refresh_receipt_for_index(&response, &verified).unwrap();
    assert!(serde_json::to_vec(&wire).unwrap().len() <= SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES);
    assert_eq!(receipt.route_results.len(), 256);
    assert_eq!(transmitted.source_failure_total(), 256);
    assert!(transmitted.source_failure_diagnostic_count() < 256);
    assert_eq!(wire, receipt.to_json());
}

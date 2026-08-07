//! Receipt coverage owned by the refresh engine.

use super::*;

fn terminal_receipt_fixture() -> (tempfile::TempDir, Value, PinnedSourceBackedGeneration) {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| publish_pin_fixture(&execution, false),
    ));
    coordinator.enqueue_periodic(&data_root).unwrap();
    let run = coordinator.run_next(&data_root).expect("terminal refresh");
    assert!(!run.failed, "{:#}", run.job);
    let pin = pin_published_generation(&data_root)
        .unwrap()
        .expect("published generation");
    (temp, run.job, pin)
}

#[test]
fn terminal_receipt_requires_one_canonical_route_result_collection() {
    let (_temp, response, pin) = terminal_receipt_fixture();
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
        missing["receipt"]
            .as_object_mut()
            .unwrap()
            .remove(field)
            .unwrap();
        let error = published_refresh_receipt(&missing, &pin)
            .expect_err("omitted terminal authority must fail closed");
        assert!(format!("{error:#}").contains(field), "{field}: {error:#}");
    }
}

#[test]
fn terminal_publication_rejects_route_rejections_beyond_committed_core_total() {
    let route_identity = "46".repeat(32);
    let mut result = SourceBackedRefreshRouteResult::succeeded(route_identity, true);
    result.rejected_record_total = 2;
    let publication = SourceBackedRefreshPublication {
        generation_id: "generation-with-impossible-rejections".to_owned(),
        published_explicit_source_catalog: None,
        unsupported_routes: 0,
        certified_source_count: 1,
        certified_source_bytes: 1,
        current: SourceBackedRefreshCurrent {
            source_count: 1,
            rejected_records: 1,
            sources_with_rejections: 1,
            certified_source_bytes: 1,
            ..SourceBackedRefreshCurrent::default()
        },
        timings: SourceBackedRefreshTimings::default(),
        route_results: vec![result],
        zero_source_authority: Vec::new(),
        catalog_route_bindings: Vec::new(),
        verified_index: None,
    };

    let error = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        publication.generation_id.clone(),
        &publication,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("route rejections exceed"),
        "{error:#}"
    );
}

#[test]
fn terminal_receipt_rejects_omitted_success_failure_and_inconsistent_totals() {
    let (_temp, response, pin) = terminal_receipt_fixture();
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
        let error = published_refresh_receipt(&value, &pin)
            .expect_err("incomplete route-result partition must fail closed");
        assert!(
            format!("{error:#}").contains("invalid route-result partition"),
            "{error:#}"
        );
    }
}

#[test]
fn terminal_receipt_rejects_malformed_or_untyped_route_results() {
    let (_temp, response, pin) = terminal_receipt_fixture();
    for outcome in [
        json!(["s"]),
        json!(["s", false, "extra"]),
        json!(["f", "unknown", false, 1, []]),
        json!(["f", "u"]),
    ] {
        let mut malformed = response.clone();
        malformed["receipt"]["route_results"] = json!({ ("33".repeat(32)): outcome });
        malformed["receipt"]["selected_route_total"] = json!(1);
        let error = published_refresh_receipt(&malformed, &pin)
            .expect_err("malformed terminal result must fail closed");
        let detail = format!("{error:#}");
        assert!(
            detail.contains("route result") || detail.contains("failure class"),
            "{detail}"
        );
    }
}

#[test]
fn terminal_receipt_preserves_legitimate_empty_route_results() {
    let (_temp, response, pin) = terminal_receipt_fixture();
    let receipt = published_refresh_receipt(&response, &pin).unwrap();
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
        provider: "codex".to_owned(),
        source_selector: "/tmp/rollout.jsonl".to_owned(),
        line: 2,
        payload_type: "image_generation_end".to_owned(),
        class: "unsupported_record".to_owned(),
        detail: "unsupported record body was rejected".to_owned(),
    }];
    let publication = SourceBackedRefreshPublication {
        generation_id: "generation-with-rejection".to_owned(),
        published_explicit_source_catalog: None,
        unsupported_routes: 0,
        certified_source_count: 1,
        certified_source_bytes: 1,
        current: SourceBackedRefreshCurrent {
            source_count: 1,
            rejected_records: 1,
            sources_with_rejections: 1,
            certified_source_bytes: 1,
            ..SourceBackedRefreshCurrent::default()
        },
        timings: SourceBackedRefreshTimings::default(),
        route_results: vec![result],
        zero_source_authority: Vec::new(),
        catalog_route_bindings: Vec::new(),
        verified_index: None,
    };
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
    assert_eq!(wire["outcome"], "completed_with_rejections");
    assert_eq!(wire["route_results"][&route_identity][4], 1);
    assert_eq!(
        wire["route_results"][&route_identity][5][0],
        json!([
            source_identity,
            "codex",
            "/tmp/rollout.jsonl",
            2,
            "image_generation_end",
            "u",
            "unsupported record body was rejected"
        ])
    );
}

#[test]
fn terminal_response_is_bounded_while_route_results_remain_exact() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let mut publication = publish_pin_fixture(&execution, false)?;
            let route_results = (0..256)
                .map(|index| {
                    let route_identity = format!("{index:064x}");
                    let mut result = SourceBackedRefreshRouteResult::failed(
                        route_identity.clone(),
                        "unreadable".to_owned(),
                        false,
                    );
                    result.source_failures = vec![SourceBackedRefreshSourceFailure {
                        route_identity: result.route_identity.clone(),
                        source_identity: "cd".repeat(32),
                        provider: "opencode".to_owned(),
                        class: "unreadable".to_owned(),
                        carried_forward: false,
                        source_selector: "s".repeat(512),
                        detail: "d".repeat(512),
                    }];
                    result
                })
                .collect::<Vec<_>>();
            publication.route_results = route_results;
            Ok(publication)
        },
    ));
    coordinator.enqueue_periodic(&data_root).unwrap();
    let run = coordinator.run_next(&data_root).unwrap();
    assert!(!run.failed, "{:#}", run.job);
    assert!(
        serde_json::to_vec(&run.job).unwrap().len() < SOURCE_REFRESH_RESPONSE_MAX_BYTES as usize
    );
    let pin = pin_published_generation(&data_root).unwrap().unwrap();
    let receipt = published_refresh_receipt(&run.job, &pin).unwrap();
    assert_eq!(receipt.source_failure_total(), 256);
    assert_eq!(receipt.route_results.len(), 256);
    assert!(receipt.source_failure_diagnostic_count() < 256);
    let first_wire = receipt.to_json();
    assert_eq!(first_wire, receipt.to_json());
    assert!(
        serde_json::to_vec(&first_wire).unwrap().len() <= SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES
    );
}

#[test]
fn mismatched_request_overlay_is_not_recorded_as_verified() {
    let temp = tempfile::tempdir().unwrap();
    let requested = test_catalog_authority(1, 0x11);
    let published = test_catalog_authority(2, 0x22);
    let coordinator = CoreRefreshEngine::new();
    let response = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "explicit_source_catalog": requested.to_json(),
            }),
        )
        .unwrap()
        .unwrap();
    let request_id = request_id(&response);
    let run = coordinator
        .run_next_with(
            |_, _| {
                let mut publication = test_publication("catalog-generation");
                publication.published_explicit_source_catalog = Some(published);
                Ok(publication)
            },
            || Ok(Some("catalog-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(run.failed);
    assert!(coordinator.status(&request_id).unwrap()["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("different from the requested authority")));
}

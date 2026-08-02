use super::*;

fn empty_terminal_receipt_fixture() -> (tempfile::TempDir, Value, PinnedSourceBackedGeneration) {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let receipt = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .commit(|_| true)?;
            Ok(empty_test_publication(receipt.generation_id))
        },
    ));
    coordinator.enqueue_periodic(&data_root).unwrap();
    let run = coordinator.run_next(&data_root).expect("empty refresh");
    assert!(!run.failed);
    let pin = pin_published_generation(&data_root)
        .unwrap()
        .expect("empty published generation");
    (temp, run.job, pin)
}

#[test]
fn current_terminal_receipt_requires_compact_route_outcomes() {
    let (_temp, response, pin) = empty_terminal_receipt_fixture();

    for field in [
        "selected_route_total",
        "successful_route_total",
        "source_failures",
        "catalog_route_outcomes",
    ] {
        let mut missing = response.clone();
        assert!(missing["receipt"]
            .as_object_mut()
            .unwrap()
            .remove(field)
            .is_some());

        let error = published_refresh_receipt(&missing, &pin)
            .expect_err("missing current-protocol route outcome must fail");
        assert!(
            format!("{error:#}").contains(field),
            "unexpected error for {field}: {error:#}"
        );
    }
}

#[test]
fn current_terminal_receipt_rejects_malformed_route_outcome_arrays() {
    let (_temp, response, pin) = empty_terminal_receipt_fixture();
    let cases = [
        ("selected_route_total", json!(null)),
        ("successful_route_total", json!({})),
        ("source_failures", json!("none")),
        ("catalog_route_outcomes", json!(17)),
    ];

    for (field, malformed_value) in cases {
        let mut malformed = response.clone();
        malformed["receipt"][field] = malformed_value;

        let error = published_refresh_receipt(&malformed, &pin)
            .expect_err("malformed current-protocol route outcome array must fail");
        assert!(
            format!("{error:#}").contains(field),
            "unexpected error for {field}: {error:#}"
        );
    }
}

#[test]
fn current_terminal_receipt_rejects_inconsistent_route_outcome_partition() {
    let (_temp, mut response, pin) = empty_terminal_receipt_fixture();
    response["receipt"]["successful_route_total"] = json!(1);

    let error = published_refresh_receipt(&response, &pin)
        .expect_err("unselected successful route must fail partition validation");
    assert!(
        format!("{error:#}").contains("invalid route-result partition"),
        "unexpected partition error: {error:#}"
    );
}

#[test]
fn catalog_outcomes_must_match_global_route_partition() {
    let (_temp, response, pin) = empty_terminal_receipt_fixture();
    let lineage = "11".repeat(32);
    let route = "22".repeat(32);

    let mut impossible_success = response.clone();
    impossible_success["receipt"]["catalog_route_outcomes"] =
        json!({ (lineage.clone()): [route.clone(), "s", true] });
    let error = published_refresh_receipt(&impossible_success, &pin)
        .expect_err("catalog success cannot exceed successful route total");
    assert!(format!("{error:#}").contains("invalid route-result partition"));

    let mut conflicting_shared_route = response;
    conflicting_shared_route["receipt"]["catalog_route_outcomes"] = json!({
        (lineage): [route.clone(), "s", false],
        ("33".repeat(32)): [route, "f", "u"],
    });
    let error = published_refresh_receipt(&conflicting_shared_route, &pin)
        .expect_err("shared route cannot have conflicting catalog outcomes");
    assert!(format!("{error:#}").contains("disagree on a shared route result"));
}

#[test]
fn successful_catalog_outcome_preserves_route_local_change() {
    for changed in [false, true] {
        let (_temp, mut response, pin) = empty_terminal_receipt_fixture();
        let lineage = "11".repeat(32);
        let route = "22".repeat(32);
        response["receipt"]["selected_route_total"] = json!(1);
        response["receipt"]["successful_route_total"] = json!(1);
        response["receipt"]["catalog_route_outcomes"] = json!({ (lineage): [route, "s", changed] });

        let receipt = published_refresh_receipt(&response, &pin).unwrap();
        assert_eq!(receipt.catalog_route_outcomes.len(), 1);
        assert_eq!(receipt.catalog_route_outcomes[0].changed, Some(changed));
    }
}

#[test]
fn successful_catalog_outcome_requires_route_local_change() {
    let (_temp, mut response, pin) = empty_terminal_receipt_fixture();
    response["receipt"]["selected_route_total"] = json!(1);
    response["receipt"]["successful_route_total"] = json!(1);
    response["receipt"]["catalog_route_outcomes"] =
        json!({ ("11".repeat(32)): ["22".repeat(32), "s"] });

    let error = published_refresh_receipt(&response, &pin)
        .expect_err("successful compact outcome without changed fact must fail closed");
    let detail = format!("{error:#}");
    assert!(detail.contains("inconsistent fields"), "{detail}");
}

#[test]
fn catalog_outcome_rejects_trailing_compact_fields() {
    for outcome in [
        json!(["22".repeat(32), "s", false, "extra"]),
        json!(["22".repeat(32), "f", "u", "extra"]),
    ] {
        let (_temp, mut response, pin) = empty_terminal_receipt_fixture();
        response["receipt"]["selected_route_total"] = json!(1);
        response["receipt"]["catalog_route_outcomes"] = json!({ ("11".repeat(32)): outcome });

        let error = published_refresh_receipt(&response, &pin)
            .expect_err("compact catalog outcomes must reject trailing fields");
        let detail = format!("{error:#}");
        assert!(detail.contains("invalid width"), "{detail}");
    }
}

#[test]
fn current_terminal_receipt_preserves_legitimate_empty_route_outcomes() {
    let (_temp, response, pin) = empty_terminal_receipt_fixture();
    let receipt = published_refresh_receipt(&response, &pin)
        .expect("present empty outcomes describe a legitimate empty current receipt");

    assert_eq!(receipt.selected_route_total, 0);
    assert_eq!(receipt.successful_route_total, 0);
    assert!(receipt.selected_route_ids.is_empty());
    assert!(receipt.successful_route_ids.is_empty());
    assert!(receipt.catalog_route_outcomes.is_empty());
    assert!(receipt.source_failures.is_empty());
    assert_eq!(receipt.to_json()["outcome"], "completed");
}

#[test]
fn terminal_response_is_transport_bounded_and_preserves_exact_catalog_outcome() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let catalog_lineage = format!("{:064x}", 255);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let commit = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .commit(|_| true)?;
            let route_ids = (0..256)
                .map(|index| format!("{index:064x}"))
                .collect::<Vec<_>>();
            let failed_route_outcomes = route_ids
                .iter()
                .map(|route_identity| SourceBackedRefreshRouteFailure {
                    route_identity: route_identity.clone(),
                    source_identity: "cd".repeat(32),
                    provider: "opencode".to_owned(),
                    class: "unreadable".to_owned(),
                    carried_forward: false,
                })
                .collect();
            let source_failures = route_ids
                .iter()
                .map(|route_identity| SourceBackedRefreshSourceFailure {
                    route_identity: route_identity.clone(),
                    source_identity: "cd".repeat(32),
                    provider: "opencode".to_owned(),
                    class: "unreadable".to_owned(),
                    carried_forward: false,
                    source_selector: "s".repeat(512),
                    detail: "d".repeat(512),
                })
                .collect();
            Ok(SourceBackedRefreshPublication {
                generation_id: commit.generation_id,
                published_explicit_source_catalog: execution
                    .explicit_source_catalog
                    .cloned()
                    .expect("catalog authority"),
                scanned_routes: route_ids.len(),
                unsupported_routes: 0,
                certified_source_count: 0,
                certified_source_bytes: 0,
                current: SourceBackedRefreshCurrent::default(),
                timings: SourceBackedRefreshTimings::default(),
                selected_route_ids: route_ids.clone(),
                successful_route_ids: Vec::new(),
                successful_route_changes: Default::default(),
                failed_route_outcomes,
                catalog_route_outcomes: route_ids
                    .iter()
                    .enumerate()
                    .map(
                        |(index, route_identity)| SourceBackedRefreshCatalogRouteOutcome {
                            catalog_lineage: format!("{index:064x}"),
                            route_identity: route_identity.clone(),
                            outcome: "failed".to_owned(),
                            failure_class: Some("unreadable".to_owned()),
                            changed: None,
                        },
                    )
                    .collect(),
                source_failures,
            })
        },
    ));
    coordinator.enqueue_periodic(&data_root).unwrap();
    let run = coordinator.run_next(&data_root).expect("bounded refresh");
    assert!(!run.failed);
    let wire = serde_json::to_vec(&run.job).unwrap();
    assert!(wire.len() < SOURCE_REFRESH_RESPONSE_MAX_BYTES as usize);
    let pin = pin_published_generation(&data_root)
        .unwrap()
        .expect("published generation");
    let receipt = published_refresh_receipt(&run.job, &pin).unwrap();
    assert_eq!(receipt.source_failure_total(), 256);
    assert!(receipt.source_failures.len() < 256);
    assert_eq!(
        receipt.source_failures_omitted(),
        256 - receipt.source_failures.len()
    );
    assert_eq!(receipt.catalog_route_outcomes.len(), 256);
    assert!(receipt.catalog_route_outcomes.iter().any(|outcome| {
        outcome.catalog_lineage == catalog_lineage && outcome.outcome == "failed"
    }));
}

#[test]
fn explicit_catalog_request_retains_daemon_metadata_and_authority() {
    let temp = tempfile::tempdir().unwrap();
    let authority = load_explicit_source_catalog_authority(temp.path()).unwrap();
    let coordinator = CoreRefreshEngine::new();
    let periodic = coordinator.enqueue_periodic(temp.path()).unwrap();
    let response = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "explicit_source_catalog": authority.to_json(),
            }),
        )
        .unwrap()
        .expect("source refresh response");

    assert_eq!(request_id(&response), request_id(&periodic));
    assert_eq!(response["coalesced_requests"], 1);
    assert_eq!(response["owner"], "daemon");
    assert_eq!(response["trigger"], "import");
    assert_eq!(response["trigger_provenance"], "explicit_source_catalog");
    assert_eq!(
        ExplicitSourceCatalogAuthority::from_json(&response["requested_explicit_source_catalog"])
            .unwrap(),
        authority
    );

    let request_id = request_id(&response);
    let run = coordinator
        .run_next_with(
            |_, _| {
                let mut publication = test_publication("catalog-generation");
                publication.published_explicit_source_catalog = authority.clone();
                Ok(publication)
            },
            || Ok(Some("catalog-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(!run.failed);
    let published = coordinator.status(&request_id).unwrap();
    assert_eq!(published["request_state"], "published");
    assert_eq!(
        ExplicitSourceCatalogAuthority::from_json(&published["published_explicit_source_catalog"])
            .unwrap(),
        authority
    );
    assert_eq!(
        ExplicitSourceCatalogAuthority::from_json(
            &published["receipt"]["published_explicit_source_catalog"]
        )
        .unwrap(),
        authority
    );
}

#[test]
fn mismatched_catalog_publication_is_not_recorded_as_verified() {
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
        .expect("source refresh response");
    let request_id = request_id(&response);

    let run = coordinator
        .run_next_with(
            |_, _| {
                let mut publication = test_publication("catalog-generation");
                publication.published_explicit_source_catalog = published;
                Ok(publication)
            },
            || Ok(Some("catalog-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();

    assert!(run.failed);
    assert_eq!(run.job["request_state"], "failed");
    assert!(run.job.get("receipt").is_none());
    assert!(run.job.get("published_explicit_source_catalog").is_none());
    assert!(coordinator.status(&request_id).unwrap()["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("different from the requested authority")));
}

#[test]
fn nonempty_explicit_catalog_publication_is_recorded_in_the_terminal_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let source_path = temp.path().join("custom.jsonl");
    let records = [
        json!({
            "record_type": "manifest",
            "schema_version": "ctx-history-jsonl-v1",
        }),
        json!({
            "record_type": "source",
            "source_id": "catalog-source",
            "provider_key": "catalog-provider",
            "source_format": "catalog-jsonl",
        }),
        json!({
            "record_type": "session",
            "source_id": "catalog-source",
            "session_id": "catalog-session",
            "started_at": "2026-07-30T12:00:00Z",
        }),
        json!({
            "record_type": "event",
            "source_id": "catalog-source",
            "session_id": "catalog-session",
            "event_index": 0,
            "event_type": "message",
            "role": "user",
            "occurred_at": "2026-07-30T12:00:01Z",
            "payload": {"text": "catalog receipt dogfood"},
            "preview": "catalog receipt dogfood",
        }),
    ];
    fs::write(
        &source_path,
        records
            .into_iter()
            .map(|record| record.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let source = ProviderSource {
        provider: CaptureProvider::Custom,
        path: source_path,
        exists: true,
        source_format: "ctx_history_jsonl_v1",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let authority = crate::commands::import::upsert_explicit_source(&data_root, &source)
        .unwrap()
        .authority;
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());
    let publication = refresh_all_provider_sources(
        &discovery,
        DiscoveryReport {
            sources: Vec::new(),
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(publication.certified_source_count, 1);
    assert_eq!(publication.published_explicit_source_catalog, authority);

    let generation_id = publication.generation_id.clone();
    let coordinator = CoreRefreshEngine::new();
    coordinator.enqueue_periodic(&data_root).unwrap();
    let run = coordinator
        .run_next_with(
            |_, _| Ok(publication),
            || Ok(Some(generation_id.clone())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();

    assert!(!run.failed);
    assert_eq!(run.job["published_generation"], generation_id);
    assert_eq!(
        run.job["published_explicit_source_catalog"],
        authority.to_json()
    );
    assert_eq!(
        run.job["receipt"]["published_explicit_source_catalog"],
        authority.to_json()
    );
    write_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root), &run.job).unwrap();
    let status =
        crate::semantic::source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();
    assert_eq!(status.report["lexical"]["generation_id"], generation_id);
    assert_eq!(status.report["catalog"]["status"], "ready");
    assert_eq!(
        status.report["catalog"]["published_authority_present"],
        true
    );
}

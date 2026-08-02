use super::*;

fn source_failure(index: usize) -> SourceBackedRefreshSourceFailure {
    SourceBackedRefreshSourceFailure {
        source_identity: format!("{index:064x}"),
        provider: "codex".to_owned(),
        class: SourceBackedRefreshSourceFailureClass::SourceChanged,
        carried_forward: true,
        source_selector: format!("/history/{index}.jsonl"),
        detail: "source changed during refresh".to_owned(),
    }
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
        &mut progress,
    )
    .unwrap();
    assert_eq!(publication.certified_source_count, 1);
    assert_eq!(publication.scanned_routes, 1);
    assert_eq!(publication.successful_routes, 1);
    assert!(publication.source_failures.is_empty());
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
    let pin = pin_published_generation(&data_root).unwrap().unwrap();
    let parsed = published_refresh_receipt(&run.job, &pin).unwrap();
    assert_eq!(parsed.scanned_routes, 1);
    assert_eq!(parsed.successful_routes, 1);
    assert!(parsed.source_failures.is_empty());

    let mut partial = run.job.clone();
    let failure = source_failure(7).to_json();
    partial["outcome"] = json!("completed_with_source_failures");
    partial["successful_routes"] = json!(0);
    partial["source_failure_total"] = json!(1);
    partial["source_failures_omitted"] = json!(0);
    partial["receipt"]["outcome"] = json!("completed_with_source_failures");
    partial["receipt"]["successful_routes"] = json!(0);
    partial["receipt"]["source_failures"] = json!({
        "failures": [failure],
        "omitted": 0,
        "total": 1,
    });
    let partial_receipt = published_refresh_receipt(&partial, &pin).unwrap();
    assert_eq!(
        partial_receipt.terminal_outcome(),
        "completed_with_source_failures"
    );
    assert_eq!(partial_receipt.successful_routes, 0);
    assert_eq!(partial_receipt.source_failures.total(), 1);

    let mut inconsistent = partial;
    inconsistent["receipt"]["successful_routes"] = json!(1);
    assert!(published_refresh_receipt(&inconsistent, &pin)
        .unwrap_err()
        .to_string()
        .contains("result counts are inconsistent"));
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

#[test]
fn terminal_receipt_serializes_partial_source_results_without_route_identities() {
    let coordinator = CoreRefreshEngine::new();
    coordinator.enqueue_for_test(None);
    let mut publication = test_publication("partial-generation");
    publication.scanned_routes = 2;
    publication.successful_routes = 1;
    publication.source_failures = SourceBackedRefreshSourceFailures {
        failures: vec![source_failure(1)],
        omitted: 0,
    };

    let run = coordinator
        .run_next_with(
            |_, _| Ok(publication),
            || Ok(Some("partial-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(!run.failed);
    assert_eq!(run.job["outcome"], "completed_with_source_failures");
    assert_eq!(run.job["scanned_routes"], 2);
    assert_eq!(run.job["successful_routes"], 1);
    assert_eq!(run.job["source_failure_total"], 1);
    assert_eq!(run.job["source_failures_omitted"], 0);
    assert_eq!(run.job["receipt"]["source_failures"]["total"], 1);
    assert_eq!(
        run.job["receipt"]["source_failures"]["failures"][0]["source_selector"],
        "/history/1.jsonl"
    );
    assert!(!run.job.to_string().contains("route_identity"));
}

#[test]
fn source_failure_parser_enforces_hash_class_bounds_and_totals() {
    let valid = json!({
        "failures": [{
            "source_identity": "01".repeat(32),
            "provider": "codex",
            "class": "source_changed",
            "carried_forward": true,
            "source_selector": "é".repeat(256),
            "detail": "d".repeat(SOURCE_REFRESH_FAILURE_TEXT_MAX_BYTES),
        }],
        "omitted": 2,
        "total": 3,
    });
    let parsed = required_source_failures(Some(&valid)).unwrap();
    assert_eq!(parsed.total(), 3);
    assert_eq!(parsed.failures.len(), 1);
    assert_eq!(parsed.omitted, 2);

    for (class, expected) in [
        (
            "unavailable",
            SourceBackedRefreshSourceFailureClass::Unavailable,
        ),
        (
            "source_changed",
            SourceBackedRefreshSourceFailureClass::SourceChanged,
        ),
        (
            "unreadable",
            SourceBackedRefreshSourceFailureClass::Unreadable,
        ),
        (
            "incompatible",
            SourceBackedRefreshSourceFailureClass::Incompatible,
        ),
    ] {
        let mut accepted = valid.clone();
        accepted["failures"][0]["class"] = json!(class);
        assert_eq!(
            required_source_failures(Some(&accepted)).unwrap().failures[0].class,
            expected
        );
    }

    let mut invalid_hash = valid.clone();
    invalid_hash["failures"][0]["source_identity"] = json!("AB".repeat(32));
    assert!(required_source_failures(Some(&invalid_hash))
        .unwrap_err()
        .to_string()
        .contains("identity is malformed"));

    let mut invalid_class = valid.clone();
    invalid_class["failures"][0]["class"] = json!("retryable");
    assert!(required_source_failures(Some(&invalid_class))
        .unwrap_err()
        .to_string()
        .contains("class is malformed"));

    let mut invalid_provider = valid.clone();
    invalid_provider["failures"][0]["provider"] = json!("not-a-provider");
    assert!(required_source_failures(Some(&invalid_provider))
        .unwrap_err()
        .to_string()
        .contains("provider is malformed"));

    let mut oversized_selector = valid.clone();
    oversized_selector["failures"][0]["source_selector"] = json!("é".repeat(257));
    assert!(required_source_failures(Some(&oversized_selector))
        .unwrap_err()
        .to_string()
        .contains("source_selector is too large"));

    let mut oversized_detail = valid.clone();
    oversized_detail["failures"][0]["detail"] =
        json!("d".repeat(SOURCE_REFRESH_FAILURE_TEXT_MAX_BYTES + 1));
    assert!(required_source_failures(Some(&oversized_detail))
        .unwrap_err()
        .to_string()
        .contains("detail is too large"));

    let mut inconsistent_total = valid.clone();
    inconsistent_total["total"] = json!(4);
    assert!(required_source_failures(Some(&inconsistent_total))
        .unwrap_err()
        .to_string()
        .contains("inconsistent source failure totals"));

    let rows = (0..=SOURCE_REFRESH_FAILURE_ROW_LIMIT)
        .map(|index| source_failure(index).to_json())
        .collect::<Vec<_>>();
    let too_many = json!({
        "failures": rows,
        "omitted": 0,
        "total": SOURCE_REFRESH_FAILURE_ROW_LIMIT + 1,
    });
    assert!(required_source_failures(Some(&too_many))
        .unwrap_err()
        .to_string()
        .contains("too many source failure rows"));
}

#[test]
fn source_result_partition_accepts_retained_history_and_rejects_no_usable_source() {
    let failures = SourceBackedRefreshSourceFailures {
        failures: vec![source_failure(1)],
        omitted: 2,
    };
    validate_source_refresh_results(3, 0, &failures, 1).unwrap();
    assert!(validate_source_refresh_results(4, 0, &failures, 1)
        .unwrap_err()
        .to_string()
        .contains("result counts are inconsistent"));
    assert!(validate_source_refresh_results(3, 0, &failures, 0)
        .unwrap_err()
        .to_string()
        .contains("no usable source remains"));
}

#[test]
fn maximum_source_failure_receipt_fits_the_bounded_ipc_response() {
    let coordinator = CoreRefreshEngine::new();
    let queued = coordinator.enqueue_for_test(None);
    let request_id = request_id(&queued);
    let mut publication = test_publication("maximum-wire-generation");
    publication.scanned_routes = SOURCE_REFRESH_FAILURE_ROW_LIMIT;
    publication.successful_routes = 0;
    publication.source_failures = SourceBackedRefreshSourceFailures {
        failures: (0..SOURCE_REFRESH_FAILURE_ROW_LIMIT)
            .map(|index| {
                let mut failure = source_failure(index);
                failure.source_selector = "\0".repeat(SOURCE_REFRESH_FAILURE_TEXT_MAX_BYTES);
                failure.detail = "\0".repeat(SOURCE_REFRESH_FAILURE_TEXT_MAX_BYTES);
                failure
            })
            .collect(),
        omitted: 0,
    };

    let run = coordinator
        .run_next_with(
            |_, _| Ok(publication),
            || Ok(Some("maximum-wire-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(!run.failed);
    let response = coordinator.status(&request_id).unwrap();
    let serialized = serde_json::to_vec(&response).unwrap();
    assert!(serialized.len() > 384 * 1024, "{}", serialized.len());
    assert!(
        serialized.len() <= SOURCE_REFRESH_RESPONSE_MAX_BYTES as usize,
        "maximum receipt serialized to {} bytes, over the {}-byte IPC cap",
        serialized.len(),
        SOURCE_REFRESH_RESPONSE_MAX_BYTES
    );
}

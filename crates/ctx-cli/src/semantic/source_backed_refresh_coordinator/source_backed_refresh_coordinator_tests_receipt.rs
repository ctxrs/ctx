use super::*;

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
    let mut progress = |_: CaptureSourceBackedRefreshProgress| Ok::<(), SourceBackedRouteError>(());
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

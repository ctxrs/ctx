use super::*;

#[test]
fn missing_codex_catalog_source_is_temporarily_unavailable_until_refresh() {
    let failure = codex_locator_hydration_failure(CodexSourceBackedErrorV0::LocatorSourceNotFound(
        "session-1".to_owned(),
    ));

    assert_eq!(failure.kind, HydrationFailureKind::TemporarilyUnavailable);
    assert!(failure.detail.contains("session-1"));
}

#[test]
fn heterogeneous_routes_publish_once_and_hydrate_exact_locators() {
    let gemini = fixture_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        1,
        NativeRecordCoordinate::Jsonl {
            byte_offset: 10,
            byte_length: 4,
            physical_ordinal: 1,
            native_session_key: None,
            native_event_key: None,
        },
        b"gemini".to_vec(),
    );
    let hermes = fixture_route(
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
        2,
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: "messages".to_owned(),
            primary_key: TypedKey::I64(7),
            row_version: None,
        },
        b"hermes".to_vec(),
    );
    let gemini_request = gemini.1.clone();
    let hermes_request = hermes.1.clone();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(gemini.0);
    registry.register(hermes.0);

    let temp = tempdir().unwrap();
    let mut progress = Vec::new();
    let receipt = refresh_source_backed_generation_with_progress(
        temp.path(),
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
        |update| {
            progress.push(update);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(receipt.scanned_routes, 2);
    assert_eq!(receipt.commit.indexed_documents, 2);
    assert_eq!(receipt.commit.certified_sources, 2);
    assert_eq!(receipt.certified_source_count, 2);
    assert_eq!(receipt.certified_source_bytes, 2);
    assert_eq!(receipt.sources.len(), 2);
    assert!(receipt.removals.is_empty());
    assert!(receipt.scan_stage_duration > Duration::ZERO);
    assert!(receipt.commit_duration > Duration::ZERO);
    assert!(progress
        .windows(2)
        .all(|pair| pair[0].elapsed <= pair[1].elapsed));
    let committed = progress.last().unwrap();
    assert_eq!(committed.phase, "committed");
    assert_eq!(committed.certified_source_count, Some(2));
    assert_eq!(committed.certified_source_bytes, Some(2));
    assert!(committed.stage_duration > Duration::ZERO);

    let resolver = registry.resolver_registry();
    assert_eq!(
        resolver
            .hydrate_event(&gemini_request)
            .unwrap()
            .provider_bytes,
        b"gemini"
    );
    assert_eq!(
        resolver
            .hydrate_event(&hermes_request)
            .unwrap()
            .provider_bytes,
        b"hermes"
    );
}

#[test]
fn ordered_batch_groups_by_route_and_exact_source_without_store() {
    let gemini_a = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 31);
    let gemini_b = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 32);
    let hermes = fixture_source(CaptureProvider::Hermes, "hermes_state_sqlite", 33);
    let requests = vec![
        fixture_event_request(&gemini_a, "gemini-a-1"),
        fixture_event_request(&hermes, "hermes-1"),
        fixture_event_request(&gemini_b, "gemini-b-1"),
        fixture_event_request(&gemini_a, "gemini-a-2"),
        fixture_event_request(&hermes, "hermes-2"),
    ];
    let expected_order = requests
        .iter()
        .map(EventHydrationRequest::event_id)
        .collect::<Vec<_>>();

    let gemini_batch_sources = Arc::new(Mutex::new(Vec::<[u8; 32]>::new()));
    let gemini_event_calls = Arc::new(AtomicUsize::new(0));
    let gemini_owned_sources = Arc::new(vec![gemini_a.clone(), gemini_b.clone()]);
    let gemini_driver = SourceBackedRouteDriver::new(
        |_sink| Ok(()),
        {
            let owned_sources = Arc::clone(&gemini_owned_sources);
            move |candidate| {
                owned_sources
                    .iter()
                    .any(|source| source.exact_descriptor_eq(candidate))
            }
        },
        |_target| false,
        {
            let event_calls = Arc::clone(&gemini_event_calls);
            move |request| {
                event_calls.fetch_add(1, Ordering::SeqCst);
                Ok(fixture_hydrated_record(request))
            }
        },
    )
    .with_batch_hydration({
        let batch_sources = Arc::clone(&gemini_batch_sources);
        move |request| {
            let source = request.events()[0].locator().source();
            assert!(request
                .events()
                .iter()
                .all(|event| source.exact_descriptor_eq(event.locator().source())));
            batch_sources
                .lock()
                .unwrap()
                .push(source.exact_descriptor_digest());
            Ok(fixture_batch_result(request))
        }
    });

    let hermes_batch_sources = Arc::new(Mutex::new(Vec::<[u8; 32]>::new()));
    let hermes_event_calls = Arc::new(AtomicUsize::new(0));
    let hermes_owned_source = hermes.clone();
    let hermes_driver = SourceBackedRouteDriver::new(
        |_sink| Ok(()),
        move |candidate| hermes_owned_source.exact_descriptor_eq(candidate),
        |_target| false,
        {
            let event_calls = Arc::clone(&hermes_event_calls);
            move |request| {
                event_calls.fetch_add(1, Ordering::SeqCst);
                Ok(fixture_hydrated_record(request))
            }
        },
    )
    .with_batch_hydration({
        let batch_sources = Arc::clone(&hermes_batch_sources);
        move |request| {
            let source = request.events()[0].locator().source();
            batch_sources
                .lock()
                .unwrap()
                .push(source.exact_descriptor_digest());
            Ok(fixture_batch_result(request))
        }
    });

    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_executable_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        gemini_driver,
    ));
    registry.register(fixture_executable_route(
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
        hermes_driver,
    ));

    let request = BatchHydrationRequest::new(requests).unwrap();
    let result = registry
        .resolver_registry()
        .hydrate_batch(&request)
        .unwrap();
    assert_eq!(
        result
            .records()
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        expected_order
    );
    let mut observed_gemini = gemini_batch_sources.lock().unwrap().clone();
    observed_gemini.sort();
    let mut expected_gemini = vec![
        gemini_a.exact_descriptor_digest(),
        gemini_b.exact_descriptor_digest(),
    ];
    expected_gemini.sort();
    assert_eq!(observed_gemini, expected_gemini);
    assert_eq!(
        hermes_batch_sources.lock().unwrap().as_slice(),
        &[hermes.exact_descriptor_digest()]
    );
    assert_eq!(gemini_event_calls.load(Ordering::SeqCst), 0);
    assert_eq!(hermes_event_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn registry_session_hydration_uses_one_native_batch_callback() {
    let source = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 34);
    let events = vec![
        fixture_event_request(&source, "first"),
        fixture_event_request(&source, "second"),
        fixture_event_request(&source, "third"),
    ];
    let session_request =
        SessionHydrationRequest::new(fixture_session_id(&source), events).unwrap();
    let event_calls = Arc::new(AtomicUsize::new(0));
    let batch_calls = Arc::new(AtomicUsize::new(0));
    let owned_source = source.clone();
    let driver = SourceBackedRouteDriver::new(
        |_sink| Ok(()),
        move |candidate| owned_source.exact_descriptor_eq(candidate),
        |_target| false,
        {
            let event_calls = Arc::clone(&event_calls);
            move |request| {
                event_calls.fetch_add(1, Ordering::SeqCst);
                Ok(fixture_hydrated_record(request))
            }
        },
    )
    .with_batch_hydration({
        let batch_calls = Arc::clone(&batch_calls);
        move |request| {
            batch_calls.fetch_add(1, Ordering::SeqCst);
            Ok(fixture_batch_result(request))
        }
    });
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_executable_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        driver,
    ));

    let result = registry
        .resolver_registry()
        .hydrate_session(&session_request)
        .unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(batch_calls.load(Ordering::SeqCst), 1);
    assert_eq!(event_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn registry_default_batch_driver_preserves_event_loop_order() {
    let source = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 35);
    let events = vec![
        fixture_event_request(&source, "first"),
        fixture_event_request(&source, "second"),
    ];
    let expected = events
        .iter()
        .map(EventHydrationRequest::event_id)
        .collect::<Vec<_>>();
    let event_calls = Arc::new(AtomicUsize::new(0));
    let owned_source = source.clone();
    let driver = SourceBackedRouteDriver::new(
        |_sink| Ok(()),
        move |candidate| owned_source.exact_descriptor_eq(candidate),
        |_target| false,
        {
            let event_calls = Arc::clone(&event_calls);
            move |request| {
                event_calls.fetch_add(1, Ordering::SeqCst);
                Ok(fixture_hydrated_record(request))
            }
        },
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_executable_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        driver,
    ));

    let request = BatchHydrationRequest::new(events).unwrap();
    let result = registry
        .resolver_registry()
        .hydrate_batch(&request)
        .unwrap();
    assert_eq!(
        result
            .records()
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(event_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn registry_rejects_malformed_native_batch_results() {
    let source = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 36);
    let events = vec![
        fixture_event_request(&source, "first"),
        fixture_event_request(&source, "second"),
    ];
    let request = BatchHydrationRequest::new(events.clone()).unwrap();

    let short = fixture_batch_resolver(&source, |_request| {
        Ok(BatchHydrationResult::new(Vec::new()).unwrap())
    })
    .hydrate_batch(&request)
    .unwrap_err();
    assert_eq!(short.kind, HydrationFailureKind::InvalidLocator);
    assert!(short.detail.contains("returned 0 records"));

    let duplicate_record = fixture_hydrated_record(&events[0]);
    let duplicate = fixture_batch_resolver(&source, move |_request| {
        Ok(
            BatchHydrationResult::new(vec![duplicate_record.clone(), duplicate_record.clone()])
                .unwrap(),
        )
    })
    .hydrate_batch(&request)
    .unwrap_err();
    assert_eq!(duplicate.kind, HydrationFailureKind::InvalidLocator);
    assert!(duplicate.detail.contains("duplicate event identity"));

    let unrequested = fixture_hydrated_record(&fixture_event_request(&source, "unrequested"));
    let wrong = fixture_batch_resolver(&source, move |request| {
        Ok(BatchHydrationResult::new(vec![
            fixture_hydrated_record(&request.events()[0]),
            unrequested.clone(),
        ])
        .unwrap())
    })
    .hydrate_batch(&request)
    .unwrap_err();
    assert_eq!(wrong.kind, HydrationFailureKind::InvalidLocator);
    assert!(wrong.detail.contains("unrequested event identity"));

    let unordered = fixture_batch_resolver(&source, |request| {
        Ok(BatchHydrationResult::new(
            request
                .events()
                .iter()
                .rev()
                .map(fixture_hydrated_record)
                .collect(),
        )
        .unwrap())
    })
    .hydrate_batch(&request)
    .unwrap_err();
    assert_eq!(unordered.kind, HydrationFailureKind::InvalidLocator);
    assert!(unordered.detail.contains("exact request order"));
}

#[test]
fn registry_returns_exact_failure_instead_of_partial_batch_success() {
    let first_source = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 37);
    let second_source = fixture_source(CaptureProvider::Hermes, "hermes_state_sqlite", 38);
    let request = BatchHydrationRequest::new(vec![
        fixture_event_request(&first_source, "first"),
        fixture_event_request(&second_source, "second"),
    ])
    .unwrap();
    let successful_calls = Arc::new(AtomicUsize::new(0));
    let failed_calls = Arc::new(AtomicUsize::new(0));
    let first_owned_source = first_source.clone();
    let first_driver = SourceBackedRouteDriver::new(
        |_sink| Ok(()),
        move |candidate| first_owned_source.exact_descriptor_eq(candidate),
        |_target| false,
        |request| Ok(fixture_hydrated_record(request)),
    )
    .with_batch_hydration({
        let successful_calls = Arc::clone(&successful_calls);
        move |request| {
            successful_calls.fetch_add(1, Ordering::SeqCst);
            Ok(fixture_batch_result(request))
        }
    });
    let failure = HydrationFailure {
        kind: HydrationFailureKind::StaleSourceEvidence,
        detail: "exact native source failure".to_owned(),
    };
    let expected_failure = failure.clone();
    let second_owned_source = second_source.clone();
    let second_driver = SourceBackedRouteDriver::new(
        |_sink| Ok(()),
        move |candidate| second_owned_source.exact_descriptor_eq(candidate),
        |_target| false,
        |request| Ok(fixture_hydrated_record(request)),
    )
    .with_batch_hydration({
        let failed_calls = Arc::clone(&failed_calls);
        move |_request| {
            failed_calls.fetch_add(1, Ordering::SeqCst);
            Err(failure.clone())
        }
    });
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_executable_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        first_driver,
    ));
    registry.register(fixture_executable_route(
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
        second_driver,
    ));

    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_batch(&request)
            .unwrap_err(),
        expected_failure
    );
    assert_eq!(successful_calls.load(Ordering::SeqCst), 1);
    assert_eq!(failed_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn registry_rejects_duplicate_routes_before_batch_callbacks() {
    let source = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 39);
    let request =
        BatchHydrationRequest::new(vec![fixture_event_request(&source, "event")]).unwrap();
    let batch_calls = Arc::new(AtomicUsize::new(0));
    let owned_source = source.clone();
    let driver = SourceBackedRouteDriver::new(
        |_sink| Ok(()),
        move |candidate| owned_source.exact_descriptor_eq(candidate),
        |_target| false,
        |request| Ok(fixture_hydrated_record(request)),
    )
    .with_batch_hydration({
        let batch_calls = Arc::clone(&batch_calls);
        move |request| {
            batch_calls.fetch_add(1, Ordering::SeqCst);
            Ok(fixture_batch_result(request))
        }
    });
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_executable_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        driver.clone(),
    ));
    registry.register(fixture_executable_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        driver,
    ));

    let failure = registry
        .resolver_registry()
        .hydrate_batch(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);
    assert!(failure.detail.contains("more than one provider route"));
    assert_eq!(batch_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn central_hydration_returns_decoded_event_text_not_serialized_containers() {
    let codex = CodexHydratedRecordV0 {
        provider_bytes: br#"{"type":"event_msg","payload":{"message":"raw"}}"#.to_vec(),
        decoded_display_text: Some("decoded Codex text".to_owned()),
    };
    assert_eq!(
        codex_display_bytes(codex).unwrap(),
        b"decoded Codex text".to_vec()
    );
    let missing = codex_display_bytes(CodexHydratedRecordV0 {
        provider_bytes: b"raw".to_vec(),
        decoded_display_text: None,
    })
    .unwrap_err();
    assert_eq!(
        missing.kind,
        HydrationFailureKind::UnsupportedParserRevision
    );

    let firebender = br#"[
            {"role":"user","content":"first"},
            {"role":"assistant","content":{"text":"decoded Firebender text"}}
        ]"#;
    assert_eq!(
        firebender_display_bytes(firebender, 1).unwrap(),
        b"decoded Firebender text".to_vec()
    );
}

#[test]
fn unsupported_detected_format_stays_typed_and_never_claims_a_locator() {
    let source = fixture_provider_source(
        CaptureProvider::Unknown,
        "unknown_detected_format",
        ProviderImportSupport::Unsupported,
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(SourceBackedRoute::unsupported(
        source,
        "no product-approved source-backed adapter",
    ));
    assert!(matches!(
        refresh_source_backed_generation(
            tempdir().unwrap().path(),
            &registry,
            WriterOptions::default()
        ),
        Err(SourceBackedCoordinatorError::NoExecutableRoutes)
    ));
}

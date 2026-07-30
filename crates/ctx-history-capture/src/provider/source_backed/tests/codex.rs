use super::*;

#[test]
fn codex_history_and_sessions_publish_one_fresh_generation_and_hydrate_exactly() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let sessions = home.join(".codex/sessions");
    let history = home.join(".codex/history.jsonl");
    fs::create_dir_all(&sessions).unwrap();

    let native_session_id = "019faadb-b9f2-7413-9fab-edf59fd787a6";
    let session_meta = serde_json::json!({
        "timestamp": "2026-07-28T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": native_session_id,
            "timestamp": "2026-07-28T12:00:00Z",
            "cwd": "/tmp/source-backed",
            "originator": "codex_cli_rs",
            "cli_version": "0.1.0",
            "source": "cli",
            "model_provider": "openai"
        }
    });
    let session_message = serde_json::json!({
        "timestamp": "2026-07-28T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "ordinary Codex session remains present"
            }]
        }
    });
    let session_bytes = format!("{session_meta}\n{session_message}\n").into_bytes();
    fs::write(
        sessions.join(format!("rollout-{native_session_id}.jsonl")),
        &session_bytes,
    )
    .unwrap();

    let prompt_tail = "full-body-tail-marker";
    let prompt_text = format!("fresh v0.26 prompt {} {prompt_tail}", "x".repeat(8_192));
    let mut prompt_bytes = serde_json::to_vec(&serde_json::json!({
        "session_id": native_session_id,
        "ts": 1_785_139_200,
        "text": prompt_text.clone(),
    }))
    .unwrap();
    prompt_bytes.push(b'\n');
    fs::write(&history, &prompt_bytes).unwrap();

    let context = DiscoveryContext::new(
        &home,
        temp.path().join("cwd"),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let sources = vec![
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_history_jsonl",
            ProviderImportSupport::Native,
            &history,
        ),
    ];
    let data_root = temp.path().join("ctx-data");
    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &data_root,
        sources,
        Vec::new(),
    );
    assert_eq!(build.executable_route_count(), 2);
    assert_eq!(build.unsupported_route_count(), 0);
    assert!(build.issues.is_empty());

    let prompt_input = CodexPromptHistorySourceBackedInputV0::explicit(
        &history,
        CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0,
    );
    let prompt_source =
        observe_codex_prompt_history_source_backed_explicit_v0(&prompt_input).unwrap();
    let mut prompt_documents = Vec::new();
    scan_codex_prompt_history_source_backed_v0(prompt_source, None, |page| {
        prompt_documents.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(prompt_documents.len(), 1);
    assert_eq!(prompt_documents[0].body, prompt_text);
    assert!(prompt_documents[0].body.ends_with(prompt_tail));
    let event_request = EventHydrationRequest::new(
        prompt_documents[0].event_id,
        prompt_documents[0].locator.clone(),
    )
    .unwrap();
    let session_request =
        SessionHydrationRequest::new(prompt_documents[0].session_id, vec![event_request.clone()])
            .unwrap();
    let resolver = build.registry.resolver_registry();
    assert_eq!(
        resolver
            .hydrate_event(&event_request)
            .unwrap()
            .provider_bytes,
        prompt_text.as_bytes()
    );
    assert_eq!(
        resolver
            .hydrate_session(&session_request)
            .unwrap()
            .into_iter()
            .map(|record| record.provider_bytes)
            .collect::<Vec<_>>(),
        vec![prompt_text.clone().into_bytes()]
    );

    let index = temp.path().join("index");
    let mut progress = Vec::new();
    let receipt = refresh_source_backed_generation_with_progress(
        &index,
        &build.registry,
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
    assert!(receipt.unsupported_routes.is_empty());
    assert_eq!(receipt.certified_source_count, 2);
    assert_eq!(
        receipt.certified_source_count,
        receipt.commit.certified_sources
    );
    assert_eq!(
        receipt.certified_source_bytes,
        receipt.commit.certified_source_bytes
    );
    assert!(receipt.scan_stage_duration > Duration::ZERO);
    assert!(receipt.commit_duration > Duration::ZERO);
    let committed = progress.last().unwrap();
    assert_eq!(
        committed.certified_source_count,
        Some(receipt.certified_source_count)
    );
    assert_eq!(
        committed.certified_source_bytes,
        Some(receipt.certified_source_bytes)
    );

    let verified = VerifiedIndex::open(&index).unwrap();
    let sources = &verified.manifest().sources;
    assert_eq!(sources.len(), 2);
    let source_formats = sources
        .iter()
        .map(|source| source.observation().source().source_format())
        .collect::<HashSet<_>>();
    assert_eq!(
        source_formats,
        HashSet::from(["codex_history_jsonl", "codex_session_jsonl"])
    );
    let history_certificate = sources
        .iter()
        .find(|source| source.observation().source().source_format() == "codex_history_jsonl")
        .unwrap();
    assert_eq!(
        history_certificate.counts().certified_bytes,
        u64::try_from(prompt_bytes.len()).unwrap()
    );
    assert_eq!(
        receipt.certified_source_bytes,
        sources
            .iter()
            .map(|source| source.counts().certified_bytes)
            .sum::<u64>()
    );

    let before_generation = verified.generation_id().to_owned();
    let before_opstamp = receipt.commit.opstamp;
    let before_meta = fs::read(index.join("meta.json")).unwrap();
    let before_segments = verified.validate_checksums().unwrap();
    drop(verified);
    let replay = refresh_source_backed_generation(
        &index,
        &build.registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    let replay_index = VerifiedIndex::open(&index).unwrap();
    assert_eq!(replay.commit.generation_id, before_generation);
    assert_eq!(replay.commit.opstamp, before_opstamp);
    assert_eq!(fs::read(index.join("meta.json")).unwrap(), before_meta);
    assert_eq!(replay_index.validate_checksums().unwrap(), before_segments);

    let appended_prompt = "Codex prompt history append-only registration sentinel";
    let mut appended_line = serde_json::to_vec(&serde_json::json!({
        "session_id": native_session_id,
        "ts": 1_785_139_201,
        "text": appended_prompt,
    }))
    .unwrap();
    appended_line.push(b'\n');
    prompt_bytes.extend(appended_line);
    fs::write(&history, &prompt_bytes).unwrap();
    let appended_build = build_automatic_source_backed_registry_from_parts(
        &context,
        &data_root,
        vec![
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &sessions,
            ),
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_history_jsonl",
                ProviderImportSupport::Native,
                &history,
            ),
        ],
        Vec::new(),
    );
    assert!(appended_build.issues.is_empty());
    let appended = refresh_source_backed_generation(
        &index,
        &appended_build.registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert!(appended.commit.opstamp > before_opstamp);
    let appended_index = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        appended_index
            .search_event_candidates("registration sentinel", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(appended_index.document_count(), 3);
}

#[test]
fn explicit_codex_session_route_is_exact_replay_stable_and_typed_for_hydration() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let selected = sessions.join("selected.jsonl");
    let sibling = sessions.join("sibling.jsonl");
    fs::create_dir_all(&sessions).unwrap();

    let native_session_id = "019facf0-1111-7777-8888-000000000001";
    let sibling_session_id = "019facf0-2222-7777-8888-000000000002";
    let selected_first = format!(
        "selected full lexical body {} selectedtaillexeme",
        "meaningful ".repeat(1_024)
    );
    let selected_second = "selected second exact message".to_owned();
    let selected_bytes =
        codex_rollout_bytes(native_session_id, &[&selected_first, &selected_second]);
    let sibling_bytes = codex_rollout_bytes(sibling_session_id, &["siblingleakagesentinel"]);
    fs::write(&selected, &selected_bytes).unwrap();
    fs::write(&sibling, &sibling_bytes).unwrap();

    let route_metadata =
        landed_format_route(CaptureProvider::Codex, "codex_session_jsonl").unwrap();
    assert_eq!(
        route_metadata.exact_hydration,
        SourceBackedHydrationSupport::Full
    );
    assert_eq!(
        route_metadata.selector_authority,
        SourceBackedSelectorAuthority::ExplicitPath
    );
    assert!(!route_metadata.automatic);
    assert!(route_metadata.explicit_manual);
    assert!(route_metadata.unsupported_reason.is_none());

    let explicit_source = fixture_provider_source_at(
        CaptureProvider::Codex,
        "codex_session_jsonl",
        ProviderImportSupport::Explicit,
        &selected,
    );
    let mut explicit_registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut explicit_registry,
        explicit_source,
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    let registered = explicit_registry.routes().next().unwrap();
    assert_eq!(
        registered.selection,
        Some(SourceBackedRouteSelection::ExplicitManual)
    );
    assert_eq!(
        registered.selector_authority,
        SourceBackedSelectorAuthority::ExplicitPath
    );
    assert!(registered.unsupported_reason.is_none());

    let explicit_index_root = temp.path().join("explicit-index");
    let cold = refresh_source_backed_generation(
        &explicit_index_root,
        &explicit_registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(cold.scanned_routes, 1);
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.commit.indexed_documents, 2);
    assert!(cold.removals.is_empty());
    let selected_source = cold.sources[0].observation().source().clone();
    assert_eq!(selected_source.source_format(), "codex_session_jsonl");

    let cold_index = VerifiedIndex::open(&explicit_index_root).unwrap();
    assert_eq!(cold_index.document_count(), 2);
    assert_eq!(
        cold_index
            .search_event_candidates("selectedtaillexeme", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(cold_index
        .search_event_candidates("siblingleakagesentinel", 10)
        .unwrap()
        .is_empty());
    let cold_events = cold_index
        .source_event_page(&selected_source, None, 10)
        .unwrap()
        .items;
    assert_eq!(cold_events.len(), 2);
    assert!(cold_events.iter().all(|event| {
        event.source_path.as_deref() == selected.to_str()
            && event.provider_session_id.as_deref() == Some(native_session_id)
    }));
    for event in &cold_events {
        assert_eq!(
            event.locator.revision_policy(),
            LocatorRevisionPolicy::StableRecordEvidence
        );
        assert!(event.locator.certified_source_revision_digest().is_none());
        let NativeRecordCoordinate::Jsonl {
            byte_length,
            physical_ordinal,
            native_session_key,
            native_event_key,
            ..
        } = event.locator.coordinate()
        else {
            panic!("explicit Codex route emitted a non-JSONL locator");
        };
        assert_ne!(*byte_length, 0);
        assert_eq!(*physical_ordinal, event.event_sequence);
        assert_eq!(
            native_session_key,
            &Some(TypedKey::Utf8(native_session_id.to_owned()))
        );
        assert_eq!(native_event_key, &Some(TypedKey::U64(event.event_sequence)));
    }

    let cold_requests = cold_events
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let cold_batch = BatchHydrationRequest::new(cold_requests.clone()).unwrap();
    let cold_hydrated = explicit_registry
        .resolver_registry()
        .hydrate_batch(&cold_batch)
        .unwrap();
    let cold_expected = cold_events
        .iter()
        .map(|event| match event.event_sequence {
            1 => selected_first.as_bytes().to_vec(),
            2 => selected_second.as_bytes().to_vec(),
            sequence => panic!("unexpected selected event sequence {sequence}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        cold_hydrated
            .records()
            .iter()
            .map(|record| record.provider_bytes.clone())
            .collect::<Vec<_>>(),
        cold_expected
    );

    let replay = refresh_source_backed_generation(
        &explicit_index_root,
        &explicit_registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(replay.commit.indexed_documents, 2);
    assert_eq!(replay.sources, cold.sources);
    let replay_index = VerifiedIndex::open(&explicit_index_root).unwrap();
    let replay_events = replay_index
        .source_event_page(&selected_source, None, 10)
        .unwrap()
        .items;
    assert_eq!(replay_events, cold_events);
    assert_eq!(
        explicit_registry
            .resolver_registry()
            .hydrate_batch(&cold_batch)
            .unwrap()
            .records()
            .iter()
            .map(|record| record.provider_bytes.clone())
            .collect::<Vec<_>>(),
        cold_expected
    );

    let mut tree_registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut tree_registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    let tree_index_root = temp.path().join("tree-index");
    let tree = refresh_source_backed_generation(
        &tree_index_root,
        &tree_registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(tree.sources.len(), 2);
    let tree_index = VerifiedIndex::open(&tree_index_root).unwrap();
    assert_eq!(tree_index.document_count(), 3);
    assert_eq!(
        tree_index
            .source_event_page(&selected_source, None, 10)
            .unwrap()
            .items,
        cold_events
    );
    assert_eq!(
        tree_registry
            .resolver_registry()
            .hydrate_event(&cold_requests[0])
            .unwrap()
            .provider_bytes,
        cold_expected[0]
    );

    let temporarily_absent = sessions.join("selected.jsonl.temporarily-absent");
    fs::rename(&selected, &temporarily_absent).unwrap();
    let unavailable = tree_registry
        .resolver_registry()
        .hydrate_event(&cold_requests[0])
        .unwrap_err();
    assert_eq!(
        unavailable.kind,
        HydrationFailureKind::TemporarilyUnavailable
    );
    fs::rename(&temporarily_absent, &selected).unwrap();

    let mutated_first = format!(
        "selected full lexical body {} mutatedtaillexeme",
        "meaningful ".repeat(1_024)
    );
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &[&mutated_first, &selected_second]),
    )
    .unwrap();
    let original_first = cold_events
        .iter()
        .find(|event| event.event_sequence == 1)
        .unwrap();
    let stale = explicit_registry
        .resolver_registry()
        .hydrate_event(
            &EventHydrationRequest::new(original_first.event_id, original_first.locator.clone())
                .unwrap(),
        )
        .unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);

    refresh_source_backed_generation(
        &explicit_index_root,
        &explicit_registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    let mutated_index = VerifiedIndex::open(&explicit_index_root).unwrap();
    assert!(mutated_index
        .search_event_candidates("selectedtaillexeme", 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        mutated_index
            .search_event_candidates("mutatedtaillexeme", 10)
            .unwrap()
            .len(),
        1
    );
    let mutated_events = mutated_index
        .source_event_page(&selected_source, None, 10)
        .unwrap()
        .items;
    assert_eq!(
        mutated_events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        cold_events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>()
    );
    assert_ne!(
        mutated_events
            .iter()
            .find(|event| event.event_sequence == 1)
            .unwrap()
            .locator,
        original_first.locator
    );

    fs::remove_file(&selected).unwrap();
    let deleted = explicit_registry
        .resolver_registry()
        .hydrate_event(&cold_requests[0])
        .unwrap_err();
    assert_eq!(deleted.kind, HydrationFailureKind::ConfirmedDeleted);
    let deletion = refresh_source_backed_generation(
        &explicit_index_root,
        &explicit_registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert!(deletion.sources.is_empty());
    assert_eq!(deletion.removals.len(), 1);
    assert_eq!(
        VerifiedIndex::open(&explicit_index_root)
            .unwrap()
            .document_count(),
        0
    );

    fs::create_dir(&selected).unwrap();
    let unavailable = explicit_registry
        .resolver_registry()
        .hydrate_event(&cold_requests[0])
        .unwrap_err();
    assert_eq!(
        unavailable.kind,
        HydrationFailureKind::TemporarilyUnavailable
    );
}

#[test]
fn resolver_routes_selected_tree_to_certified_leaf_format() {
    let route = fixture_route_with_selected_format(
        CaptureProvider::Qoder,
        "qoder_transcript_jsonl_tree",
        "qoder_transcript_jsonl",
        8,
        NativeRecordCoordinate::Jsonl {
            byte_offset: 3,
            byte_length: 5,
            physical_ordinal: 1,
            native_session_key: None,
            native_event_key: None,
        },
        b"qoder".to_vec(),
    );
    let request = route.1.clone();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route.0);
    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_event(&request)
            .unwrap()
            .provider_bytes,
        b"qoder"
    );
}

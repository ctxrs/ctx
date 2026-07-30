use super::*;
use std::{fs::OpenOptions, io::Write};

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
fn codex_automatic_session_roots_are_one_move_stable_inventory_and_resolver() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let sessions = home.join(".codex/sessions");
    let archived = home.join(".codex/archived_sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&archived).unwrap();
    let live_session_id = "019fb010-0000-7000-8000-000000000001";
    let archived_session_id = "019fb010-0000-7000-8000-000000000002";
    let live_path = sessions.join(format!("rollout-{live_session_id}.jsonl"));
    let archived_path = archived.join(format!("rollout-{archived_session_id}.jsonl"));
    fs::write(
        &live_path,
        codex_rollout_bytes(live_session_id, &["unionliverootsentinel"]),
    )
    .unwrap();
    fs::write(
        &archived_path,
        codex_rollout_bytes(archived_session_id, &["unionarchivedrootsentinel"]),
    )
    .unwrap();

    let context = DiscoveryContext::new(
        &home,
        temp.path().join("cwd"),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let data_root = temp.path().join("ctx-data");
    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &data_root,
        vec![
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &archived,
            ),
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &sessions,
            ),
        ],
        Vec::new(),
    );
    assert_eq!(build.executable_route_count(), 1);
    assert_eq!(build.unsupported_route_count(), 0);
    assert!(build.issues.is_empty());
    let route = build.registry.routes().next().unwrap();
    assert_eq!(route.source.path, sessions);

    let writer_options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let index_root = temp.path().join("index");
    let cold =
        refresh_source_backed_generation(&index_root, &build.registry, writer_options.clone())
            .unwrap();
    assert_eq!(cold.scanned_routes, 1);
    assert_eq!(cold.sources.len(), 2);
    assert!(cold.removals.is_empty());
    assert_eq!(cold.commit.indexed_documents, 2);
    let cold_generation = cold.commit.generation_id.clone();
    let cold_opstamp = cold.commit.opstamp;

    let verified = VerifiedIndex::open(&index_root).unwrap();
    let events = verified
        .manifest()
        .sources
        .iter()
        .flat_map(|source| {
            verified
                .source_event_page(source.observation().source(), None, 10)
                .unwrap()
                .items
        })
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    let requests = events
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let batch = build
        .registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests.clone()).unwrap())
        .unwrap();
    for (event, record) in events.iter().zip(batch.records()) {
        let expected = match event.provider_session_id.as_deref() {
            Some(id) if id == live_session_id => b"unionliverootsentinel".as_slice(),
            Some(id) if id == archived_session_id => b"unionarchivedrootsentinel".as_slice(),
            other => panic!("unexpected union session identity: {other:?}"),
        };
        assert_eq!(record.provider_bytes, expected);
        assert_eq!(
            build
                .registry
                .resolver_registry()
                .hydrate_event(
                    &EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap()
                )
                .unwrap()
                .provider_bytes,
            expected
        );
    }
    drop(verified);

    let replay =
        refresh_source_backed_generation(&index_root, &build.registry, writer_options.clone())
            .unwrap();
    assert_eq!(replay.commit.generation_id, cold_generation);
    assert_eq!(replay.commit.opstamp, cold_opstamp);
    assert_eq!(replay.sources, cold.sources);
    assert!(replay.removals.is_empty());

    let moved_path = archived.join(format!("rollout-{live_session_id}.jsonl"));
    fs::rename(&live_path, &moved_path).unwrap();
    let moved =
        refresh_source_backed_generation(&index_root, &build.registry, writer_options.clone())
            .unwrap();
    assert_eq!(moved.sources.len(), cold.sources.len());
    assert!(cold.sources.iter().all(|before| {
        moved.sources.iter().any(|after| {
            after
                .observation()
                .source()
                .exact_descriptor_eq(before.observation().source())
                && after.content_digest() == before.content_digest()
                && after.counts() == before.counts()
        })
    }));
    assert!(moved.removals.is_empty());
    assert!(sessions.read_dir().unwrap().next().is_none());
    let moved_verified = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(moved_verified.document_count(), 2);
    let moved_event_ids = moved_verified
        .manifest()
        .sources
        .iter()
        .flat_map(|source| {
            moved_verified
                .source_event_page(source.observation().source(), None, 10)
                .unwrap()
                .items
        })
        .map(|event| event.event_id)
        .collect::<HashSet<_>>();
    let cold_event_ids = events
        .iter()
        .map(|event| event.event_id)
        .collect::<HashSet<_>>();
    assert_eq!(moved_event_ids, cold_event_ids);
    drop(moved_verified);
    for request in &requests {
        build
            .registry
            .resolver_registry()
            .hydrate_event(request)
            .unwrap();
    }

    fs::remove_file(&moved_path).unwrap();
    fs::remove_file(&archived_path).unwrap();
    let deletion =
        refresh_source_backed_generation(&index_root, &build.registry, writer_options.clone())
            .unwrap();
    assert!(deletion.sources.is_empty());
    assert_eq!(deletion.removals.len(), 2);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().document_count(),
        0
    );
    let deletion_generation = deletion.commit.generation_id.clone();

    let empty_replay =
        refresh_source_backed_generation(&index_root, &build.registry, writer_options).unwrap();
    assert_eq!(empty_replay.commit.generation_id, deletion_generation);
    assert!(empty_replay.sources.is_empty());
    assert_eq!(empty_replay.removals.len(), 2);
    let unavailable = build
        .registry
        .resolver_registry()
        .hydrate_event(&requests[0])
        .unwrap_err();
    assert_eq!(
        unavailable.kind,
        HydrationFailureKind::TemporarilyUnavailable
    );
}

#[test]
fn codex_session_union_recovers_exact_hydration_before_successor_refresh() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let sessions = home.join(".codex/sessions");
    let archived = home.join(".codex/archived_sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&archived).unwrap();

    let active_id = "019fb3ec-7ea0-7150-bba0-acde00000001";
    let archived_id = "019fb3ec-7ea0-7150-bba0-acde00000002";
    let active_text = "resident active Codex union sentinel";
    let archived_text = "resident archived Codex union sentinel";
    fs::write(
        sessions.join(format!("rollout-{active_id}.jsonl")),
        codex_rollout_bytes(active_id, &[active_text]),
    )
    .unwrap();
    fs::write(
        archived.join(format!("rollout-{archived_id}.jsonl")),
        codex_rollout_bytes(archived_id, &[archived_text]),
    )
    .unwrap();

    let context = DiscoveryContext::new(
        &home,
        temp.path().join("cwd"),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let sources = || {
        vec![
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &sessions,
            ),
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &archived,
            ),
        ]
    };
    let data_root = temp.path().join("ctx-data");
    let initial = build_automatic_source_backed_registry_from_parts(
        &context,
        &data_root,
        sources(),
        Vec::new(),
    );
    assert_eq!(initial.executable_route_count(), 1);
    let index_root = temp.path().join("index");
    refresh_source_backed_generation(
        &index_root,
        &initial.registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();

    let index = VerifiedIndex::open(&index_root).unwrap();
    let requests = [active_text, archived_text]
        .into_iter()
        .map(|text| {
            let candidate = index
                .search_event_candidates(text, 2)
                .unwrap()
                .into_iter()
                .next()
                .unwrap();
            EventHydrationRequest::new(candidate.event.event_id, candidate.event.locator)
                .unwrap()
                .with_source_path_hint(candidate.event.source_path)
                .unwrap()
        })
        .collect::<Vec<_>>();
    drop(index);
    drop(initial);

    // This is the daemon-restart shape: rebuild provider routes from current
    // authority and hydrate the retained generation before a successor scan.
    let recovered = build_automatic_source_backed_registry_from_parts(
        &context,
        &data_root,
        sources(),
        Vec::new(),
    );
    assert_eq!(recovered.executable_route_count(), 1);
    let result = recovered
        .registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests).unwrap())
        .unwrap();
    assert_eq!(
        result
            .records()
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![active_text.as_bytes(), archived_text.as_bytes()]
    );
}

#[test]
fn codex_automatic_session_union_rejects_duplicate_native_ids_before_publication() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let sessions = home.join(".codex/sessions");
    let archived = home.join(".codex/archived_sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&archived).unwrap();
    let duplicate_id = "019fb010-0000-7000-8000-000000000011";
    let rollout = codex_rollout_bytes(duplicate_id, &["duplicateunionrootsentinel"]);
    fs::write(
        sessions.join(format!("rollout-{duplicate_id}.jsonl")),
        &rollout,
    )
    .unwrap();
    fs::write(
        archived.join(format!("rollout-{duplicate_id}.jsonl")),
        rollout,
    )
    .unwrap();

    let context = DiscoveryContext::new(
        &home,
        temp.path().join("cwd"),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.path().join("ctx-data"),
        vec![
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &archived,
            ),
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &sessions,
            ),
        ],
        Vec::new(),
    );
    assert_eq!(build.executable_route_count(), 0);
    assert_eq!(build.unsupported_route_count(), 1);
    assert!(build.issues.iter().any(|issue| matches!(
        issue,
        SourceBackedAutomaticRegistryIssue::Unavailable {
            source,
            reason: SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail },
        } if source.path == sessions
            && detail.contains(duplicate_id)
            && detail.contains("resolves to more than one source")
    )));
    assert!(!temp.path().join("index").exists());
}

#[test]
fn codex_session_tree_route_parallel_cold_scan_matches_single_worker_and_preserves_lifecycle() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_ids = [
        "019fb000-0000-7000-8000-000000000001",
        "019fb000-0000-7000-8000-000000000002",
        "019fb000-0000-7000-8000-000000000003",
        "019fb000-0000-7000-8000-000000000004",
    ];
    let mut expected_bodies = HashSet::new();
    for (index, native_session_id) in native_session_ids.iter().enumerate() {
        let messages = [
            format!("routeparallelidentity{index}sentinel"),
            format!("routeparallelcontent{index}sentinel"),
        ];
        expected_bodies.extend(messages.iter().map(|message| message.as_bytes().to_vec()));
        fs::write(
            sessions.join(format!("rollout-{native_session_id}.jsonl")),
            codex_rollout_bytes(
                native_session_id,
                &messages.iter().map(String::as_str).collect::<Vec<_>>(),
            ),
        )
        .unwrap();
    }

    let single_counters = Arc::new(Mutex::new(Vec::new()));
    let mut single_registry = SourceBackedProviderRegistry::new();
    register_codex_session_tree_route_for_test(
        &mut single_registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
        1,
        Arc::clone(&single_counters),
        None,
    )
    .unwrap();
    let parallel_counters = Arc::new(Mutex::new(Vec::new()));
    let mut parallel_registry = SourceBackedProviderRegistry::new();
    register_codex_session_tree_route_for_test(
        &mut parallel_registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
        4,
        Arc::clone(&parallel_counters),
        None,
    )
    .unwrap();

    let writer_options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let single_index_root = temp.path().join("single-index");
    let single = refresh_source_backed_generation(
        &single_index_root,
        &single_registry,
        writer_options.clone(),
    )
    .unwrap();
    let parallel_index_root = temp.path().join("parallel-index");
    let parallel = refresh_source_backed_generation(
        &parallel_index_root,
        &parallel_registry,
        writer_options.clone(),
    )
    .unwrap();
    let single_scan = single_counters.lock().unwrap()[0];
    let parallel_scan = parallel_counters.lock().unwrap()[0];
    assert_eq!(single_scan.scanner_workers, 1);
    assert_eq!(parallel_scan.scanner_workers, 4);
    assert_eq!(single_scan.peak_active_scanners, 1);
    assert_eq!(parallel_scan.peak_active_scanners, 4);
    assert_eq!(single_scan.catalog_sources, 4);
    assert_eq!(parallel_scan.catalog_sources, 4);
    assert_eq!(single_scan.cold_sources, 4);
    assert_eq!(parallel_scan.cold_sources, 4);
    assert_eq!(single_scan.staged_documents, 8);
    assert_eq!(parallel_scan.staged_documents, 8);
    assert_eq!(single_scan.complete_records_scanned, 12);
    assert_eq!(parallel_scan.complete_records_scanned, 12);
    let mut normalized_single_scan = single_scan;
    let mut normalized_parallel_scan = parallel_scan;
    normalized_single_scan.scanner_workers = 0;
    normalized_parallel_scan.scanner_workers = 0;
    normalized_single_scan.peak_active_scanners = 0;
    normalized_parallel_scan.peak_active_scanners = 0;
    assert_eq!(normalized_single_scan, normalized_parallel_scan);
    assert_eq!(single.commit.indexed_documents, 8);
    assert_eq!(parallel.commit.indexed_documents, 8);

    let single_verified = VerifiedIndex::open(&single_index_root).unwrap();
    let parallel_verified = VerifiedIndex::open(&parallel_index_root).unwrap();
    assert_eq!(
        single_verified.manifest().sources,
        parallel_verified.manifest().sources
    );
    assert_eq!(single.commit.generation_id, parallel.commit.generation_id);
    let source_events = |verified: &VerifiedIndex| {
        verified
            .manifest()
            .sources
            .iter()
            .flat_map(|source| {
                verified
                    .source_event_page(source.observation().source(), None, 10)
                    .unwrap()
                    .items
            })
            .collect::<Vec<_>>()
    };
    let single_events = source_events(&single_verified);
    let parallel_events = source_events(&parallel_verified);
    assert_eq!(single_events, parallel_events);
    let requests = single_events
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let request = BatchHydrationRequest::new(requests).unwrap();
    let single_content = single_registry
        .resolver_registry()
        .hydrate_batch(&request)
        .unwrap()
        .records()
        .iter()
        .map(|record| record.provider_bytes.clone())
        .collect::<Vec<_>>();
    let parallel_content = parallel_registry
        .resolver_registry()
        .hydrate_batch(&request)
        .unwrap()
        .records()
        .iter()
        .map(|record| record.provider_bytes.clone())
        .collect::<Vec<_>>();
    assert_eq!(single_content, parallel_content);
    assert_eq!(
        parallel_content.into_iter().collect::<HashSet<_>>(),
        expected_bodies
    );
    let cold_generation = parallel.commit.generation_id;
    drop(single_verified);
    drop(parallel_verified);

    let replay = refresh_source_backed_generation(
        &parallel_index_root,
        &parallel_registry,
        writer_options.clone(),
    )
    .unwrap();
    let replay_scan = parallel_counters.lock().unwrap()[1];
    assert_eq!(replay.commit.generation_id, cold_generation);
    assert_eq!(replay_scan.scanner_workers, 0);
    assert_eq!(replay_scan.replayed_sources, 4);
    assert_eq!(replay_scan.staged_documents, 0);

    let append_record = serde_json::json!({
        "timestamp": "2026-07-29T12:01:00Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "input_text",
                "text": "routeappendlifecyclesentinel"
            }]
        }
    });
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(sessions.join(format!("rollout-{}.jsonl", native_session_ids[0])))
            .unwrap(),
        "{append_record}"
    )
    .unwrap();
    refresh_source_backed_generation(
        &parallel_index_root,
        &parallel_registry,
        writer_options.clone(),
    )
    .unwrap();
    let append_scan = parallel_counters.lock().unwrap()[2];
    assert_eq!(append_scan.scanner_workers, 1);
    assert_eq!(append_scan.appended_sources, 1);
    assert_eq!(append_scan.replayed_sources, 3);
    assert_eq!(append_scan.staged_documents, 1);

    fs::write(
        sessions.join(format!("rollout-{}.jsonl", native_session_ids[1])),
        codex_rollout_bytes(
            native_session_ids[1],
            &["routereplacementlifecyclesentinel"],
        ),
    )
    .unwrap();
    refresh_source_backed_generation(
        &parallel_index_root,
        &parallel_registry,
        writer_options.clone(),
    )
    .unwrap();
    let replacement_scan = parallel_counters.lock().unwrap()[3];
    assert_eq!(replacement_scan.scanner_workers, 1);
    assert_eq!(replacement_scan.replaced_sources, 1);
    assert_eq!(replacement_scan.replayed_sources, 3);
    assert_eq!(replacement_scan.staged_documents, 1);

    fs::remove_file(sessions.join(format!("rollout-{}.jsonl", native_session_ids[2]))).unwrap();
    refresh_source_backed_generation(&parallel_index_root, &parallel_registry, writer_options)
        .unwrap();
    let deletion_scan = parallel_counters.lock().unwrap()[4];
    assert_eq!(deletion_scan.scanner_workers, 0);
    assert_eq!(deletion_scan.replayed_sources, 3);
    assert_eq!(deletion_scan.deleted_sources, 1);
    let lifecycle_index = VerifiedIndex::open(&parallel_index_root).unwrap();
    assert_eq!(lifecycle_index.document_count(), 6);
    assert_eq!(
        lifecycle_index
            .search_event_candidates("routeappendlifecyclesentinel", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        lifecycle_index
            .search_event_candidates("routereplacementlifecyclesentinel", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(lifecycle_index
        .search_event_candidates("routeparallelidentity2sentinel", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn codex_session_tree_parallel_route_keeps_failed_terminal_certification_atomic() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let archived = temp.path().join("archived_sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&archived).unwrap();
    let baseline_id = "019fb000-0000-7000-8000-000000000011";
    fs::write(
        sessions.join(format!("rollout-{baseline_id}.jsonl")),
        codex_rollout_bytes(baseline_id, &["atomicbaselinesentinel"]),
    )
    .unwrap();
    let mut baseline_registry = SourceBackedProviderRegistry::new();
    register_codex_session_tree_routes(
        &mut baseline_registry,
        vec![
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &sessions,
            ),
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &archived,
            ),
        ],
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    let writer_options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    refresh_source_backed_generation(&index_root, &baseline_registry, writer_options.clone())
        .unwrap();
    let baseline = VerifiedIndex::open(&index_root).unwrap();
    let baseline_generation = baseline.generation_id().to_owned();
    let baseline_sources = baseline.manifest().sources.clone();
    drop(baseline);

    fs::remove_file(sessions.join(format!("rollout-{baseline_id}.jsonl"))).unwrap();
    for (root, native_session_id, marker) in [
        (
            &sessions,
            "019fb000-0000-7000-8000-000000000012",
            "uncommittedparallelroutesentinelone",
        ),
        (
            &archived,
            "019fb000-0000-7000-8000-000000000013",
            "uncommittedparallelroutesentineltwo",
        ),
    ] {
        fs::write(
            root.join(format!("rollout-{native_session_id}.jsonl")),
            codex_rollout_bytes(native_session_id, &[marker]),
        )
        .unwrap();
    }
    let late_archived = archived.clone();
    let after_scan: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let late_id = "019fb000-0000-7000-8000-000000000014";
        fs::write(
            late_archived.join(format!("rollout-{late_id}.jsonl")),
            codex_rollout_bytes(late_id, &["lateinventorycertificationsentinel"]),
        )
        .unwrap();
    });
    let counters = Arc::new(Mutex::new(Vec::new()));
    let mut failing_registry = SourceBackedProviderRegistry::new();
    register_codex_session_tree_routes_for_test(
        &mut failing_registry,
        vec![
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &sessions,
            ),
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &archived,
            ),
        ],
        SourceBackedRouteSelection::Automatic,
        2,
        Arc::clone(&counters),
        Some(after_scan),
    )
    .unwrap();
    let error = refresh_source_backed_generation(&index_root, &failing_registry, writer_options)
        .unwrap_err();
    assert!(
        matches!(
            &error,
            SourceBackedCoordinatorError::Index(IndexError::CompleteInventoryInvalidated {
                provider,
                authority_namespace,
            }) if provider == "codex" && authority_namespace == "codex.sessions-root"
        ),
        "unexpected terminal certification error: {error:?}"
    );
    let failed_scan = counters.lock().unwrap()[0];
    assert_eq!(failed_scan.scanner_workers, 2);
    assert_eq!(failed_scan.cold_sources, 2);
    assert_eq!(failed_scan.deleted_sources, 1);

    let after = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(after.generation_id(), baseline_generation);
    assert_eq!(after.manifest().sources, baseline_sources);
    assert_eq!(after.document_count(), 1);
    assert_eq!(
        after
            .search_event_candidates("atomicbaselinesentinel", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(after
        .search_event_candidates("uncommittedparallelroutesentinelone", 10)
        .unwrap()
        .is_empty());
    assert!(after
        .search_event_candidates("lateinventorycertificationsentinel", 10)
        .unwrap()
        .is_empty());
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

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        collections::HashMap,
        fs,
    };

    use ctx_history_capture::{
        complete_content::{CompleteContentError, CompleteContentErrorKind},
        ingest_codex_source_backed_v0,
    };
    use ctx_history_core::{
        database_path, derive_event_id, derive_session_id, BatchHydrationRequest,
        BatchHydrationResult, CertifiedSource, ContentSourceResolver, EventHydrationRequest,
        EventIdentityInput, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
        LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
        ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
        SourceRecordLocator, StableEntityId, TypedKey,
    };
    use ctx_history_index::{GenerationWriter, LexicalDocument, WriterOptions};
    use serde_json::json;
    use tempfile::tempdir;

    use crate::semantic::SourceBackedRefreshDaemonUnavailable;

    use super::*;

    const TEST_SESSION_ID: &str = "019fa000-0000-7000-8000-0000000000d1";
    const TEST_QUERY: &str = "pinnedgenerationrouting";

    #[derive(Default)]
    struct MockContentResolver {
        bodies: HashMap<StableEntityId, Vec<u8>>,
        calls: RefCell<Vec<(String, String)>>,
        batch_calls: Cell<usize>,
    }

    impl MockContentResolver {
        fn with_body(mut self, event: &EventRecord, body: impl Into<Vec<u8>>) -> Self {
            self.bodies.insert(event.event_id, body.into());
            self
        }
    }

    impl ContentSourceResolver for MockContentResolver {
        fn hydrate_event(
            &self,
            request: &EventHydrationRequest,
        ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
            self.calls.borrow_mut().push((
                request.locator().source().provider().to_owned(),
                request.locator().source().source_format().to_owned(),
            ));
            let provider_bytes =
                self.bodies
                    .get(&request.event_id())
                    .cloned()
                    .ok_or_else(|| HydrationFailure {
                        kind: HydrationFailureKind::MissingRecord,
                        detail: "mock provider record is absent".to_owned(),
                    })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes,
            })
        }

        fn hydrate_batch(
            &self,
            request: &BatchHydrationRequest,
        ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
            self.batch_calls
                .set(self.batch_calls.get().saturating_add(1));
            let records = request
                .events()
                .iter()
                .map(|event| self.hydrate_event(event))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let result = BatchHydrationResult::new(records).map_err(|error| HydrationFailure {
                kind: HydrationFailureKind::InvalidLocator,
                detail: error.to_string(),
            })?;
            result.validate_for_request(request)?;
            Ok(result)
        }
    }

    fn fixture_event(
        provider: CaptureProvider,
        source_format: &str,
        lineage: u8,
        sequence: u64,
    ) -> EventRecord {
        let source = SourceKey::derive(
            provider.as_str(),
            source_format,
            "fixture",
            1,
            SourceAnchor::CatalogLineage([lineage; 32]),
        )
        .unwrap();
        let native_session_key = NativeSessionKey::native_id(
            "session",
            TypedKey::utf8(format!("fixture-session-{lineage}")).unwrap(),
        )
        .unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &native_session_key,
        })
        .unwrap();
        let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        let locator = SourceRecordLocator::new(
            source,
            NativeRecordCoordinate::ProviderNative {
                namespace: "fixture".to_owned(),
                coordinate: TypedKey::U64(sequence),
            },
            LocatorRevisionPolicy::ExactSourceRevision,
            Some([lineage; 32]),
            [sequence as u8; 32],
        )
        .unwrap();
        EventRecord {
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            locator,
            provider: provider.as_str().to_owned(),
            source_format: source_format.to_owned(),
            provider_session_id: Some(format!("fixture-session-{lineage}")),
            branch: None,
            source_path: None,
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence: sequence,
            occurred_at_unix_ms: None,
            event_type: "message".to_owned(),
            role: Some("assistant".to_owned()),
            workspace: None,
            cwd: None,
            touched_files: Vec::new(),
        }
    }

    fn request(refresh: RefreshArg) -> SourceSearchRequest {
        SourceSearchRequest {
            query: TEST_QUERY.to_owned(),
            terms: Vec::new(),
            limit: 10,
            provider: Some(CaptureProvider::Codex),
            history_source: None,
            provider_key: None,
            source_id: None,
            source_format: None,
            workspace: None,
            since: None,
            primary_only: false,
            include_subagents: false,
            event_type: None,
            file: None,
            session: None,
            events: false,
            include_current_session: true,
            backend: Some(SearchBackendArg::Lexical),
            semantic_weight: 0.35,
            semantic_enabled: true,
            refresh,
        }
    }

    fn write_test_generation(data_root: &Path) {
        let sessions = data_root.join("sessions");
        let source = sessions.join(format!("rollout-{TEST_SESSION_ID}.jsonl"));
        fs::create_dir_all(&sessions).unwrap();
        let records = [
            json!({
                "timestamp": "2026-07-28T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": TEST_SESSION_ID,
                    "timestamp": "2026-07-28T12:00:00Z",
                    "cwd": "/workspace/pinned",
                    "originator": "codex_cli_rs",
                    "cli_version": "0.1.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-07-28T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!("{TEST_QUERY} sentinel")
                    }]
                }
            }),
        ];
        let body = records
            .iter()
            .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
            .collect::<String>();
        fs::write(source, body).unwrap();
        ingest_codex_source_backed_v0(&sessions, index_root(data_root)).unwrap();
    }

    fn append_fixture_event(data_root: &Path, event: EventRecord, revision: u8) {
        let source = event.locator.source().clone();
        let mut writer = GenerationWriter::open(
            index_root(data_root),
            WriterOptions {
                indexer_threads: 1,
                memory_bytes: 32 * 1024 * 1024,
            },
        )
        .unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer
            .add_document(LexicalDocument {
                event_id: event.event_id,
                session_id: event.session_id,
                parent_session_id: event.parent_session_id,
                root_session_id: event.root_session_id,
                source: source.clone(),
                locator: event.locator,
                provider_session_id: event.provider_session_id,
                branch: event.branch,
                source_path: event.source_path,
                agent_type: event.agent_type,
                is_primary: event.is_primary,
                event_sequence: event.event_sequence,
                occurred_at_unix_ms: event.occurred_at_unix_ms,
                event_type: event.event_type,
                role: event.role,
                body: "ambiguous provider session fixture".to_owned(),
                workspace: event.workspace,
                cwd: event.cwd,
                touched_files: event.touched_files,
            })
            .unwrap();
        let observation =
            SourceObservation::new(source, "fixture-revision-v1", vec![revision]).unwrap();
        writer
            .certify_source(
                CertifiedSource::certify(
                    observation.clone(),
                    observation,
                    "fixture-parser-v1",
                    [revision; 32],
                    ScannedSourceCounts {
                        complete_records: 1,
                        retained_records: 1,
                        indexed_documents: 1,
                        certified_bytes: 1,
                        ..ScannedSourceCounts::default()
                    },
                )
                .unwrap(),
            )
            .unwrap();
        writer.commit(|_| true).unwrap();
    }

    #[test]
    fn result_window_requires_one_additional_shaped_session() {
        let candidates = [
            EventSearchCandidate {
                event: fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 1, 1),
                score: 3.0,
            },
            EventSearchCandidate {
                event: fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 1, 2),
                score: 2.0,
            },
            EventSearchCandidate {
                event: fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 2, 1),
                score: 1.0,
            },
        ];

        let duplicates_only = shape_search_result_window(candidates[..2].iter(), 1, false);
        assert_eq!(duplicates_only.hits.len(), 1);
        assert_eq!(duplicates_only.hits[0].more_matches_in_session, 1);
        assert!(!duplicates_only.more_available);

        let additional_session = shape_search_result_window(candidates.iter(), 1, false);
        assert_eq!(additional_session.limit, 1);
        assert_eq!(additional_session.hits.len(), 1);
        assert_eq!(additional_session.hits[0].more_matches_in_session, 1);
        assert!(additional_session.more_available);
    }

    #[test]
    fn event_result_window_returns_limit_and_records_only_one_extra_hit() {
        let candidates = (1..=4)
            .map(|sequence| EventSearchCandidate {
                event: fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 1, sequence),
                score: 5.0 - sequence as f32,
            })
            .collect::<Vec<_>>();

        let window = shape_search_result_window(candidates.iter(), 2, true);
        assert_eq!(window.limit, 2);
        assert_eq!(window.hits.len(), 2);
        assert!(window.more_available);
        assert_eq!(window.hits[0].event.event_sequence, 1);
        assert_eq!(window.hits[1].event.event_sequence, 2);
    }

    #[test]
    fn show_provider_session_resolution_is_ambiguous_until_provider_qualified() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let mut warp = fixture_event(CaptureProvider::Warp, "warp_sqlite", 2, 2);
        warp.provider_session_id = Some(TEST_SESSION_ID.to_owned());
        append_fixture_event(temp.path(), warp, 2);
        let index = open_index(temp.path()).unwrap();

        let matches = index
            .sessions_by_provider_session_id(TEST_SESSION_ID, None)
            .unwrap();
        assert_eq!(matches.len(), 2);
        let error = resolve_show_session(&index, None, Some(TEST_SESSION_ID), None).unwrap_err();
        let detail = error.to_string();
        assert!(detail.contains("is ambiguous"), "{detail}");
        for session in matches {
            assert!(detail.contains(&session.session_id.to_string()), "{detail}");
        }
        assert!(
            detail.contains("pass --provider or a ctx session ID"),
            "{detail}"
        );

        let codex = resolve_show_session(
            &index,
            None,
            Some(TEST_SESSION_ID),
            Some(CaptureProvider::Codex),
        )
        .unwrap();
        assert_eq!(codex.provider, "codex");
        assert_eq!(codex.provider_session_id.as_deref(), Some(TEST_SESSION_ID));

        let warp = resolve_show_session(
            &index,
            None,
            Some(TEST_SESSION_ID),
            Some(CaptureProvider::Warp),
        )
        .unwrap();
        assert_eq!(warp.provider, "warp");
        assert_eq!(warp.provider_session_id.as_deref(), Some(TEST_SESSION_ID));
    }

    #[test]
    fn core_refresh_modes_map_to_the_daemon_contract() {
        assert_eq!(
            source_backed_refresh_mode(RefreshArg::Off),
            SourceBackedRefreshMode::Off
        );
        assert_eq!(
            source_backed_refresh_mode(RefreshArg::Background),
            SourceBackedRefreshMode::Background
        );
        assert_eq!(
            source_backed_refresh_mode(RefreshArg::Wait),
            SourceBackedRefreshMode::Wait
        );
    }

    #[test]
    fn daemon_unavailable_error_remains_typed_through_core_routing() {
        let temp = tempdir().unwrap();
        let error = match refresh_for_search(&request(RefreshArg::Wait), temp.path()) {
            Ok(_) => panic!("refresh unexpectedly succeeded without a daemon"),
            Err(error) => error,
        };
        assert!(error
            .downcast_ref::<SourceBackedRefreshDaemonUnavailable>()
            .is_some());
        assert!(format!("{error:#}").contains("no foreground writer was started"));
    }

    #[test]
    fn core_search_consumes_the_coordinator_pin_without_reopening() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let requested_mode = Cell::new(None);
        let outcome =
            refresh_for_search_with(&request(RefreshArg::Off), temp.path(), |data_root, mode| {
                requested_mode.set(Some(mode));
                Ok(SourceBackedRefreshObservation {
                    mode,
                    status: "off".to_owned(),
                    request_id: None,
                    daemon_available: false,
                    source_count: 0,
                    receipt: None,
                    pin: PinnedSourceBackedGeneration::from_index(open_index(data_root)?),
                })
            })
            .unwrap();
        assert_eq!(requested_mode.get(), Some(SourceBackedRefreshMode::Off));
        let generation = outcome.pin.generation_id().to_owned();

        fs::remove_file(index_root(temp.path()).join("meta.json")).unwrap();
        let (value, collection, index) = search_existing_generation_with_hydrator(
            &request(RefreshArg::Off),
            outcome.pin.into_index(),
            temp.path(),
            0.35,
            outcome.status,
            outcome.source_count,
            |_index, _data_root, events| {
                Ok(events
                    .iter()
                    .map(|event| {
                        (
                            event.event_id.as_uuid(),
                            "exact injected search body".to_owned(),
                        )
                    })
                    .collect())
            },
        )
        .unwrap();

        assert_eq!(index.generation_id(), generation);
        assert_eq!(value["retrieval"]["generation_id"], generation);
        assert_eq!(collection.result_window.hits.len(), 1);
    }

    #[test]
    fn refresh_off_surfaces_typed_resolver_unavailable_without_retrying() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let wait_calls = Cell::new(0);
        let error = match search_with_hydration_retry_with(
            &request(RefreshArg::Off),
            temp.path(),
            0.35,
            RefreshOutcome {
                pin: PinnedSourceBackedGeneration::from_index(open_index(temp.path()).unwrap()),
                status: "existing_generation",
                source_count: 0,
            },
            search_existing_generation,
            |_request, _data_root| {
                wait_calls.set(wait_calls.get() + 1);
                panic!("refresh off must not retry source discovery")
            },
        ) {
            Ok(_) => panic!("refresh-off hydration unexpectedly succeeded"),
            Err(error) => error,
        };

        assert!(PinnedSourceBackedGeneration::source_hydration_retryable(
            &error
        ));
        assert!(format!("{error:#}").contains("resolver_service_unavailable"));
        assert_eq!(wait_calls.get(), 0);
    }

    #[test]
    fn background_search_retries_hydration_once_after_daemon_wait_repin() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let run_calls = Cell::new(0);
        let wait_calls = Cell::new(0);
        let outcome = search_with_hydration_retry_with(
            &request(RefreshArg::Background),
            temp.path(),
            0.35,
            RefreshOutcome {
                pin: PinnedSourceBackedGeneration::from_index(open_index(temp.path()).unwrap()),
                status: "daemon_background",
                source_count: 1,
            },
            |request, index, data_root, semantic_weight, status, source_count| {
                run_calls.set(run_calls.get() + 1);
                if run_calls.get() == 1 {
                    search_existing_generation(
                        request,
                        index,
                        data_root,
                        semantic_weight,
                        status,
                        source_count,
                    )
                } else {
                    search_existing_generation_with_hydrator(
                        request,
                        index,
                        data_root,
                        semantic_weight,
                        status,
                        source_count,
                        |_index, _data_root, events| {
                            Ok(events
                                .iter()
                                .map(|event| {
                                    (
                                        event.event_id.as_uuid(),
                                        "exact source after daemon repin".to_owned(),
                                    )
                                })
                                .collect())
                        },
                    )
                }
            },
            |_request, data_root| {
                wait_calls.set(wait_calls.get() + 1);
                Ok(RefreshOutcome {
                    pin: PinnedSourceBackedGeneration::from_index(open_index(data_root)?),
                    status: "published",
                    source_count: 1,
                })
            },
        )
        .unwrap();

        assert_eq!(outcome.3, "published");
        assert_eq!(
            outcome.0["results"][0]["snippet"],
            "exact source after daemon repin"
        );
        assert_eq!(run_calls.get(), 2);
        assert_eq!(wait_calls.get(), 1);
    }

    #[test]
    fn generation_only_semantic_is_typed_and_hybrid_falls_back_without_exact_projection() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let index = open_index(temp.path()).unwrap();
        assert!(!database_path(temp.path().to_path_buf()).exists());

        let mut lexical_request = request(RefreshArg::Off);
        lexical_request.backend = Some(SearchBackendArg::Lexical);
        let filters = index_search_filters(&lexical_request, &index).unwrap();
        let lexical =
            collect_search_hits_with_backend(&lexical_request, &index, temp.path(), 0.35, &filters)
                .unwrap();
        assert_eq!(lexical.effective_backend, SearchBackendArg::Lexical);
        assert_eq!(lexical.result_window.hits.len(), 1);

        let mut hybrid_request = request(RefreshArg::Off);
        hybrid_request.backend = Some(SearchBackendArg::Hybrid);
        let filters = index_search_filters(&hybrid_request, &index).unwrap();
        let fallback =
            collect_search_hits_with_backend(&hybrid_request, &index, temp.path(), 0.35, &filters)
                .unwrap();
        assert_eq!(fallback.requested_backend, SearchBackendArg::Hybrid);
        assert_eq!(fallback.effective_backend, SearchBackendArg::Lexical);
        assert_eq!(fallback.semantic_weight, 0.35);
        assert_eq!(fallback.semantic_status, "unavailable");
        assert_eq!(
            fallback.semantic_fallback.as_ref().map(|value| value.code),
            Some("semantic_store_missing")
        );
        assert_eq!(fallback.result_window.hits.len(), 1);

        let mut semantic_request = request(RefreshArg::Off);
        semantic_request.backend = Some(SearchBackendArg::Semantic);
        let filters = index_search_filters(&semantic_request, &index).unwrap();
        let missing = collect_search_hits_with_backend(
            &semantic_request,
            &index,
            temp.path(),
            0.35,
            &filters,
        )
        .unwrap_err();
        let not_ready = missing
            .downcast_ref::<SourceBackedSemanticNotReady>()
            .expect("semantic-only errors remain typed");
        assert_eq!(not_ready.code(), "semantic_store_missing");
        assert!(not_ready.detail().contains("flat-F32"));

        let mut embedding = vec![0.0; 384];
        embedding[0] = 1.0;
        let exact_source_texts = index
            .semantic_event_page(None, ctx_history_index::MAX_SEMANTIC_EVENT_PAGE_ITEMS)
            .unwrap()
            .items
            .into_iter()
            .map(|event| {
                (
                    event.event_id.as_uuid(),
                    format!("exact provider fixture text containing {TEST_QUERY}"),
                )
            })
            .collect();
        PinnedSourceBackedGeneration::install_source_generation_flat_fixture(
            &index,
            temp.path(),
            &embedding,
            exact_source_texts,
        )
        .unwrap();
        assert!(temp.path().join("search").join("semantic").is_dir());
        assert!(
            !temp.path().join("semantic-vectors").exists(),
            "the fresh source epoch must not open or reuse the legacy vector root"
        );

        for backend in [SearchBackendArg::Semantic, SearchBackendArg::Hybrid] {
            let mut source_request = request(RefreshArg::Off);
            source_request.backend = Some(backend);
            let filters = index_search_filters(&source_request, &index).unwrap();
            let collection = collect_search_hits_with_backend_using(
                &source_request,
                &index,
                temp.path(),
                0.35,
                &filters,
                |index, data_root, _query, filters, candidate_limit| {
                    PinnedSourceBackedGeneration::semantic_candidates_for_source_generation_with_embedding(
                        index,
                        data_root,
                        filters,
                        candidate_limit,
                        &embedding,
                    )
                },
            )
            .unwrap();
            assert_eq!(collection.requested_backend, backend);
            assert_eq!(collection.effective_backend, backend);
            assert_eq!(collection.semantic_status, "ready");
            assert_eq!(collection.result_window.hits.len(), 1);
            let diagnostics = collection.semantic_diagnostics.unwrap();
            assert_eq!(diagnostics["query_count"], 1);
            let query_diagnostics = &diagnostics["queries"][0]["diagnostics"];
            assert_eq!(query_diagnostics["vector_backend"], "flat_f32");
            assert_eq!(
                query_diagnostics["core_generation_id"],
                index.generation_id()
            );
            assert!(query_diagnostics["flat_generation"].as_u64().unwrap() > 0);
            assert!(query_diagnostics["flat_generation_hash"]
                .as_str()
                .is_some_and(|value| !value.is_empty()));
        }

        assert!(
            !database_path(temp.path().to_path_buf()).exists(),
            "generation-only semantic/hybrid must not create or open the legacy Store"
        );
    }

    #[test]
    fn zero_weight_hybrid_performs_no_semantic_callback_or_store_work() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let index = open_index(temp.path()).unwrap();
        let mut hybrid_request = request(RefreshArg::Off);
        hybrid_request.backend = Some(SearchBackendArg::Hybrid);
        let filters = index_search_filters(&hybrid_request, &index).unwrap();

        let collection = collect_search_hits_with_backend_using(
            &hybrid_request,
            &index,
            temp.path(),
            0.0,
            &filters,
            |_index, _data_root, _query, _filters, _candidate_limit| {
                panic!("zero-weight hybrid must not pin vectors, embed, or use IPC")
            },
        )
        .unwrap();

        assert_eq!(collection.requested_backend, SearchBackendArg::Hybrid);
        assert_eq!(collection.effective_backend, SearchBackendArg::Lexical);
        assert_eq!(collection.semantic_weight, 0.0);
        assert_eq!(collection.semantic_status, "skipped");
        assert!(collection.semantic_fallback.is_none());
        assert_eq!(collection.result_window.hits.len(), 1);
        assert!(!temp.path().join("search").join("semantic").exists());
        assert!(!database_path(temp.path().to_path_buf()).exists());
    }

    #[test]
    fn mcp_source_route_applies_the_semantic_config_default_to_source_generations() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let mut source_request = request(RefreshArg::Off);
        source_request.query = "query-with-no-fixture-match".to_owned();
        source_request.backend = None;

        let lexical = mcp_search(source_request.clone(), temp.path()).unwrap();
        assert_eq!(lexical["retrieval"]["requested_mode"], "lexical");
        assert_eq!(lexical["retrieval"]["effective_mode"], "lexical");

        config::set_semantic_search_enabled(temp.path(), true).unwrap();
        let hybrid = mcp_search(source_request, temp.path()).unwrap();
        assert_eq!(hybrid["retrieval"]["requested_mode"], "hybrid");
        assert_eq!(hybrid["retrieval"]["effective_mode"], "lexical");
        assert_eq!(
            hybrid["retrieval"]["semantic_fallback_code"],
            "semantic_store_missing"
        );

        let mut file_only = request(RefreshArg::Off);
        file_only.query.clear();
        file_only.backend = None;
        file_only.file = Some(PathBuf::from("/fixture/no-match.rs"));
        let file_only = mcp_search(file_only, temp.path()).unwrap();
        assert_eq!(file_only["retrieval"]["requested_mode"], "lexical");
        assert_eq!(file_only["retrieval"]["effective_mode"], "lexical");
        assert!(!database_path(temp.path().to_path_buf()).exists());
    }

    #[test]
    fn complete_content_hydrates_typed_locators_for_multiple_providers() {
        let codex = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 1, 1);
        let warp = fixture_event(CaptureProvider::Warp, "warp_sqlite", 2, 2);
        let resolver = MockContentResolver::default()
            .with_body(&codex, "complete Codex source")
            .with_body(&warp, "complete Warp source");
        let resolved = resolve_complete_contents(&[&codex, &warp], usize::MAX, &resolver).unwrap();

        assert_eq!(resolved[0].text, "complete Codex source");
        assert_eq!(resolved[1].text, "complete Warp source");
        assert_eq!(resolver.batch_calls.get(), 1);
        assert_eq!(
            resolver.calls.into_inner(),
            vec![
                ("codex".to_owned(), "codex_session_jsonl".to_owned()),
                ("warp".to_owned(), "warp_sqlite".to_owned()),
            ]
        );
    }

    #[test]
    fn complete_content_fails_when_exact_source_is_unavailable() {
        let event = fixture_event(CaptureProvider::Warp, "warp_sqlite", 3, 3);
        let error =
            resolve_complete_contents(&[&event], usize::MAX, &MockContentResolver::default())
                .unwrap_err();

        assert!(format!("{error:#}").contains("mock provider record is absent"));
    }

    #[test]
    fn complete_content_rejects_non_utf8_provider_bytes() {
        let event = fixture_event(CaptureProvider::Warp, "warp_sqlite", 4, 4);
        let resolver = MockContentResolver::default().with_body(&event, vec![b'o', b'k', 0x80]);
        let error = resolve_complete_contents(&[&event], usize::MAX, &resolver).unwrap_err();

        assert!(format!("{error:#}").contains("non-UTF-8 exact content"));
    }

    #[test]
    fn complete_content_preserves_the_cumulative_output_limit() {
        let first = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 5, 5);
        let second = fixture_event(CaptureProvider::Warp, "warp_sqlite", 6, 6);
        let resolver = MockContentResolver::default()
            .with_body(&first, "four")
            .with_body(&second, "five");
        let error = resolve_complete_contents(&[&first, &second], 7, &resolver).unwrap_err();

        assert!(format!("{error:#}").contains("exceeds the 7-byte output limit"));
        assert!(format!("{error:#}").contains(&second.event_id.to_string()));
    }

    #[test]
    fn show_json_output_limit_is_typed_for_both_policy_tokens() {
        let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 7, 7);
        for policy in [ContentPolicy::Indexed, ContentPolicy::Complete] {
            let value = json!({
                "content_policy": policy.as_str(),
                "events": [{
                    "ctx_event_id": event.event_id.as_uuid(),
                    "text": "provider source",
                }],
            });
            let error = enforce_json_output_limit(&value, 1, event.event_id.as_uuid()).unwrap_err();
            let typed = error
                .downcast_ref::<CompleteContentError>()
                .expect("show output bound should preserve the typed content error");
            assert_eq!(typed.kind, CompleteContentErrorKind::ContentTooLarge);
            assert_eq!(typed.event_id, event.event_id.as_uuid());
        }
    }

    #[test]
    fn measured_output_byte_helpers_match_existing_renderers() {
        let session = json!({
            "target": "session",
            "ctx_session_id": "session-1",
            "provider": "codex",
            "provider_session_id": "provider-1",
            "source": {
                "path": "/tmp/session.jsonl",
                "source_format": "codex_session_jsonl",
                "exists": true,
            },
            "resume": {
                "command": "codex resume provider-1",
            },
        });
        let session_text = "ctx_session_id: session-1\n\
provider: codex\n\
provider_session_id: provider-1\n\
path: /tmp/session.jsonl\n\
source_format: codex_session_jsonl\n\
source_exists: true\n\
resume_command: codex resume provider-1\n";
        assert_eq!(
            locate_session_text_output_bytes(&session),
            session_text.len()
        );

        let event = json!({
            "target": "event",
            "ctx_event_id": "event-1",
            "ctx_session_id": "session-1",
            "provider": "codex",
            "provider_session_id": "provider-1",
            "event_type": "message",
            "role": "assistant",
            "cursor": "cursor-1",
            "source": {
                "path": "/tmp/session.jsonl",
            },
            "source_record": {
                "ordinal": 4,
                "subrecord_index": 2,
            },
        });
        let event_text = "ctx_event_id: event-1\n\
ctx_session_id: session-1\n\
provider: codex\n\
provider_session_id: provider-1\n\
event_type: message\n\
role: assistant\n\
cursor: cursor-1\n\
path: /tmp/session.jsonl\n\
source_record_ordinal: 4\n\
source_record_subrecord_index: 2\n";
        assert_eq!(locate_event_text_output_bytes(&event), event_text.len());
        assert_eq!(
            pretty_json_stdout_bytes(&event).unwrap(),
            serde_json::to_string_pretty(&event).unwrap().len() + 1
        );
        assert_eq!(stdout_body_bytes("body"), 5);
        assert_eq!(stdout_body_bytes("body\n"), 5);
    }
}

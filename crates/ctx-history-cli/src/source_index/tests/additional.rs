use super::*;

#[test]
fn refresh_off_uses_the_existing_generation_without_activation() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());

    let outcome =
        refresh_for_search(&request(RefreshArg::Off), RefreshArg::Off, temp.path()).unwrap();

    assert_eq!(outcome.status, "existing_generation");
    assert_eq!(
        outcome.pin.generation_id(),
        open_index(temp.path()).unwrap().generation_id()
    );
}

#[test]
fn core_search_consumes_the_coordinator_pin_after_active_generation_pointer_deletion() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let requested_mode = Cell::new(None);
    let pin = crate::semantic::pin_active_verified_generation(temp.path()).unwrap();
    let outcome = refresh_for_search_with(
        &request(RefreshArg::Off),
        RefreshArg::Off,
        temp.path(),
        |_data_root, mode| {
            requested_mode.set(Some(mode));
            Ok(ctx_daemon_cli::SourceBackedRefreshObservation {
                mode,
                status: "off".to_owned(),
                request_id: None,
                daemon_available: false,
                source_count: 0,
                request_previous_generation: None,
                request_generation_changed: false,
                scanned_routes: None,
                receipt: None,
                pin,
            })
        },
    )
    .unwrap();
    assert_eq!(requested_mode.get(), Some(SourceBackedRefreshMode::Off));
    let generation = outcome.pin.generation_id().to_owned();

    fs::remove_file(index_root(temp.path()).join("active-generation.json")).unwrap();
    let (value, collection, index) = search_existing_generation(
        &request(RefreshArg::Off),
        outcome.pin.into_index(),
        temp.path(),
        0.35,
        outcome.status,
        outcome.source_count,
    )
    .unwrap();

    assert_eq!(index.generation_id(), generation);
    assert_eq!(value["retrieval"]["generation_id"], generation);
    assert_eq!(collection.result_window.hits.len(), 1);
}

#[test]
fn two_rotations_keep_full_id_machine_reads_pinned_and_retry_compact_or_human_reads() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let machine_pin = open_index(temp.path()).unwrap();
    let compact_pin = open_index(temp.path()).unwrap();
    let human_pin = open_index(temp.path()).unwrap();
    let session_id = machine_pin
        .sessions_by_provider_session_id(TEST_SESSION_ID, Some("codex"), None, None)
        .unwrap()[0]
        .session_id
        .as_uuid();

    let second = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 92, 1),
        "second generation",
    );
    append_fixture_session(temp.path(), &[second], 92);
    let third = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 93, 1),
        "third generation",
    );
    append_fixture_session(temp.path(), &[third], 93);

    let mut machine_request = request(RefreshArg::Off);
    machine_request.session = Some(session_id.to_string());
    machine_request.events = true;
    let (value, collection, pinned) = search_existing_generation(
        &machine_request,
        machine_pin,
        temp.path(),
        0.35,
        "existing_generation",
        1,
    )
    .unwrap();
    assert_eq!(collection.result_window.hits.len(), 1);
    assert_eq!(
        value["results"][0]["ctx_session_id"],
        session_id.to_string()
    );
    assert_ne!(
        pinned.generation_id(),
        open_index(temp.path()).unwrap().generation_id()
    );

    let mut compact_request = request(RefreshArg::Off);
    compact_request.session = Some(session_id.simple().to_string()[..8].to_owned());
    compact_request.events = true;
    let compact_error = search_existing_generation(
        &compact_request,
        compact_pin,
        temp.path(),
        0.35,
        "existing_generation",
        1,
    )
    .err()
    .expect("expired compact input must request a concurrent-generation retry");
    assert!(super::shared::is_active_generation_race(&compact_error));

    let human_error = generation_with_retained_peer(temp.path(), human_pin)
        .err()
        .expect("expired human presentation must request a concurrent-generation retry");
    assert!(super::shared::is_active_generation_race(&human_error));
}

#[test]
fn limit_200_search_reduces_each_large_core_body_before_retaining_presentations() {
    let temp = tempdir().unwrap();
    let body = format!("{} {TEST_QUERY}", "x".repeat(96 * 1024));
    let body_bytes = body.len();
    assert!(body_bytes * ctx_history_read_application::MAX_SEARCH_RESULTS > MAX_CORE_CONTENT_BYTES);
    let events = (1..=ctx_history_read_application::MAX_SEARCH_RESULTS)
        .map(|sequence| {
            let event = fixture_event(
                CaptureProvider::Codex,
                "codex_session_jsonl",
                91,
                sequence as u64,
            );
            fixture_core_event(&event, body.clone())
        })
        .collect::<Vec<_>>();
    append_fixture_session(temp.path(), &events, 91);

    let mut source_request = request(RefreshArg::Off);
    source_request.events = true;
    source_request.limit = ctx_history_read_application::MAX_SEARCH_RESULTS;
    let (value, collection, _) = search_existing_generation(
        &source_request,
        open_index(temp.path()).unwrap(),
        temp.path(),
        source_request.semantic_weight,
        "existing_generation",
        1,
    )
    .unwrap();
    assert_eq!(
        collection.result_window.hits.len(),
        ctx_history_read_application::MAX_SEARCH_RESULTS
    );
    assert!(!collection.result_window.more_available);
    let results = value["results"].as_array().unwrap();
    let retained_snippet_bytes = results
        .iter()
        .map(|result| result["snippet"].as_str().unwrap().len())
        .sum::<usize>();
    assert!(
        retained_snippet_bytes
            <= ctx_history_read_application::SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES
    );
    assert_eq!(
        results.len(),
        ctx_history_read_application::MAX_SEARCH_RESULTS
    );
    assert!(results.iter().all(|result| {
        !result["snippet"].as_str().unwrap().is_empty()
            && result["snippet"].as_str().unwrap().chars().count() <= SEARCH_SNIPPET_MAX_CHARS
            && result["snippet"].as_str().unwrap().contains(TEST_QUERY)
            && result["snippet"].as_str().unwrap().len() < body_bytes
            && result["snippet_truncated"] == true
            && result["snippet_max_chars"] == SEARCH_SNIPPET_MAX_CHARS
    }));
    assert_eq!(
        value["result_window"]["returned"],
        ctx_history_read_application::MAX_SEARCH_RESULTS
    );
    assert_eq!(value["result_window"]["more_available"], false);
}

include!("search_presentation_hydration.rs");

#[test]
fn search_context_bytes_use_core_snippets_and_indexed_complete_session_sizes_not_json() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let (value, collection, index) = search_existing_generation(
        &request(RefreshArg::Off),
        open_index(temp.path()).unwrap(),
        temp.path(),
        0.35,
        "existing_generation",
        1,
    )
    .unwrap();
    let observation = search_context_observation(&value, &collection, &index);
    let delivered = value["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["snippet"].as_str().unwrap().len())
        .sum::<usize>();
    let session_id = collection.result_window.hits[0].event.session_id;
    let complete_bytes = index
        .core_events_for_session(session_id)
        .unwrap()
        .iter()
        .map(|event| {
            event
                .core_record
                .content
                .normalized_body
                .as_ref()
                .map_or(0, String::len)
                + event
                    .core_record
                    .content
                    .structured_content
                    .as_ref()
                    .map(|value| serde_json::to_vec(value).unwrap().len())
                    .unwrap_or(0)
                + event
                    .core_record
                    .content
                    .activity
                    .as_ref()
                    .map(|activity| serde_json::to_vec(activity).unwrap().len())
                    .unwrap_or(0)
        })
        .sum::<usize>();
    assert_eq!(
        observation.complete_byte_totals(),
        Some((delivered as u64, complete_bytes as u64))
    );
    assert_ne!(
        delivered,
        serde_json::to_vec(&value["results"]).unwrap().len(),
        "JSON keys and framing must not enter canonical context bytes"
    );
}

#[test]
fn generation_only_semantic_is_typed_and_hybrid_falls_back_without_exact_projection() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();
    assert!(!temp.path().join("work.sqlite").exists());

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
        fallback
            .semantic_fallback
            .as_ref()
            .and_then(|value| value.reason.map(semantic_reason_code)),
        Some("semantic_store_missing")
    );
    assert_eq!(fallback.result_window.hits.len(), 1);

    let mut semantic_request = request(RefreshArg::Off);
    semantic_request.backend = Some(SearchBackendArg::Semantic);
    let filters = index_search_filters(&semantic_request, &index).unwrap();
    let missing =
        collect_search_hits_with_backend(&semantic_request, &index, temp.path(), 0.35, &filters)
            .unwrap_err();
    let not_ready = missing
        .downcast_ref::<SemanticNotReady>()
        .expect("semantic-only errors remain typed");
    assert_eq!(not_ready.code(), "semantic_store_missing");
    assert!(not_ready.detail().contains("flat-F32"));

    assert!(
        !temp.path().join("work.sqlite").exists(),
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
    assert!(!temp.path().join("work.sqlite").exists());
}

#[test]
fn zero_weight_hybrid_skips_semantic_fallback_for_lexical_only_filters() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();

    for (content_scope, event_type) in [
        (SearchContentScope::Calls, None),
        (SearchContentScope::Outputs, None),
        (SearchContentScope::All, Some("tool_call".to_owned())),
    ] {
        let mut request = request(RefreshArg::Off);
        request.backend = Some(SearchBackendArg::Hybrid);
        request.semantic_weight = 0.0;
        request.content_scope = content_scope;
        request.event_type = event_type;
        let filters = index_search_filters(&request, &index).unwrap();

        let collection = collect_search_hits_with_backend_using(
            &request,
            &index,
            temp.path(),
            0.0,
            &filters,
            |_index, _data_root, _query, _filters, _candidate_limit| {
                panic!("zero-weight hybrid must not enter semantic retrieval")
            },
        )
        .unwrap();

        assert_eq!(collection.requested_backend, SearchBackendArg::Hybrid);
        assert_eq!(collection.effective_backend, SearchBackendArg::Lexical);
        assert_eq!(collection.semantic_weight, 0.0);
        assert_eq!(collection.semantic_status, "skipped");
        assert!(collection.semantic_fallback.is_none());
        assert!(collection.semantic_diagnostics.is_none());
    }
}

#[test]
fn all_and_transcript_preserve_the_hybrid_semantic_lane() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();

    for content_scope in [SearchContentScope::All, SearchContentScope::Transcript] {
        let mut hybrid_request = request(RefreshArg::Off);
        hybrid_request.backend = Some(SearchBackendArg::Hybrid);
        hybrid_request.content_scope = content_scope;
        let filters = index_search_filters(&hybrid_request, &index).unwrap();
        let semantic_called = Cell::new(false);
        let collection = collect_search_hits_with_backend_using(
            &hybrid_request,
            &index,
            temp.path(),
            hybrid_request.semantic_weight,
            &filters,
            |_index, _data_root, _query, _filters, _candidate_limit| {
                semantic_called.set(true);
                Ok((Vec::new(), json!({"fixture": "semantic-lane"})))
            },
        )
        .unwrap();

        assert!(semantic_called.get(), "scope {content_scope:?}");
        assert_eq!(collection.requested_backend, SearchBackendArg::Hybrid);
        assert_eq!(collection.effective_backend, SearchBackendArg::Hybrid);
        assert_eq!(collection.semantic_status, "ready");
        assert!(collection.semantic_fallback.is_none());
    }
}

#[test]
fn calls_and_outputs_make_hybrid_lexical_with_truthful_json_metadata() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();

    for (content_scope, expected_name) in [
        (SearchContentScope::Calls, "calls"),
        (SearchContentScope::Outputs, "outputs"),
    ] {
        let mut hybrid_request = request(RefreshArg::Off);
        hybrid_request.backend = Some(SearchBackendArg::Hybrid);
        hybrid_request.content_scope = content_scope;
        let filters = index_search_filters(&hybrid_request, &index).unwrap();
        let collection = collect_search_hits_with_backend_using(
            &hybrid_request,
            &index,
            temp.path(),
            hybrid_request.semantic_weight,
            &filters,
            |_index, _data_root, _query, _filters, _candidate_limit| {
                panic!("lexical-only content scopes must not enter semantic retrieval")
            },
        )
        .unwrap();

        assert_eq!(collection.requested_backend, SearchBackendArg::Hybrid);
        assert_eq!(collection.effective_backend, SearchBackendArg::Lexical);
        assert_eq!(collection.semantic_weight, 0.0);
        assert_eq!(collection.semantic_status, "unsupported");
        assert_eq!(
            collection
                .semantic_fallback
                .as_ref()
                .and_then(|fallback| fallback.reason.map(semantic_reason_code)),
            Some("semantic_content_scope_unsupported")
        );
        assert!(collection.result_window.hits.is_empty());

        let value = search_json(
            &hybrid_request,
            temp.path(),
            &index,
            &collection,
            &filters,
            &[],
            "existing_generation",
            1,
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(value["filters"]["content_scope"], expected_name);
        assert_eq!(value["retrieval"]["requested_mode"], "hybrid");
        assert_eq!(value["retrieval"]["effective_mode"], "lexical");
        assert_eq!(value["retrieval"]["semantic_weight"], 0.0);
        assert_eq!(value["retrieval"]["semantic_status"], "unsupported");
        assert_eq!(
            value["retrieval"]["semantic_fallback_code"],
            "semantic_content_scope_unsupported"
        );
    }
}

#[test]
fn semantic_only_rejects_lexical_only_scopes_before_search() {
    let config = history_config(true, true);

    for content_scope in [SearchContentScope::Calls, SearchContentScope::Outputs] {
        let mut semantic_request = request(RefreshArg::Off);
        semantic_request.backend = Some(SearchBackendArg::Semantic);
        semantic_request.content_scope = content_scope;
        let error = resolve_source_search_backend(&semantic_request, &config)
            .expect_err("semantic-only lexical scopes must fail before search");
        let not_ready = error
            .downcast_ref::<SemanticNotReady>()
            .expect("unsupported scope must retain the typed semantic error contract");
        assert_eq!(not_ready.code(), "semantic_content_scope_unsupported");
        assert!(not_ready.detail().contains(match content_scope {
            SearchContentScope::Calls => "'calls'",
            SearchContentScope::Outputs => "'outputs'",
            SearchContentScope::All | SearchContentScope::Transcript => unreachable!(),
        }));
        assert!(!not_ready.retryable());
    }
}

#[test]
fn exact_non_message_event_type_uses_the_same_semantic_boundary() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();
    let config = history_config(true, true);
    let mut request = request(RefreshArg::Off);
    request.event_type = Some("tool_call".to_owned());
    request.backend = Some(SearchBackendArg::Hybrid);
    let filters = index_search_filters(&request, &index).unwrap();
    let collection = collect_search_hits_with_backend_using(
        &request,
        &index,
        temp.path(),
        request.semantic_weight,
        &filters,
        |_index, _data_root, _query, _filters, _candidate_limit| {
            panic!("exact non-message event types must not enter semantic retrieval")
        },
    )
    .unwrap();
    assert_eq!(collection.requested_backend, SearchBackendArg::Hybrid);
    assert_eq!(collection.effective_backend, SearchBackendArg::Lexical);
    assert_eq!(
        collection
            .semantic_fallback
            .as_ref()
            .and_then(|fallback| fallback.reason.map(semantic_reason_code)),
        Some("semantic_event_type_unsupported")
    );

    request.backend = Some(SearchBackendArg::Semantic);
    let error = resolve_source_search_backend(&request, &config)
        .expect_err("semantic-only exact non-message searches must fail before search");
    let not_ready = error
        .downcast_ref::<SemanticNotReady>()
        .expect("event type rejection must retain the typed semantic error contract");
    assert_eq!(not_ready.code(), "semantic_event_type_unsupported");
    assert!(not_ready.detail().contains("'tool_call'"));
}

#[test]
fn mcp_source_route_applies_the_semantic_config_default_to_source_generations() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let mut source_request = request(RefreshArg::Off);
    source_request.query = "query-with-no-fixture-match".to_owned();
    source_request.backend = None;

    let (lexical, _) = mcp_search(
        source_request.clone(),
        temp.path(),
        history_snapshot(true, false),
    )
    .unwrap();
    assert_eq!(lexical["retrieval"]["requested_mode"], "lexical");
    assert_eq!(lexical["retrieval"]["effective_mode"], "lexical");

    let (hybrid, _) =
        mcp_search(source_request, temp.path(), history_snapshot(true, true)).unwrap();
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
    let (file_only, _) = mcp_search(file_only, temp.path(), history_snapshot(true, false)).unwrap();
    assert_eq!(file_only["retrieval"]["requested_mode"], "lexical");
    assert_eq!(file_only["retrieval"]["effective_mode"], "lexical");
    assert!(!temp.path().join("work.sqlite").exists());
}

#[test]
fn daemon_disabled_cli_default_and_hybrid_fall_back_but_semantic_is_typed() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let config = history_config(false, true);
    let index = open_index(temp.path()).unwrap();

    for backend in [None, Some(SearchBackendArg::Hybrid)] {
        let mut source_request = request(RefreshArg::Off);
        source_request.backend = backend;
        let resolved =
            super::search::resolve_source_search_backend(&source_request, &config).unwrap();
        assert_eq!(resolved, SearchBackendArg::Hybrid);
        source_request.backend = Some(resolved);
        let filters = index_search_filters(&source_request, &index).unwrap();
        let collection = ctx_history_read_application::collect_search_hits_using(
            &source_request,
            &index,
            &filters,
            ctx_history_read_application::SemanticAvailability::Unavailable(
                ctx_history_read_application::SemanticReason::ExecutionUnavailable,
            ),
            |_query, _filters, _candidate_limit| {
                panic!("daemon-disabled hybrid must fall back before semantic work")
            },
        )
        .unwrap();
        assert_eq!(collection.requested_backend, SearchBackendArg::Hybrid);
        assert_eq!(collection.effective_backend, SearchBackendArg::Lexical);
        assert_eq!(collection.semantic_status, "unavailable");
        assert_eq!(
            collection
                .semantic_fallback
                .as_ref()
                .and_then(|fallback| fallback.reason.map(semantic_reason_code)),
            Some("semantic_daemon_disabled")
        );
        assert_eq!(collection.result_window.hits.len(), 1);
    }

    let mut semantic = request(RefreshArg::Off);
    semantic.backend = Some(SearchBackendArg::Semantic);
    let error = super::search::resolve_source_search_backend(&semantic, &config)
        .expect_err("semantic-only must still fail when the daemon is disabled");
    let not_ready = error
        .downcast_ref::<SemanticNotReady>()
        .expect("semantic-only daemon failure must remain typed");
    assert_eq!(not_ready.code(), "semantic_daemon_disabled");
}

#[test]
fn daemon_disabled_mcp_default_and_hybrid_return_lexical_fallback() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    for backend in [None, Some(SearchBackendArg::Hybrid)] {
        let mut source_request = request(RefreshArg::Off);
        source_request.backend = backend;
        let (value, _) =
            mcp_search(source_request, temp.path(), history_snapshot(false, true)).unwrap();
        assert_eq!(value["retrieval"]["requested_mode"], "hybrid");
        assert_eq!(value["retrieval"]["effective_mode"], "lexical");
        assert_eq!(
            value["retrieval"]["semantic_fallback_code"],
            "semantic_daemon_disabled"
        );
        assert_eq!(value["results"].as_array().unwrap().len(), 1);
    }

    let mut semantic = request(RefreshArg::Off);
    semantic.backend = Some(SearchBackendArg::Semantic);
    let error = mcp_search(semantic, temp.path(), history_snapshot(false, true))
        .expect_err("MCP semantic-only must fail when the daemon is disabled");
    assert!(matches!(
        error,
        McpSearchError::SemanticNotReady {
            code: "semantic_daemon_disabled",
            ..
        }
    ));
}

#[test]
fn show_json_output_limit_is_typed() {
    let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 7, 7);
    let value = json!({
        "events": [{
            "ctx_event_id": event.event_id.as_uuid(),
            "text": "Core content",
        }],
    });
    let error = enforce_json_output_limit(&value, 1, event.event_id.as_uuid()).unwrap_err();
    let typed = error
        .downcast_ref::<crate::presentation_limit::PresentationOutputLimitError>()
        .expect("show output bound should preserve the presentation-limit error");
    assert_eq!(typed.event_id, event.event_id.as_uuid());
    assert_eq!(typed.maximum_bytes, 1);
}

#[test]
fn core_show_accepts_content_beyond_16k_and_preflights_the_output_limit() {
    let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 8, 8);
    let body = format!("BEGIN-{}-END", "x".repeat(20 * 1024));
    let core_event = fixture_core_event(&event, &body);

    let rendered = render_event_values(
        &[&core_event],
        crate::presentation_limit::CLI_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    assert_eq!(rendered[0]["text"], body);

    let error = render_event_values(&[&core_event], 1024).unwrap_err();
    let typed = error
        .downcast_ref::<crate::presentation_limit::PresentationOutputLimitError>()
        .expect("Core body should be bounded before event JSON construction");
    assert_eq!(typed.event_id, event.event_id.as_uuid());
    assert_eq!(typed.maximum_bytes, 1024);
    assert!(typed.actual_bytes > 20 * 1024);
}

#[test]
fn bounded_mcp_show_returns_prefix_and_event_window_stays_body_bounded() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let first = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 70, 1),
        "selected-small-body",
    );
    let oversized = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 70, 2),
        format!("NONSELECTED-LARGE-{}", "x".repeat(8 * 1024)),
    );
    append_fixture_session(temp.path(), &[first.clone(), oversized.clone()], 70);
    let index = open_index(temp.path()).unwrap();
    let session = SessionRecord::from(&first.event);

    let session_value = mcp_show_session(
        temp.path(),
        &session.session_id.as_uuid().to_string(),
        TranscriptMode::Log,
        1,
        None,
        4 * 1024,
    )
    .unwrap();
    assert_eq!(session_value["events"].as_array().unwrap().len(), 1);
    assert_eq!(
        session_value["events"][0]["ctx_event_id"],
        first.event_id.as_uuid().to_string()
    );
    assert_eq!(session_value["pagination"]["has_more"], true);

    let shown = ctx_history_read_application::PinnedHistoryQuery::new(&index, None)
        .show_event(&ctx_history_read_application::ShowEventRequest {
            selector: first.event_id.to_string(),
            before: 0,
            after: 0,
            window: None,
            budget: ctx_history_read_application::EventWindowBudget {
                maximum_events: ctx_history_index::MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
                maximum_encoded_core_bytes: ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
                maximum_content_bytes: 2 * 1024,
            },
        })
        .unwrap();
    assert_eq!(shown.events.len(), 1);
    assert_eq!(shown.events[0].event_id, first.event_id);
}

#[test]
fn bounded_show_reports_the_first_oversized_selected_core_body_deterministically() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let first = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 71, 1),
        "selected-small-body",
    );
    let oversized = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 71, 2),
        format!("SELECTED-LARGE-{}", "y".repeat(8 * 1024)),
    );
    append_fixture_session(temp.path(), &[first.clone(), oversized.clone()], 71);
    let session = SessionRecord::from(&first.event);

    let first_page = mcp_show_session(
        temp.path(),
        &session.session_id.as_uuid().to_string(),
        TranscriptMode::Log,
        2,
        None,
        4 * 1024,
    )
    .unwrap();
    assert_eq!(first_page["pagination"]["returned"], 1);
    assert_eq!(first_page["pagination"]["has_more"], true);
    let cursor = first_page["pagination"]["next_cursor"]
        .as_str()
        .expect("the bounded prefix should include a continuation cursor")
        .to_owned();

    for error in (0..2).map(|_| {
        mcp_show_session(
            temp.path(),
            &session.session_id.as_uuid().to_string(),
            TranscriptMode::Log,
            2,
            Some(&cursor),
            4 * 1024,
        )
        .unwrap_err()
    }) {
        let typed = error
            .downcast_ref::<crate::presentation_limit::PresentationOutputLimitError>()
            .expect("oversized selected Core body should preserve the presentation error");
        assert_eq!(typed.event_id, oversized.event_id.as_uuid());
        assert_eq!(typed.maximum_bytes, 4 * 1024);
        assert!(typed.actual_bytes > typed.maximum_bytes);
    }
}

#[test]
fn bounded_show_enforces_one_cumulative_encoded_core_budget_across_batches() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let first = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 72, 1),
        format!("FIRST-ENCODED-{}", "a".repeat(1024)),
    );
    let second = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 72, 2),
        format!("SECOND-ENCODED-{}", "b".repeat(1024)),
    );
    append_fixture_session(temp.path(), &[first.clone(), second.clone()], 72);
    let index = open_index(temp.path()).unwrap();
    let encoded_bytes = [&first, &second].map(|event| {
        index
            .core_events_by_ids_with_budget(
                &[event.event_id.as_uuid()],
                1,
                ctx_history_index::DEFAULT_CORE_EVENT_PAGE_BUDGET,
            )
            .unwrap()
            .unwrap()
            .encoded_core_bytes
    });
    let cumulative_encoded_bytes = encoded_bytes.iter().sum::<usize>();
    let encoded_limit = cumulative_encoded_bytes - 1;

    let error = ctx_history_read_application::PinnedHistoryQuery::new(&index, None)
        .show_event(&ctx_history_read_application::ShowEventRequest {
            selector: first.event_id.to_string(),
            before: 0,
            after: 1,
            window: None,
            budget: ctx_history_read_application::EventWindowBudget {
                maximum_events: 2,
                maximum_encoded_core_bytes: encoded_limit,
                maximum_content_bytes: 64 * 1024,
            },
        })
        .unwrap_err();
    let typed = error
        .downcast_ref::<ctx_history_read_application::EncodedCoreQueryLimitError>()
        .expect("cumulative encoded Core overflow should preserve its typed bound");
    assert_eq!(typed.event_id, second.event_id.as_uuid());
    assert_eq!(typed.actual_bytes, cumulative_encoded_bytes);
    assert_eq!(typed.maximum_bytes, encoded_limit);
}

#[test]
fn unbounded_cli_show_streams_valid_json_beyond_4096_events_in_order() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let events = (1..=4_105)
        .map(|sequence| {
            let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 74, sequence);
            fixture_core_event(&event, format!("huge-event-{sequence}"))
        })
        .collect::<Vec<_>>();
    append_fixture_session(temp.path(), &events, 74);
    let session = SessionRecord::from(&events[0].event);
    let (mut ui, stdout) = test_ui();

    let result = stream_cli_session(
        temp.path(),
        Some(session.session_id.to_string()),
        None,
        None,
        None,
        None,
        TranscriptMode::Log,
        OutputFormat::Json,
        None,
        None,
        &mut ui,
    )
    .unwrap();
    ui.flush().unwrap();

    assert_eq!(result.events_returned, 4_105);
    let transcript: Value = serde_json::from_slice(&stdout.bytes()).unwrap();
    let rendered = transcript["events"].as_array().unwrap();
    assert_eq!(rendered.len(), 4_105);
    assert_eq!(rendered[0]["text"], "huge-event-1");
    assert_eq!(rendered[4_104]["text"], "huge-event-4105");
    assert!(transcript.get("truncated").is_none());
}

#[test]
fn human_cli_show_stream_renders_header_events_empty_and_truncation() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());

    let events = (1..=2)
        .map(|sequence| {
            let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 78, sequence);
            fixture_core_event(&event, format!("human-event-{sequence}"))
        })
        .collect::<Vec<_>>();
    append_fixture_session(temp.path(), &events, 78);
    let session = SessionRecord::from(&events[0].event);
    let (mut ui, stdout) = test_ui();

    let result = stream_cli_session(
        temp.path(),
        Some(session.session_id.to_string()),
        None,
        None,
        None,
        None,
        TranscriptMode::Log,
        OutputFormat::Text,
        Some(1),
        None,
        &mut ui,
    )
    .unwrap();
    ui.flush().unwrap();

    assert_eq!(result.events_returned, 1);
    let rendered = String::from_utf8(stdout.bytes()).unwrap();
    assert!(rendered.contains("Session"));
    assert!(rendered.contains("human-event-1"));
    assert!(!rendered.contains("human-event-2"));
    assert!(rendered.contains("Transcript is truncated."));
    assert!(rendered.contains("Showing the first 1 events."));

    let mut filtered = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 79, 1);
    filtered.event_type = "tool_call".to_owned();
    filtered.role = None;
    let filtered = fixture_core_event(&filtered, "filtered-human-event");
    append_fixture_session(temp.path(), std::slice::from_ref(&filtered), 79);
    let filtered_session = SessionRecord::from(&filtered.event);
    let (mut ui, stdout) = test_ui();

    let result = stream_cli_session(
        temp.path(),
        Some(filtered_session.session_id.to_string()),
        None,
        None,
        None,
        None,
        TranscriptMode::Lite,
        OutputFormat::Text,
        None,
        None,
        &mut ui,
    )
    .unwrap();
    ui.flush().unwrap();

    assert_eq!(result.events_returned, 0);
    let rendered = String::from_utf8(stdout.bytes()).unwrap();
    assert!(rendered.contains("Session"));
    assert!(rendered.contains("No transcript events."));
    assert!(!rendered.contains("filtered-human-event"));
}

#[test]
fn max_events_does_not_claim_truncation_for_only_filtered_raw_events() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let events = (1..=205)
        .map(|sequence| {
            let mut event =
                fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 75, sequence);
            if sequence == 1 {
                event.role = Some("user".to_owned());
            } else {
                event.event_type = "tool_call".to_owned();
            }
            fixture_core_event(&event, format!("filtered-event-{sequence}"))
        })
        .collect::<Vec<_>>();
    append_fixture_session(temp.path(), &events, 75);
    let session = SessionRecord::from(&events[0].event);
    let (mut ui, stdout) = test_ui();

    let result = stream_cli_session(
        temp.path(),
        Some(session.session_id.to_string()),
        None,
        None,
        None,
        None,
        TranscriptMode::Full,
        OutputFormat::Json,
        Some(1),
        None,
        &mut ui,
    )
    .unwrap();
    ui.flush().unwrap();

    assert_eq!(result.events_returned, 1);
    let transcript: Value = serde_json::from_slice(&stdout.bytes()).unwrap();
    assert_eq!(transcript["events"].as_array().unwrap().len(), 1);
    assert!(transcript.get("truncated").is_none(), "{transcript:#}");
}

#[test]
fn lite_selection_carries_the_pending_assistant_across_a_page_boundary() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let events = (1..=202)
        .map(|sequence| {
            let mut event =
                fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 76, sequence);
            match sequence {
                1 | 202 => event.role = Some("user".to_owned()),
                201 => event.event_type = "tool_call".to_owned(),
                _ => {}
            }
            fixture_core_event(&event, format!("lite-event-{sequence}"))
        })
        .collect::<Vec<_>>();
    append_fixture_session(temp.path(), &events, 76);
    let session = SessionRecord::from(&events[0].event);
    let (mut ui, stdout) = test_ui();

    stream_cli_session(
        temp.path(),
        Some(session.session_id.to_string()),
        None,
        None,
        None,
        None,
        TranscriptMode::Lite,
        OutputFormat::Jsonl,
        None,
        None,
        &mut ui,
    )
    .unwrap();
    ui.flush().unwrap();

    let lines = stdout
        .bytes()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 4, "three events plus completion metadata");
    assert_eq!(lines[0]["event"]["text"], "lite-event-1");
    assert_eq!(lines[1]["event"]["text"], "lite-event-200");
    assert_eq!(lines[2]["event"]["text"], "lite-event-202");
    assert_eq!(lines[3]["payload_type"], "session_transcript_completion");
    assert_eq!(lines[3]["complete"], true);
}

#[test]
fn mcp_show_session_continues_selected_events_without_overlap() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let events = (1..=5)
        .map(|sequence| {
            let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 77, sequence);
            fixture_core_event(&event, format!("mcp-page-event-{sequence}"))
        })
        .collect::<Vec<_>>();
    append_fixture_session(temp.path(), &events, 77);
    let session_id = events[0].session_id.as_uuid().to_string();

    let first = mcp_show_session(
        temp.path(),
        &session_id,
        TranscriptMode::Log,
        2,
        None,
        TEST_MCP_OUTPUT_LIMIT,
    )
    .unwrap();
    assert_eq!(first["pagination"]["limit"], 2);
    assert_eq!(first["pagination"]["returned"], 2);
    assert_eq!(first["pagination"]["has_more"], true);
    assert_eq!(first["events"][0]["text"], "mcp-page-event-1");
    assert_eq!(first["events"][1]["text"], "mcp-page-event-2");
    let first_cursor = first["pagination"]["next_cursor"].as_str().unwrap();

    let second = mcp_show_session(
        temp.path(),
        &session_id,
        TranscriptMode::Log,
        2,
        Some(first_cursor),
        TEST_MCP_OUTPUT_LIMIT,
    )
    .unwrap();
    assert_eq!(second["pagination"]["returned"], 2);
    assert_eq!(second["pagination"]["has_more"], true);
    assert_eq!(second["events"][0]["text"], "mcp-page-event-3");
    assert_eq!(second["events"][1]["text"], "mcp-page-event-4");
    let second_cursor = second["pagination"]["next_cursor"].as_str().unwrap();

    let terminal = mcp_show_session(
        temp.path(),
        &session_id,
        TranscriptMode::Log,
        2,
        Some(second_cursor),
        TEST_MCP_OUTPUT_LIMIT,
    )
    .unwrap();
    assert_eq!(terminal["pagination"]["returned"], 1);
    assert_eq!(terminal["pagination"]["has_more"], false);
    assert!(terminal["pagination"]["next_cursor"].is_null());
    assert_eq!(terminal["events"][0]["text"], "mcp-page-event-5");

    let malformed = mcp_show_session(
        temp.path(),
        &session_id,
        TranscriptMode::Log,
        2,
        Some("not-a-valid-cursor"),
        TEST_MCP_OUTPUT_LIMIT,
    )
    .unwrap_err();
    assert!(matches!(
        malformed.downcast_ref::<IndexError>(),
        Some(IndexError::InvalidSessionEventCursorCoordinate)
    ));
}

#[test]
fn measured_output_byte_helpers_match_existing_renderers() {
    let event = json!({"ctx_event_id": "event-1"});
    assert_eq!(
        pretty_json_stdout_bytes(&event).unwrap(),
        serde_json::to_string_pretty(&event).unwrap().len() + 1
    );
    assert_eq!(stdout_body_bytes("body"), 5);
    assert_eq!(stdout_body_bytes("body\n"), 5);
}

#[test]
fn show_human_output_limit_uses_one_unbounded_canonical_measurement() {
    let value = json!({
        "target": "event",
        "events": [{
            "ctx_event_id": "01900001-0000-7000-8000-000000000002",
            "role": "assistant",
            "event_type": "message",
            "text": "a sentence that wraps differently on a narrow terminal but has one canonical bound"
        }]
    });
    let expected = render_show_document(
        &value,
        &crate::ui::RenderContext::canonical_human_measurement(),
    )
    .render_plain()
    .len();
    let narrow = render_show_document(
        &value,
        &crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
            crate::ui::StreamKind::Stdout,
            32,
        )),
    )
    .render_plain()
    .len();
    assert!(
        narrow > expected,
        "fixture must add narrow-terminal wrapping"
    );
    assert_eq!(canonical_show_output_bytes(&value), expected);
    let event_id = uuid::Uuid::parse_str("01900001-0000-7000-8000-000000000002").unwrap();
    crate::presentation_limit::enforce_presentation_output_limit(
        canonical_show_output_bytes(&value),
        expected,
        event_id,
    )
    .unwrap();
    assert!(
        crate::presentation_limit::enforce_presentation_output_limit(narrow, expected, event_id)
            .is_err(),
        "a live-width count would incorrectly reject the same logical output"
    );
}

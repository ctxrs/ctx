#[cfg(test)]
mod tests {
    mod semantic_fallback;

    use std::{
        cell::Cell,
        collections::HashMap,
        fs,
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    use ctx_history_capture::{
        provider_source_for_path, refresh_source_backed_generation,
        register_landed_source_backed_route, SourceBackedProviderRegistry,
        SourceBackedRouteSelection,
    };
    use ctx_history_core::{
        derive_event_id, derive_session_id, CertifiedSource, CoreContentPolicyStatus, CoreRecord,
        EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts,
        SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, TypedKey,
    };
    use ctx_history_index::{
        EventSearchFilters, GenerationWriter, IndexError, SessionRecord, WriterOptions,
        LEXICAL_QUERY_LIMITS,
    };
    use serde_json::{json, Value};
    use tempfile::tempdir;

    use crate::{
        commands::show::{ShowEventArgs, ShowSessionArgs},
        output::OutputFormat,
        transcript::TranscriptMode,
        ui::{RenderContext, StreamKind, TestContext, Ui},
        ShowTarget,
    };

    use super::*;
    use super::{
        render::{render_show_document, search_json, SearchCorePresentation},
        search::{
            core_records_for_search_hits_with_budget, NormalizedSearchQuery, SearchCollection,
            SearchCoreHydrationBudget, SearchCoreHydrationBudgetExceeded,
            SearchCoreHydrationBudgetStage, SearchHit, SearchResultWindow,
            SEARCH_CORE_BODY_PREFIX_CHARS, SEARCH_CORE_HYDRATION_BUDGET,
            SEARCH_CORE_MAX_RETAINED_BODY_BYTES,
        },
        show::{
            canonical_show_output_bytes, core_events_by_ids_with_presentation_limits, event_window,
            event_window_value, mcp_show_session, render_event_value, render_event_values,
            render_show_error, session_transcript_value, stream_cli_session,
            take_core_presentation_fetch_ids, validate_show_target,
            EncodedCorePresentationLimitError,
        },
    };

    mod recovery;

    const TEST_SESSION_ID: &str = "019fa000-0000-7000-8000-0000000000d1";
    const TEST_QUERY: &str = "pinnedgenerationrouting";

    include!("tests/fixtures.rs");

    #[test]
    fn normalized_query_representation_covers_terms_echo_and_safe_follow_up_arguments() {
        let mut source_request = request(RefreshArg::Off);
        source_request.query = "  build failure  ".to_owned();
        source_request.terms = vec![
            "release's checksum".to_owned(),
            "BUILD FAILURE".to_owned(),
            "   ".to_owned(),
        ];

        let normalized = NormalizedSearchQuery::from_request(&source_request);
        assert_eq!(
            normalized.texts(),
            vec!["build failure", "release's checksum", "BUILD FAILURE"]
        );
        assert_eq!(
            normalized.display(),
            "build failure OR release's checksum OR BUILD FAILURE"
        );
        assert_eq!(
            normalized.shell_arguments(),
            "'build failure' --term='release'\\''s checksum' --term='BUILD FAILURE'"
        );

        source_request.query.clear();
        source_request.terms = vec!["  term-only  ".to_owned()];
        let term_only = NormalizedSearchQuery::from_request(&source_request);
        assert_eq!(term_only.display(), "term-only");
        assert_eq!(term_only.shell_arguments(), "--term=term-only");
        source_request.terms = vec!["--option-like".to_owned()];
        assert_eq!(
            NormalizedSearchQuery::from_request(&source_request).shell_arguments(),
            "--term=--option-like"
        );
    }

    #[test]
    fn oversized_single_query_is_rejected_before_refresh_coordination() {
        let mut source_request = request(RefreshArg::Off);
        source_request.query = "x".repeat(LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes + 1);
        let coordinated = Cell::new(false);

        let error = refresh_for_search_with(
            &source_request,
            Path::new("/query-limit-test-does-not-open"),
            |_, _| {
                coordinated.set(true);
                panic!("oversized query must fail before refresh coordination")
            },
        )
        .err()
        .expect("oversized query must be rejected");

        assert!(matches!(
            error.downcast_ref::<IndexError>(),
            Some(IndexError::LexicalQueryBytesTooLarge { actual, maximum })
                if *actual == LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes + 1
                    && *maximum == LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes
        ));
        assert!(!coordinated.get());
    }

    #[test]
    fn repeated_terms_are_rejected_before_refresh_coordination() {
        let mut source_request = request(RefreshArg::Off);
        source_request.query.clear();
        source_request.terms =
            vec!["bounded".to_owned(); LEXICAL_QUERY_LIMITS.maximum_alternatives + 1];
        let coordinated = Cell::new(false);

        let error = refresh_for_search_with(
            &source_request,
            Path::new("/query-limit-test-does-not-open"),
            |_, _| {
                coordinated.set(true);
                panic!("repeated terms must fail before refresh coordination")
            },
        )
        .err()
        .expect("repeated terms must be rejected");

        assert!(matches!(
            error.downcast_ref::<IndexError>(),
            Some(IndexError::LexicalQueryAlternativesTooMany { observed, maximum })
                if *observed == LEXICAL_QUERY_LIMITS.maximum_alternatives + 1
                    && *maximum == LEXICAL_QUERY_LIMITS.maximum_alternatives
        ));
        assert!(!coordinated.get());
    }

    #[test]
    fn search_schema_v1_snapshot_reads_snippets_and_citations_from_core() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let index = open_index(temp.path()).unwrap();
        let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 31, 1);
        let event_id = event.event_id.as_uuid();
        let core_event = fixture_core_event(&event, "Core-owned search snippet");
        let mut source_request = request(RefreshArg::Off);
        source_request.query = "  primary query ".to_owned();
        source_request.terms = vec!["term with spaces".to_owned()];
        source_request.limit = 1;
        let collection = SearchCollection {
            result_window: SearchResultWindow {
                limit: 1,
                hits: vec![SearchHit {
                    event,
                    score: 1.0,
                    more_matches_in_session: 0,
                }],
                more_available: false,
            },
            candidate_pool: 1,
            candidate_pool_truncated: false,
            requested_backend: SearchBackendArg::Lexical,
            effective_backend: SearchBackendArg::Lexical,
            semantic_weight: 0.0,
            semantic_status: "skipped",
            semantic_fallback: None,
            semantic_diagnostics: None,
        };
        let follow_up_root = std::path::Path::new("/tmp/ctx root/owner's history");
        let value = search_json(
            &source_request,
            follow_up_root,
            &index,
            &collection,
            &EventSearchFilters::default(),
            &HashMap::from([(
                event_id,
                SearchCorePresentation {
                    record: core_event,
                    snippet_truncated: false,
                },
            )]),
            "existing_generation",
            1,
            std::time::Duration::ZERO,
        )
        .unwrap();

        assert_eq!(
            sorted_json_keys(&value),
            vec![
                "filters",
                "freshness",
                "generated_at",
                "payload_type",
                "phase_attribution",
                "query",
                "result_window",
                "results",
                "retrieval",
                "schema_version",
                "truncation",
            ]
        );
        assert!(
            chrono::DateTime::parse_from_rfc3339(value["generated_at"].as_str().unwrap()).is_ok()
        );
        assert_eq!(value["query"], "primary query OR term with spaces");
        assert_eq!(
            sorted_json_keys(&value["result_window"]),
            vec!["limit", "more_available", "returned"]
        );
        assert_eq!(
            value["result_window"],
            json!({
                "limit": 1,
                "returned": 1,
                "more_available": false,
            })
        );
        assert!(value.get("cursor").is_none());
        assert!(value["result_window"].get("cursor").is_none());
        let result = &value["results"][0];
        assert_eq!(result["snippet"], "Core-owned search snippet");
        assert_eq!(result["snippet_truncated"], false);
        assert!(result.get("source_path").is_none());
        assert!(result.get("source_exists").is_none());
        assert!(result.get("cursor").is_none());
        assert!(result["citations"][0].get("source_path").is_none());
        assert!(result["citations"][0].get("source_exists").is_none());
        assert!(result["citations"][0].get("cursor").is_none());
        let commands = result["suggested_next_commands"].as_array().unwrap();
        assert!(commands.iter().all(|command| {
            command.as_str().is_some_and(|command| {
                command.starts_with(r#"ctx --data-root '/tmp/ctx root/owner'\''s history' "#)
            })
        }));
        assert_eq!(
            result["suggested_next_commands"][2],
            format!(
                r#"ctx --data-root '/tmp/ctx root/owner'\''s history' search 'primary query' --term='term with spaces' --session {}"#,
                result["ctx_session_id"].as_str().unwrap()
            )
        );
    }

    #[test]
    fn search_json_rank_tracks_non_monotonic_shaped_result_order() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let index = open_index(temp.path()).unwrap();
        let first = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 41, 1);
        let second = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 42, 1);
        let first_id = first.event_id.as_uuid();
        let second_id = second.event_id.as_uuid();
        let first_core = fixture_core_event(&first, "first shaped result");
        let second_core = fixture_core_event(&second, "second shaped result");
        let mut source_request = request(RefreshArg::Off);
        source_request.limit = 2;
        let collection = SearchCollection {
            result_window: SearchResultWindow {
                limit: 2,
                hits: vec![
                    SearchHit {
                        event: first,
                        score: 0.25,
                        more_matches_in_session: 0,
                    },
                    SearchHit {
                        event: second,
                        score: 9.5,
                        more_matches_in_session: 0,
                    },
                ],
                more_available: false,
            },
            candidate_pool: 2,
            candidate_pool_truncated: false,
            requested_backend: SearchBackendArg::Lexical,
            effective_backend: SearchBackendArg::Lexical,
            semantic_weight: 0.0,
            semantic_status: "skipped",
            semantic_fallback: None,
            semantic_diagnostics: None,
        };
        let value = search_json(
            &source_request,
            temp.path(),
            &index,
            &collection,
            &EventSearchFilters::default(),
            &HashMap::from([
                (
                    first_id,
                    SearchCorePresentation {
                        record: first_core,
                        snippet_truncated: false,
                    },
                ),
                (
                    second_id,
                    SearchCorePresentation {
                        record: second_core,
                        snippet_truncated: false,
                    },
                ),
            ]),
            "existing_generation",
            1,
            std::time::Duration::ZERO,
        )
        .unwrap();

        let results = value["results"].as_array().unwrap();
        assert_eq!(results[0]["ctx_event_id"], first_id.to_string());
        assert_eq!(results[1]["ctx_event_id"], second_id.to_string());
        assert_eq!(results[0]["rank"], 1);
        assert_eq!(results[1]["rank"], 2);
        assert_eq!(results[0]["retrieval_score"], 0.25);
        assert_eq!(results[1]["retrieval_score"], 9.5);
    }

    #[test]
    fn show_schema_v1_reads_complete_normalized_core_content() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let index = open_index(temp.path()).unwrap();
        let session = index
            .sessions_by_provider_session_id(TEST_SESSION_ID, Some("codex"))
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let events = index
            .core_events_for_session(session.session_id.as_uuid())
            .unwrap();
        let selected = events.first().unwrap();

        let session_value = session_transcript_value(
            &session,
            TranscriptMode::Log,
            OutputFormat::Json,
            events.iter().map(render_event_value).collect(),
            false,
            None,
        );
        assert_eq!(
            sorted_json_keys(&session_value),
            vec![
                "ctx_session_id",
                "events",
                "format",
                "mode",
                "payload_type",
                "provider",
                "provider_session_id",
                "schema_version",
                "session",
                "target",
            ]
        );
        assert_eq!(session_value["session"]["record_type"], "session");
        assert_eq!(
            session_value["session"]["item_id"],
            session.session_id.as_uuid().to_string()
        );
        assert_eq!(session_value["provider_session_id"], TEST_SESSION_ID);
        assert!(session_value.get("source").is_none());

        let event_value = event_window_value(
            selected,
            OutputFormat::Json,
            vec![render_event_value(selected)],
        )
        .unwrap();
        assert_eq!(
            sorted_json_keys(&event_value),
            vec![
                "ctx_event_id",
                "ctx_session_id",
                "event",
                "events",
                "format",
                "payload_type",
                "schema_version",
                "target",
            ]
        );
        assert_eq!(
            sorted_json_keys(&event_value["event"]["content"]),
            vec!["complete", "policy_status"]
        );
        assert_eq!(
            event_value["event"]["content"],
            json!({
                "complete": true,
                "policy_status": "selected",
            })
        );
        assert_eq!(event_value["event"]["provider_session_id"], TEST_SESSION_ID);
        assert!(event_value["event"].get("source").is_none());
        assert!(event_value["event"].get("cursor").is_none());
    }

    #[test]
    fn show_content_completeness_follows_policy_status() {
        let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 44, 1);
        let selected = fixture_core_event(&event, "selected body");
        let mut redacted = fixture_core_event(&event, "redacted body");
        redacted.core_record.content.policy_status = CoreContentPolicyStatus::Redacted {
            reason: "sensitive".to_owned(),
        };
        redacted.core_record.validate_contract().unwrap();
        let mut omitted = fixture_core_event(&event, "omitted body");
        omitted.core_record.content.policy_status = CoreContentPolicyStatus::Omitted {
            reason: "unsupported".to_owned(),
        };
        omitted.core_record.content.normalized_body = None;
        omitted.core_record.content.structured_content = None;
        omitted.core_record.validate_contract().unwrap();

        let selected = render_event_value(&selected);
        let redacted = render_event_value(&redacted);
        let omitted = render_event_value(&omitted);

        assert_eq!(selected["content"]["complete"], true);
        assert_eq!(selected["content"]["policy_status"], "selected");
        assert_eq!(redacted["content"]["complete"], false);
        assert_eq!(redacted["content"]["policy_status"], "redacted");
        assert_eq!(redacted["content"]["policy_reason"], "sensitive");
        assert_eq!(omitted["content"]["complete"], false);
        assert_eq!(omitted["content"]["policy_status"], "omitted");
        assert_eq!(omitted["content"]["policy_reason"], "unsupported");
    }

    #[test]
    fn show_selector_shapes_validate_before_pristine_root_access() {
        for target in [
            ShowTarget::Session(show_session_args(None, None)),
            ShowTarget::Session(show_session_args(
                Some("deadbeef"),
                Some("provider-session"),
            )),
        ] {
            let error = validate_show_target(&target).unwrap_err().to_string();
            assert!(
                error.contains("requires a ctx session ID or --provider-session")
                    || error.contains("not both"),
                "{error}"
            );
            assert!(!error.contains("index is not initialized"), "{error}");
        }
        let show_identity = validate_show_target(&ShowTarget::Event(show_event_args("not-an-id")))
            .unwrap_err()
            .to_string();
        assert!(
            show_identity.contains("event id must be"),
            "{show_identity}"
        );
        let session_identity = validate_show_target(&ShowTarget::Session(show_session_args(
            Some("not-an-id"),
            None,
        )))
        .unwrap_err()
        .to_string();
        assert!(
            session_identity.contains("session id must be"),
            "{session_identity}"
        );
        assert!(!session_identity.contains("index is not initialized"));

        let provider_identity =
            validate_show_target(&ShowTarget::Session(show_session_args(None, Some("   "))))
                .unwrap_err()
                .to_string();
        assert!(
            provider_identity.contains("provider session ID must not be empty"),
            "{provider_identity}"
        );
        assert!(!provider_identity.contains("index is not initialized"));
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
    fn refresh_off_uses_the_existing_generation_without_activation() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());

        let outcome = refresh_for_search(&request(RefreshArg::Off), temp.path()).unwrap();

        assert_eq!(outcome.status, "existing_generation");
        assert_eq!(
            outcome.pin.generation_id(),
            open_index(temp.path()).unwrap().generation_id()
        );
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
    fn limit_200_search_retains_compact_query_centered_core_projections() {
        let temp = tempdir().unwrap();
        let body = format!("{} {TEST_QUERY}", "🦀".repeat(4_100));
        let body_bytes = body.len();
        let events = (1..=crate::MAX_SEARCH_LIMIT)
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
        source_request.limit = crate::MAX_SEARCH_LIMIT;
        let index = open_index(temp.path()).unwrap();
        let filters = index_search_filters(&source_request, &index).unwrap();
        let collection = collect_search_hits_with_backend(
            &source_request,
            &index,
            temp.path(),
            source_request.semantic_weight,
            &filters,
        )
        .unwrap();
        assert_eq!(collection.result_window.hits.len(), crate::MAX_SEARCH_LIMIT);
        assert!(!collection.result_window.more_available);
        let normalized_query = NormalizedSearchQuery::from_request(&source_request);

        let core_records = core_records_for_search_hits_with_budget(
            &index,
            &collection.result_window.hits,
            &normalized_query,
            SEARCH_CORE_HYDRATION_BUDGET,
        )
        .unwrap();
        let retained_body_bytes = core_records
            .values()
            .map(|record| {
                record
                    .record
                    .core_record
                    .content
                    .normalized_body
                    .as_ref()
                    .map_or(0, String::len)
            })
            .sum::<usize>();
        assert!(retained_body_bytes <= SEARCH_CORE_MAX_RETAINED_BODY_BYTES);
        assert!(core_records.values().all(|record| {
            let projected = record.record.core_record.content.normalized_body.as_deref();
            record.snippet_truncated
                && projected.is_some_and(|projected| {
                    projected.chars().count() == SEARCH_CORE_BODY_PREFIX_CHARS
                        && projected.contains(TEST_QUERY)
                        && projected.len() < body_bytes
                })
                && record
                    .record
                    .core_record
                    .content
                    .structured_content
                    .is_none()
                && record.record.core_record.metadata.is_empty()
                && record.record.core_record.repository_bindings.is_empty()
                && record
                    .record
                    .core_record
                    .repository_file_observations
                    .is_empty()
                && record
                    .record
                    .core_record
                    .repository_vcs_observations
                    .is_empty()
        }));

        let value = search_json(
            &source_request,
            temp.path(),
            &index,
            &collection,
            &filters,
            &core_records,
            "existing_generation",
            1,
            std::time::Duration::ZERO,
        )
        .unwrap();
        let results = value["results"].as_array().unwrap();
        assert_eq!(results.len(), crate::MAX_SEARCH_LIMIT);
        assert!(results.iter().all(|result| {
            result["snippet"].as_str().unwrap().chars().count() == SEARCH_CORE_BODY_PREFIX_CHARS
                && result["snippet"].as_str().unwrap().contains(TEST_QUERY)
                && result["snippet_truncated"] == true
                && result["snippet_max_chars"] == SEARCH_CORE_BODY_PREFIX_CHARS
        }));
        assert_eq!(value["result_window"]["returned"], crate::MAX_SEARCH_LIMIT);
        assert_eq!(value["result_window"]["more_available"], false);

        let decode_error = core_records_for_search_hits_with_budget(
            &index,
            &collection.result_window.hits,
            &normalized_query,
            SearchCoreHydrationBudget {
                maximum_encoded_core_bytes: SEARCH_CORE_HYDRATION_BUDGET.maximum_encoded_core_bytes,
                maximum_content_bytes: body_bytes.checked_mul(crate::MAX_SEARCH_LIMIT).unwrap() - 1,
                maximum_retained_body_bytes: SEARCH_CORE_HYDRATION_BUDGET
                    .maximum_retained_body_bytes,
            },
        )
        .unwrap_err();
        let typed = decode_error
            .downcast_ref::<SearchCoreHydrationBudgetExceeded>()
            .expect("aggregate decode failure must stay typed");
        assert_eq!(typed.stage, SearchCoreHydrationBudgetStage::Decode);
        assert_eq!(
            typed.maximum_content_bytes,
            body_bytes * crate::MAX_SEARCH_LIMIT - 1
        );

        let retention_error = core_records_for_search_hits_with_budget(
            &index,
            &collection.result_window.hits,
            &normalized_query,
            SearchCoreHydrationBudget {
                maximum_retained_body_bytes: retained_body_bytes - 1,
                ..SEARCH_CORE_HYDRATION_BUDGET
            },
        )
        .unwrap_err();
        let typed = retention_error
            .downcast_ref::<SearchCoreHydrationBudgetExceeded>()
            .expect("aggregate retention failure must stay typed");
        assert_eq!(typed.stage, SearchCoreHydrationBudgetStage::Retention);
        assert_eq!(typed.retained_body_bytes, retained_body_bytes);
    }

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
        let session_id = collection.result_window.hits[0].event.session_id.as_uuid();
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
            })
            .sum::<usize>();
        assert_eq!(
            observation.metadata_for_test(),
            (
                crate::local_usage::ContextCoverage::Complete,
                delivered as u64,
                complete_bytes as u64,
            )
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
    fn mcp_source_route_applies_the_semantic_config_default_to_source_generations() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let mut source_request = request(RefreshArg::Off);
        source_request.query = "query-with-no-fixture-match".to_owned();
        source_request.backend = None;

        let (lexical, _) = mcp_search(source_request.clone(), temp.path()).unwrap();
        assert_eq!(lexical["retrieval"]["requested_mode"], "lexical");
        assert_eq!(lexical["retrieval"]["effective_mode"], "lexical");

        config::set_semantic_search_enabled(temp.path(), true).unwrap();
        let (hybrid, _) = mcp_search(source_request, temp.path()).unwrap();
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
        let (file_only, _) = mcp_search(file_only, temp.path()).unwrap();
        assert_eq!(file_only["retrieval"]["requested_mode"], "lexical");
        assert_eq!(file_only["retrieval"]["effective_mode"], "lexical");
        assert!(!temp.path().join("work.sqlite").exists());
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

        take_core_presentation_fetch_ids();
        let window = event_window(&index, &first, 0, 0, None, 2 * 1024).unwrap();
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].event_id, first.event_id);
        assert_eq!(
            take_core_presentation_fetch_ids(),
            vec![first.event_id.as_uuid()],
            "the nonselected large window body must never be requested for Core decode"
        );
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

        let error = core_events_by_ids_with_presentation_limits(
            &index,
            &[first.event_id.as_uuid(), second.event_id.as_uuid()],
            2,
            64 * 1024,
            encoded_limit,
        )
        .unwrap_err();
        let typed = error
            .downcast_ref::<EncodedCorePresentationLimitError>()
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
                let event =
                    fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 74, sequence);
                fixture_core_event(&event, format!("huge-event-{sequence}"))
            })
            .collect::<Vec<_>>();
        append_fixture_session(temp.path(), &events, 74);
        let index = open_index(temp.path()).unwrap();
        let session = SessionRecord::from(&events[0].event);
        let (mut ui, stdout) = test_ui();

        let result = stream_cli_session(
            &index,
            &session,
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
        let index = open_index(temp.path()).unwrap();
        let session = SessionRecord::from(&events[0].event);
        let (mut ui, stdout) = test_ui();

        let result = stream_cli_session(
            &index,
            &session,
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
        let index = open_index(temp.path()).unwrap();
        let session = SessionRecord::from(&events[0].event);
        let (mut ui, stdout) = test_ui();

        stream_cli_session(
            &index,
            &session,
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
                let event =
                    fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 77, sequence);
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
            crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
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
            crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
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
            crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
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
            crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
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
            crate::presentation_limit::enforce_presentation_output_limit(
                narrow, expected, event_id
            )
            .is_err(),
            "a live-width count would incorrectly reject the same logical output"
        );
    }
}

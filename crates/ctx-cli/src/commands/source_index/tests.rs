#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::HashMap,
        fs,
    };

    use ctx_history_capture::ingest_codex_source_backed_v0;
    use ctx_history_core::{
        database_path, derive_event_id, derive_session_id, CertifiedSource, EventIdentityInput,
        LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
        ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
        SourceRecordLocator, TypedKey,
    };
    use ctx_history_index::{
        EventSearchFilters, GenerationWriter, LexicalDocument, WriterOptions,
    };
    use serde_json::json;
    use tempfile::tempdir;

    use crate::{
        commands::show::{ShowEventArgs, ShowSessionArgs},
        output::OutputFormat,
        transcript::TranscriptMode,
        ShowTarget,
    };

    use super::*;
    use super::{
        render::{render_show_document, search_json},
        search::{
            NormalizedSearchQuery, SearchCollection, SearchHit, SearchResultWindow,
        },
        show::{
            canonical_show_output_bytes, event_window_value, render_event_value,
            render_event_values,
            session_transcript_value, validate_show_target,
        },
    };

    const TEST_SESSION_ID: &str = "019fa000-0000-7000-8000-0000000000d1";
    const TEST_QUERY: &str = "pinnedgenerationrouting";

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

    fn fixture_core_event(event: &EventRecord, body: impl Into<String>) -> CoreEventRecord {
        let document = LexicalDocument {
            event_id: event.event_id,
            session_id: event.session_id,
            parent_session_id: event.parent_session_id,
            root_session_id: event.root_session_id,
            source: event.locator.source().clone(),
            locator: event.locator.clone(),
            provider_session_id: event.provider_session_id.clone(),
            branch: event.branch.clone(),
            source_path: event.source_path.clone(),
            agent_type: event.agent_type.clone(),
            is_primary: event.is_primary,
            event_sequence: event.event_sequence,
            occurred_at_unix_ms: event.occurred_at_unix_ms,
            event_type: event.event_type.clone(),
            role: event.role.clone(),
            body: body.into(),
            workspace: event.workspace.clone(),
            cwd: event.cwd.clone(),
            touched_files: event.touched_files.clone(),
        };
        CoreEventRecord {
            event: event.clone(),
            core_record: document.to_core_record().unwrap(),
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

    fn sorted_json_keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys = value
            .as_object()
            .expect("schema snapshot target must be an object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }

    fn show_session_args(id: Option<&str>, provider_session: Option<&str>) -> ShowSessionArgs {
        ShowSessionArgs {
            id: id.map(str::to_owned),
            provider: None,
            provider_session: provider_session.map(str::to_owned),
            mode: TranscriptMode::Lite,
            format: OutputFormat::Json,
            out: None,
        }
    }

    fn show_event_args(id: &str) -> ShowEventArgs {
        ShowEventArgs {
            id: id.to_owned(),
            before: 0,
            after: 0,
            window: None,
            format: OutputFormat::Json,
        }
    }

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
            &HashMap::from([(event_id, core_event)]),
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
                (first_id, first_core),
                (second_id, second_core),
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
            events
                .iter()
                .map(render_event_value)
                .collect(),
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
        assert!(session_identity.contains("session id must be"), "{session_identity}");
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

        fs::remove_file(index_root(temp.path()).join("meta.json")).unwrap();
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
    fn search_context_bytes_use_core_snippets_and_complete_core_sessions_not_json() {
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
                event.core_record.content.normalized_body.as_ref().map_or(0, String::len)
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
        assert!(!database_path(temp.path().to_path_buf()).exists());
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

        let error = render_event_values(&[&core_event], 1024)
            .unwrap_err();
        let typed = error
            .downcast_ref::<crate::presentation_limit::PresentationOutputLimitError>()
            .expect("Core body should be bounded before event JSON construction");
        assert_eq!(typed.event_id, event.event_id.as_uuid());
        assert_eq!(typed.maximum_bytes, 1024);
        assert!(typed.actual_bytes > 20 * 1024);
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
}

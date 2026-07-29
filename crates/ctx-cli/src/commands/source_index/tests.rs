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
    use ctx_history_index::{
        EventSearchFilters, GenerationWriter, LexicalDocument, SessionRecord, WriterOptions,
    };
    use serde_json::json;
    use tempfile::tempdir;

    use crate::{
        cli::{LocateEventArgs, LocateSessionArgs},
        commands::show::{ShowEventArgs, ShowSessionArgs},
        output::{JsonOutputFormat, OutputFormat},
        transcript::TranscriptMode,
        LocateTarget, ShowTarget,
    };

    use super::*;
    use super::{
        locate::{locate_event_value, locate_session_value, validate_locate_target},
        render::{render_locate_event_availability_text, render_search_text, search_json},
        search::{NormalizedSearchQuery, SearchCollection, SearchHit, SearchResultWindow},
        shared::session_source_json,
        show::{
            event_window_value, render_event_value, session_transcript_value, validate_show_target,
        },
    };

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
            content: ContentPolicy::Indexed,
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
            content: ContentPolicy::Indexed,
            format: OutputFormat::Json,
        }
    }

    fn locate_session_target(id: Option<&str>, provider_session: Option<&str>) -> LocateTarget {
        LocateTarget::Session(LocateSessionArgs {
            id: id.map(str::to_owned),
            provider: None,
            provider_session: provider_session.map(str::to_owned),
            format: JsonOutputFormat::Json,
        })
    }

    fn locate_event_target(id: &str) -> LocateTarget {
        LocateTarget::Event(LocateEventArgs {
            id: id.to_owned(),
            format: JsonOutputFormat::Json,
        })
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
        assert_eq!(
            render_search_text(
                &json!({
                    "query": term_only.display(),
                    "results": [],
                }),
                false,
            ),
            "no results for term-only\n"
        );

        source_request.terms = vec!["--option-like".to_owned()];
        assert_eq!(
            NormalizedSearchQuery::from_request(&source_request).shell_arguments(),
            "--term=--option-like"
        );
    }

    #[test]
    fn search_schema_v1_snapshot_has_timestamp_result_window_and_source_provenance() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let index = open_index(temp.path()).unwrap();
        let source_path = temp.path().join("search-source.jsonl");
        fs::write(&source_path, "source").unwrap();
        let mut event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 31, 1);
        event.source_path = Some(source_path.display().to_string());
        let event_id = event.event_id.as_uuid();
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
        let value = search_json(
            &source_request,
            &index,
            &collection,
            &EventSearchFilters::default(),
            &HashMap::from([(event_id, "hydrated source snippet".to_owned())]),
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
        assert_eq!(result["source_path"], source_path.display().to_string());
        assert_eq!(result["source_exists"], true);
        assert!(result.get("cursor").is_none());
        assert_eq!(result["citations"][0]["source_exists"], true);
        assert!(result["citations"][0].get("cursor").is_none());
        assert_eq!(
            result["suggested_next_commands"][2],
            format!(
                "ctx search 'primary query' --term='term with spaces' --session {}",
                result["ctx_session_id"].as_str().unwrap()
            )
        );
    }

    #[test]
    fn show_schema_v1_snapshots_restore_source_and_content_shapes() {
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
            .events_for_session(session.session_id.as_uuid())
            .unwrap();
        let selected = events.first().unwrap();

        let session_value = session_transcript_value(
            &session,
            TranscriptMode::Log,
            ContentPolicy::Indexed,
            OutputFormat::Json,
            session_source_json(&session, events.first()),
            events
                .iter()
                .map(|event| {
                    render_event_value(
                        event,
                        "injected provider-authoritative content".to_owned(),
                        ContentPolicy::Indexed,
                    )
                })
                .collect(),
            false,
            None,
        );
        assert_eq!(
            sorted_json_keys(&session_value),
            vec![
                "content_policy",
                "ctx_session_id",
                "events",
                "format",
                "mode",
                "payload_type",
                "provider",
                "provider_session_id",
                "schema_version",
                "session",
                "source",
                "target",
            ]
        );
        assert_eq!(session_value["session"]["record_type"], "session");
        assert_eq!(
            session_value["session"]["item_id"],
            session.session_id.as_uuid().to_string()
        );
        assert_eq!(session_value["source"]["exists"], true);

        for policy in [ContentPolicy::Indexed, ContentPolicy::Complete] {
            let event_value = event_window_value(
                selected,
                policy,
                OutputFormat::Json,
                vec![render_event_value(
                    selected,
                    "injected provider-authoritative content".to_owned(),
                    policy,
                )],
            )
            .unwrap();
            assert_eq!(
                sorted_json_keys(&event_value),
                vec![
                    "content_policy",
                    "ctx_event_id",
                    "ctx_session_id",
                    "event",
                    "events",
                    "format",
                    "payload_type",
                    "schema_version",
                    "source",
                    "target",
                ]
            );
            assert_eq!(
                sorted_json_keys(&event_value["event"]["content"]),
                vec![
                    "complete",
                    "complete_content_available",
                    "origin",
                    "requested",
                    "source_verified",
                    "stored_truncated",
                ]
            );
            assert_eq!(
                event_value["event"]["content"],
                json!({
                    "requested": policy.as_str(),
                    "complete": true,
                    "origin": "provider_source",
                    "stored_truncated": false,
                    "source_verified": true,
                    "complete_content_available": true,
                })
            );
            assert_eq!(event_value["event"]["source"]["exists"], true);
            assert!(event_value["source"].get("cursor").is_none());
            assert!(event_value["event"].get("cursor").is_none());
        }
    }

    #[test]
    fn locate_schema_v1_exposes_safe_provenance_and_deleted_source_availability() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("deleted-source.jsonl");
        fs::write(&source_path, "source").unwrap();
        let mut event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 32, 1);
        event.source_path = Some(source_path.display().to_string());
        let session = SessionRecord {
            session_id: event.session_id,
            parent_session_id: event.parent_session_id,
            root_session_id: event.root_session_id,
            provider: event.provider.clone(),
            source_format: event.source_format.clone(),
            provider_session_id: event.provider_session_id.clone(),
            branch: event.branch.clone(),
            source_path: event.source_path.clone(),
            agent_type: event.agent_type.clone(),
            is_primary: event.is_primary,
            workspace: event.workspace.clone(),
            cwd: event.cwd.clone(),
            first_event_sequence: event.event_sequence,
            first_occurred_at_unix_ms: event.occurred_at_unix_ms,
        };
        let session_value = locate_session_value(&session, Some(&event));
        assert_eq!(
            sorted_json_keys(&session_value),
            vec![
                "agent_type",
                "ctx_session_id",
                "payload_type",
                "provider",
                "provider_session_id",
                "resume",
                "root_ctx_session_id",
                "schema_version",
                "source",
                "target",
            ]
        );
        assert_eq!(
            session_value["source"]["source_id"],
            event.locator.source().identity().as_uuid().to_string()
        );
        assert_eq!(session_value["source"]["exists"], true);

        let present = locate_event_value(&event);
        assert_eq!(
            sorted_json_keys(&present),
            vec![
                "complete_content",
                "ctx_event_id",
                "ctx_session_id",
                "event_type",
                "payload_type",
                "provider",
                "provider_session_id",
                "resume",
                "role",
                "schema_version",
                "sequence",
                "source",
                "source_record",
                "target",
            ]
        );
        assert_eq!(
            sorted_json_keys(&present["source"]),
            vec![
                "exists",
                "path",
                "provider",
                "provider_session_id",
                "source_format",
                "source_id",
            ]
        );
        assert_eq!(
            sorted_json_keys(&present["source_record"]),
            vec!["kind", "namespace"]
        );
        assert!(present["source_record"].get("locator").is_none());
        assert!(present["source_record"].get("record_digest").is_none());
        assert_eq!(
            present["complete_content"],
            json!({
                "locator_available": true,
                "available": true,
                "source_authority": "provider",
                "source_family": "provider_native",
                "locator_kind": "provider_native",
            })
        );

        fs::remove_file(&source_path).unwrap();
        let deleted = locate_event_value(&event);
        assert_eq!(deleted["source"]["exists"], false);
        assert_eq!(deleted["complete_content"]["locator_available"], true);
        assert_eq!(deleted["complete_content"]["available"], false);
        let text = render_locate_event_availability_text(&deleted);
        assert!(text.contains("source_exists: false\n"), "{text}");
        assert!(
            text.contains("complete_content_locator_available: true\n"),
            "{text}"
        );
        assert!(
            text.contains("complete_content_available: false\n"),
            "{text}"
        );
    }

    #[test]
    fn show_and_locate_selector_shapes_validate_before_pristine_root_access() {
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
        for target in [
            locate_session_target(None, None),
            locate_session_target(Some("deadbeef"), Some("provider-session")),
        ] {
            let error = validate_locate_target(&target).unwrap_err().to_string();
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
        let locate_identity = validate_locate_target(&locate_event_target("not-an-id"))
            .unwrap_err()
            .to_string();
        assert!(
            locate_identity.contains("event id must be"),
            "{locate_identity}"
        );
        for error in [
            validate_show_target(&ShowTarget::Session(show_session_args(
                Some("not-an-id"),
                None,
            )))
            .unwrap_err()
            .to_string(),
            validate_locate_target(&locate_session_target(Some("not-an-id"), None))
                .unwrap_err()
                .to_string(),
        ] {
            assert!(error.contains("session id must be"), "{error}");
            assert!(!error.contains("index is not initialized"), "{error}");
        }
        for error in [
            validate_show_target(&ShowTarget::Session(show_session_args(None, Some("   "))))
                .unwrap_err()
                .to_string(),
            validate_locate_target(&locate_session_target(None, Some("   ")))
                .unwrap_err()
                .to_string(),
        ] {
            assert!(
                error.contains("provider session ID must not be empty"),
                "{error}"
            );
            assert!(!error.contains("index is not initialized"), "{error}");
        }
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
                "source_format": "codex_session_jsonl",
                "exists": false,
            },
            "source_record": {
                "ordinal": 4,
                "subrecord_index": 2,
            },
            "complete_content": {
                "locator_available": true,
                "available": false,
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
source_record_subrecord_index: 2\n\
source_format: codex_session_jsonl\n\
source_exists: false\n\
complete_content_locator_available: true\n\
complete_content_available: false\n";
        assert_eq!(locate_event_text_output_bytes(&event), event_text.len());
        assert_eq!(
            pretty_json_stdout_bytes(&event).unwrap(),
            serde_json::to_string_pretty(&event).unwrap().len() + 1
        );
        assert_eq!(stdout_body_bytes("body"), 5);
        assert_eq!(stdout_body_bytes("body\n"), 5);
    }
}

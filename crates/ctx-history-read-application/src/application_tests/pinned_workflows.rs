use super::*;

#[derive(Default)]
struct RecordingListStream {
    ordinals: Vec<usize>,
    page_sizes: Vec<usize>,
    completion: Option<(usize, usize, bool, bool)>,
}

impl ListEventsStreamCallback for RecordingListStream {
    type Error = Infallible;

    fn page(
        &mut self,
        page: ListEventsStreamPage<'_>,
    ) -> Result<ListEventsStreamControl, Self::Error> {
        self.ordinals.push(page.ordinal);
        self.page_sizes.push(page.page.items.len());
        Ok(ListEventsStreamControl::Continue)
    }

    fn complete(&mut self, completion: ListEventsStreamCompletion<'_>) -> Result<(), Self::Error> {
        self.completion = Some((
            completion.items,
            completion.pages,
            completion.terminal,
            completion.truncated,
        ));
        Ok(())
    }
}

struct ProjectedSemanticPort {
    candidates: Vec<EventSearchCandidate>,
    allowed_source_keys: Vec<String>,
}

struct ProjectedSemanticQuery<'a> {
    index: &'a VerifiedIndex,
    candidates: &'a [EventSearchCandidate],
    allowed_source_keys: &'a [String],
}

impl HistorySemanticPort for ProjectedSemanticPort {
    type Query<'a> = ProjectedSemanticQuery<'a>;

    fn begin_query<'a>(
        &'a self,
        index: &'a VerifiedIndex,
    ) -> Result<Self::Query<'a>, HistorySemanticError> {
        Ok(ProjectedSemanticQuery {
            index,
            candidates: &self.candidates,
            allowed_source_keys: &self.allowed_source_keys,
        })
    }
}

impl HistorySemanticQuery for ProjectedSemanticQuery<'_> {
    fn prepare_alternative(&mut self, _query: &str) -> Result<Value, HistorySemanticError> {
        Ok(json!({"adapter": "projected-test-query"}))
    }

    fn candidates(
        &mut self,
        filter: &ctx_history_index_query::CompiledSearchFilter,
        _candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError> {
        assert_eq!(
            filter.filters().allowed_source_keys.as_deref(),
            Some(self.allowed_source_keys)
        );
        let projection = self
            .index
            .semantic_filter_projection_compiled(filter)
            .unwrap();
        Ok(HistorySemanticBatch {
            candidates: self
                .candidates
                .iter()
                .filter(|candidate| projection.contains(candidate.event.event_id.as_uuid()))
                .cloned()
                .collect(),
            diagnostics: json!({"adapter": "projected-test"}),
        })
    }
}

#[test]
fn list_stream_pins_cursor_generation_once_and_summarizes_pages() {
    let temp = tempdir().unwrap();
    let (index, _) = publish(temp.path());
    let selection = CoreEventRangeSelection::all(CoreEventRangeFilters::default()).unwrap();
    let first = PinnedHistoryQuery::new(&index, None)
        .list_events_page(&ListEventsPageRequest {
            selection: selection.clone(),
            cursor: None,
            limit: 1,
            page_items: 1,
            byte_budget: ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
            strict_budget: None,
        })
        .unwrap();
    let cursor = first.page.next_cursor.unwrap();
    let mut generation =
        RecordingGenerationPort::new(VerifiedIndex::open_pinned(temp.path()).unwrap());
    let mut stream = RecordingListStream::default();
    let result = execute_list_events_stream(
        ListEventsPageRequest {
            selection,
            cursor: Some(cursor.clone()),
            limit: 2,
            page_items: 1,
            byte_budget: ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
            strict_budget: None,
        },
        &mut generation,
        &mut stream,
    )
    .unwrap();
    assert_eq!(generation.calls.get(), 1);
    assert_eq!(
        generation.target.borrow().as_ref(),
        Some(&GenerationReadTarget::Exact(
            cursor.generation_id().to_owned()
        ))
    );
    assert_eq!(stream.ordinals, [0, 1]);
    assert_eq!(stream.page_sizes, [1, 1]);
    assert_eq!(stream.completion, Some((2, 2, true, false)));
    assert_eq!(result.items, 2);
    assert_eq!(result.pages, 2);
    assert!(result.terminal);
    assert!(!result.truncated);
}

#[test]
fn semantic_and_hybrid_recall_filtered_copy_with_absent_ancestor_end_to_end() {
    let temp = tempdir().unwrap();
    let personal_source = provider_root_source("copy-personal.jsonl");
    let archive_source = provider_root_source("copy-archive.jsonl");
    let absent_ancestor = record_for_session(
        &personal_source,
        "absent-copy-ancestor",
        1,
        "user",
        "not published",
    );
    let mut copied = record_for_session(
        &personal_source,
        "copied-occurrence",
        4,
        "user",
        "recalled copied occurrence",
    );
    copied.parent_session_id = Some(absent_ancestor.session_id);
    copied.root_session_id = Some(absent_ancestor.session_id);
    copied.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
    copied.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: absent_ancestor.session_id,
        ancestor_event_id: absent_ancestor.event_id,
        proof: ProviderNativeCopyProof::NativeEventIdentity,
    });
    let mut family_peer = record_for_session(
        &personal_source,
        "same-copy-family",
        3,
        "user",
        "same family semantic peer",
    );
    family_peer.parent_session_id = Some(absent_ancestor.session_id);
    family_peer.root_session_id = Some(absent_ancestor.session_id);
    family_peer.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
    let independent = record_for_session(
        &personal_source,
        "independent-copy-recall",
        2,
        "user",
        "independent semantic peer",
    );
    let filtered_stronger = record_for_session(
        &archive_source,
        "filtered-stronger-copy",
        5,
        "user",
        "strongest semantic peer",
    );

    let archive_route = SourceRouteIdentity::from_sha256("a1".repeat(32)).unwrap();
    let personal_route = SourceRouteIdentity::from_sha256("b2".repeat(32)).unwrap();
    let definitions = vec![
        ProviderRootDefinition {
            id: "archive".to_owned(),
            provider: "codex".parse().unwrap(),
            path: temp.path().join("archive"),
            group: Some("cold".to_owned()),
            kind: None,
        },
        ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: "codex".parse().unwrap(),
            path: temp.path().join("personal"),
            group: Some("work".to_owned()),
            kind: None,
        },
    ];
    let applied_roots = vec![
        AppliedProviderRoot::new(definitions[0].clone(), vec![archive_route.clone()]).unwrap(),
        AppliedProviderRoot::new(definitions[1].clone(), vec![personal_route.clone()]).unwrap(),
    ];
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .set_applied_provider_roots(
            false,
            provider_source_config_digest(false, &definitions),
            applied_roots,
        )
        .unwrap();
    writer.begin_source(personal_source.clone()).unwrap();
    for record in [&copied, &family_peer, &independent] {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer
        .certify_source(certificate(&personal_source, 3))
        .unwrap();
    writer.begin_source(archive_source.clone()).unwrap();
    writer.add_core_record(filtered_stronger.clone()).unwrap();
    writer
        .certify_source(certificate(&archive_source, 1))
        .unwrap();
    writer
        .set_present_source_routes(vec![
            SourceRouteSnapshot::present(archive_route, vec![archive_source]).unwrap(),
            SourceRouteSnapshot::present(personal_route, vec![personal_source.clone()]).unwrap(),
        ])
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert!(index
        .event_by_id(absent_ancestor.event_id.as_uuid())
        .unwrap()
        .is_none());
    let candidate = |record: &CoreRecord, score| EventSearchCandidate {
        event: index
            .event_by_id(record.event_id.as_uuid())
            .unwrap()
            .unwrap(),
        score,
    };
    let semantic = ProjectedSemanticPort {
        candidates: vec![
            candidate(&filtered_stronger, 400.0),
            candidate(&copied, 300.0),
            candidate(&family_peer, 200.0),
            candidate(&independent, 100.0),
        ],
        allowed_source_keys: vec![source_token(&personal_source)],
    };

    for backend in [SearchBackend::Semantic, SearchBackend::Hybrid] {
        let mut request = lexical_request();
        request.query = "backend-only-copy-phantom".to_owned();
        request.limit = 2;
        request.events = false;
        request.backend = Some(backend);
        request.semantic_weight = 1.0;
        request.source_roots = vec!["personal".to_owned()];
        let plan = plan_search(
            request,
            SearchPolicy {
                default_backend: SearchBackend::Hybrid,
                semantic: SemanticAvailability::Available,
            },
        )
        .unwrap();
        let mut generation =
            RecordingGenerationPort::new(VerifiedIndex::open_pinned(temp.path()).unwrap());
        let application = execute_search(
            SearchApplicationRequest {
                plan,
                generation_target: GenerationReadTarget::Active,
                compact_projection: false,
                active_session: None,
            },
            &mut generation,
            &semantic,
        )
        .unwrap();

        assert_eq!(generation.calls.get(), 1);
        assert_eq!(
            application.query().filters.allowed_source_keys.as_ref(),
            Some(&semantic.allowed_source_keys)
        );
        assert_eq!(application.query().collection.candidate_pool, 3);
        assert_eq!(
            application.query().collection.diversification.status,
            SearchDiversificationStatus::Indeterminate
        );
        let hits = &application.query().collection.result_window.hits;
        assert_eq!(hits.len(), 2);
        assert!(application.query().collection.result_window.more_available);
        assert_eq!(hits[0].event.event_id, copied.event_id.as_uuid());
        assert_eq!(hits[0].event.session_id, copied.session_id.as_uuid());
        assert_eq!(
            hits[0].event.provider_session_id.as_deref(),
            Some("copied-occurrence")
        );
        assert_eq!(
            hits[0].event.event_copy.as_ref(),
            copied.event_copy.as_ref()
        );
        assert_eq!(hits[1].event.event_id, independent.event_id.as_uuid());
        assert_eq!(hits[1].event.session_id, independent.session_id.as_uuid());

        let commands = hits
            .iter()
            .map(|_| SearchResultCommands {
                suggested_next_commands: Vec::new(),
            })
            .collect::<Vec<_>>();
        let value = application
            .render_read_model(SearchApplicationReadModelInput {
                commands: &commands,
                freshness_mode: "test",
                generated_at: "2026-08-25T00:00:00.000Z",
                semantic_fallback_code: None,
                semantic_fallback_detail: None,
                metrics: SearchRenderMetrics {
                    refresh_status: "unchanged",
                    refresh_source_count: 2,
                    query_duration: application.query_duration(),
                },
            })
            .unwrap();
        assert_eq!(value["filters"]["source_root"], json!(["personal"]));
        let rendered = &value["results"][0];
        assert_eq!(rendered["item_id"], copied.session_id.as_uuid().to_string());
        assert_eq!(
            rendered["ctx_event_id"],
            copied.event_id.as_uuid().to_string()
        );
        assert_eq!(
            rendered["ctx_session_id"],
            copied.session_id.as_uuid().to_string()
        );
        assert_eq!(
            rendered["event_copy"]["ancestor_ctx_event_id"],
            absent_ancestor.event_id.as_uuid().to_string()
        );
        assert_eq!(rendered["event_copy"]["proof"], "native_event_identity");
        let citation = &rendered["citations"][0];
        assert_eq!(citation["item_id"], copied.event_id.as_uuid().to_string());
        assert_eq!(
            citation["ctx_session_id"],
            copied.session_id.as_uuid().to_string()
        );
    }
}

#[test]
fn one_pin_owns_search_locate_show_and_list_application_workflows() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let query = PinnedHistoryQuery::new(&index, None);

    let search = query
        .search(
            plan_search(
                lexical_request(),
                SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
            )
            .unwrap(),
            None,
            &UnusedSemanticPort,
        )
        .unwrap();
    assert_eq!(search.collection.result_window.hits.len(), 3);
    assert_eq!(search.presentations.len(), 3);

    let excluded = query
        .search(
            plan_search(
                lexical_request(),
                SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
            )
            .unwrap(),
            Some(&ActiveSessionExclusion {
                provider: "custom".to_owned(),
                provider_session_id: "pinned-session".to_owned(),
            }),
            &UnusedSemanticPort,
        )
        .unwrap();
    assert!(excluded.collection.result_window.hits.is_empty());

    let LocateResult::Event(located) = query
        .locate(&LocateRequest::Event {
            selector: records[1].event_id.to_string(),
        })
        .unwrap()
    else {
        panic!("event locate returned a session")
    };
    assert_eq!(located.event_id, records[1].event_id);

    let shown = query
        .show_event(&ShowEventRequest {
            selector: records[1].event_id.to_string(),
            before: 1,
            after: 1,
            window: None,
            budget: EventWindowBudget::default(),
        })
        .unwrap();
    assert_eq!(shown.selected.event_id, records[1].event_id);
    assert_eq!(shown.events.len(), 3);

    let session_page = query
        .show_session_page(&ShowSessionPageRequest {
            selector: Some(records[0].session_id.to_string()),
            provider_session_id: None,
            provider: None,
            provider_key: None,
            source_id: None,
            mode: SessionEventMode::Full,
            cursor: None,
            limit: 2,
            page_items: 1,
            page_budget: CoreEventPageBudget::new(
                ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
                ctx_history_core::MAX_CORE_CONTENT_BYTES,
            ),
        })
        .unwrap();
    assert_eq!(session_page.events.len(), 2);
    assert!(session_page.has_more);
    assert!(session_page.next_cursor.is_some());

    let listed = query
        .list_events(&ListEventsRequest {
            since: None,
            until: None,
            filters: CoreEventRangeFilters::default(),
            cursor: None,
            limit: 10,
            page_items: 10,
            byte_budget: ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
            strict_budget: None,
        })
        .unwrap();
    assert_eq!(listed.page.items.len(), 3);
}

#[test]
fn structured_read_models_are_composed_from_pinned_query_results() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let query = PinnedHistoryQuery::new(&index, None);

    let search = query
        .search(
            plan_search(
                lexical_request(),
                SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
            )
            .unwrap(),
            None,
            &UnusedSemanticPort,
        )
        .unwrap();
    let commands = search
        .collection
        .result_window
        .hits
        .iter()
        .map(|hit| SearchResultCommands {
            suggested_next_commands: vec![format!("adapter command {}", hit.event.event_id)],
        })
        .collect::<Vec<_>>();
    let search_value = render_search_json(SearchJsonInput {
        request: &search.request,
        index: &index,
        collection: &search.collection,
        filters: &search.filters,
        presentations: &search.presentations,
        commands: &commands,
        freshness_mode: "checkpoint",
        generated_at: "2026-08-11T12:00:00.000Z",
        semantic_fallback_code: None,
        semantic_fallback_detail: None,
        metrics: SearchRenderMetrics {
            refresh_status: "unchanged",
            refresh_source_count: 1,
            query_duration: Duration::from_millis(125),
        },
    })
    .unwrap();
    assert_eq!(search_value["schema_version"], 2);
    assert_eq!(search_value["payload_type"], "search_results");
    assert_eq!(search_value["generated_at"], "2026-08-11T12:00:00.000Z");
    assert_eq!(search_value["freshness"]["mode"], "checkpoint");
    assert_eq!(search_value["phase_attribution"]["query_seconds"], 0.125);
    assert_eq!(search_value["results"].as_array().unwrap().len(), 3);
    assert!(search_value["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|result| result.get("copied_lineage").is_none()));
    assert_eq!(
        search_value["results"][0]["suggested_next_commands"],
        json!([format!(
            "adapter command {}",
            search.collection.result_window.hits[0].event.event_id
        )])
    );

    let shown = query
        .show_event(&ShowEventRequest {
            selector: records[1].event_id.to_string(),
            before: 1,
            after: 1,
            window: None,
            budget: EventWindowBudget::default(),
        })
        .unwrap();
    let event_window = event_window_with_lineage_read_model(
        &shown.selected,
        &shown.events,
        &shown.copied_lineage,
        StructuredOutputFormat::Json,
        ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
    )
    .unwrap();
    assert_eq!(event_window["target"], "event");
    assert_eq!(event_window["event"]["text"], "needle reply");
    assert!(event_window["event"].get("event_copy").is_none());

    let compact = CompactPresentationProjection::new(&index, None)
        .project(&event_window)
        .unwrap();
    assert_ne!(
        compact["ctx_event_id"],
        shown.selected.event_id.as_uuid().to_string()
    );
    assert_eq!(
        event_window["ctx_event_id"],
        shown.selected.event_id.as_uuid().to_string()
    );

    let selection = CoreEventRangeSelection::all(CoreEventRangeFilters::default()).unwrap();
    let wire = event_query_wire_request(&selection, EventContentProjection::Text, 250);
    assert_eq!(wire.domain, json!({ "kind": "all" }));
    assert_eq!(wire.filters, json!({}));
    assert_eq!(wire.direction, "ascending");
    assert_eq!(wire.page_items(), 100);
    let receipt = serde_json::to_value(event_query_receipt(
        &index,
        &wire,
        index.generation_id(),
        None,
        false,
        true,
    ))
    .unwrap();
    assert_eq!(receipt["generation_id"], index.generation_id());
    assert_eq!(receipt["freshness"]["mode"], "pinned");
    assert_eq!(receipt["freshness"]["read_only"], true);
    assert_eq!(receipt["frontier"]["status"], "unavailable");

    let listed = query
        .list_events(&ListEventsRequest {
            since: None,
            until: None,
            filters: CoreEventRangeFilters::default(),
            cursor: None,
            limit: 1,
            page_items: 1,
            byte_budget: ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
            strict_budget: None,
        })
        .unwrap();
    let listed_value =
        render_event_read_model(&listed.page.items[0], EventContentProjection::Text).unwrap();
    assert_eq!(listed_value["content_projection"], "text");
    assert_eq!(listed_value["text"], "needle first");
    assert!(listed_value
        .get("structured_content")
        .is_some_and(|value| value.is_null()));
    assert!(listed_value.get("activity").is_none());
    let full_value =
        render_event_read_model(&listed.page.items[0], EventContentProjection::Full).unwrap();
    assert_eq!(
        full_value["activity"],
        serde_json::to_value(records[0].content.activity.as_ref().unwrap()).unwrap()
    );
    assert_eq!(
        full_value["activity"]["facts"][0],
        full_value["activity"]["facts"][2]
    );
    let record = event_query_event_read_model(index.generation_id(), 0, listed_value);
    assert_eq!(record["record_type"], "event_range_event");
    assert_eq!(record["ordinal"], 0);
    assert_eq!(record["event"]["text"], "needle first");
}

#[test]
fn structured_cursor_and_compact_reference_compatibility_are_neutral() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let query = PinnedHistoryQuery::new(&index, None);
    let page = query
        .show_session_page(&ShowSessionPageRequest {
            selector: Some(records[0].session_id.to_string()),
            provider_session_id: None,
            provider: None,
            provider_key: None,
            source_id: None,
            mode: SessionEventMode::Full,
            cursor: None,
            limit: 1,
            page_items: 1,
            page_budget: CoreEventPageBudget::new(
                ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
                ctx_history_core::MAX_CORE_CONTENT_BYTES,
            ),
        })
        .unwrap();
    let cursor = page.next_cursor.clone().unwrap();
    let rendered = retain_structured_session_page(
        page.events,
        page.has_more,
        ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
    )
    .unwrap();
    let transcript = paginated_session_transcript_read_model(
        &page.session,
        StructuredTranscriptMode::Full,
        StructuredOutputFormat::Json,
        rendered.events,
        1,
        rendered.has_more,
        rendered.next_cursor.as_ref(),
    )
    .unwrap();
    assert_eq!(transcript["pagination"]["limit"], 1);
    assert_eq!(transcript["pagination"]["returned"], 1);
    assert_eq!(transcript["pagination"]["has_more"], true);
    let encoded = encode_session_event_cursor(&cursor).unwrap();
    assert_eq!(decode_session_event_cursor(&encoded).unwrap(), cursor);

    assert_eq!(normalize_uuid_prefix(" ABCDEF12 ").unwrap(), "abcdef12");
    assert_eq!(
        normalize_uuid_prefix("abcdef").unwrap_err(),
        UuidPrefixError::TooShort
    );
    assert_eq!(
        normalize_uuid_prefix("abcdef1-").unwrap_err(),
        UuidPrefixError::InvalidHex
    );
}

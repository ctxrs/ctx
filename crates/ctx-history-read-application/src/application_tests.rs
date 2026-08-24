use std::{
    cell::{Cell, RefCell},
    convert::Infallible,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, CertifiedSource, CoreActivity, CoreRecord, EventIdentityInput,
    LiteralFactKind, NativeItemKey, NativeSessionKey, ProviderDeclaredFact, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, TypedKey,
    CORE_ACTIVITY_REVISION,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_index_query::{
    CoreEventPageBudget, CoreEventRangeFilters, CoreEventRangeSelection, SearchContentScope,
    VerifiedIndex,
};
use serde_json::json;
use tempfile::tempdir;

use crate::{
    copied_lineage_read_model, decode_session_event_cursor, encode_session_event_cursor,
    event_query_event_read_model, event_query_receipt, event_query_wire_request,
    event_window_with_lineage_read_model, execute_list_events_stream, execute_locate,
    execute_search, execute_show_event, execute_show_session_page, execute_show_session_stream,
    normalize_search_request, normalize_uuid_prefix, paginated_session_transcript_read_model,
    plan_search, render_event_read_model, render_search_json, retain_structured_session_page,
    search_filters, ActiveSessionExclusion, CompactPresentationProjection, EventContentProjection,
    EventWindowBudget, GenerationRead, GenerationReadPort, GenerationReadRequest,
    GenerationReadTarget, HistorySemanticBatch, HistorySemanticError, HistorySemanticPort,
    HistorySemanticQuery, ListEventsPageRequest, ListEventsRequest, ListEventsStreamCallback,
    ListEventsStreamCompletion, ListEventsStreamControl, ListEventsStreamPage,
    LocateApplicationRequest, LocateRequest, LocateResult, PinnedHistoryQuery, RetainedPeerRead,
    SearchApplicationError, SearchApplicationReadModelInput, SearchApplicationRequest,
    SearchBackend, SearchJsonInput, SearchPolicy, SearchRenderMetrics, SearchRequest,
    SearchResultCommands, SemanticAvailability, SemanticReason, SessionEventMode,
    ShowEventApplicationRequest, ShowEventRequest, ShowSessionApplicationRequest,
    ShowSessionPageRequest, ShowSessionStreamCallback, ShowSessionStreamControl,
    ShowSessionStreamPage, ShowSessionStreamRequest, ShowSessionStreamStart,
    StructuredOutputFormat, StructuredTranscriptMode, UuidPrefixError,
};

struct UnusedSemanticPort;

struct UnusedSemanticQuery;

impl HistorySemanticPort for UnusedSemanticPort {
    type Query<'a> = UnusedSemanticQuery;

    fn begin_query<'a>(
        &'a self,
        _index: &'a VerifiedIndex,
    ) -> Result<Self::Query<'a>, HistorySemanticError> {
        panic!("lexical application query must not open the semantic port")
    }
}

impl HistorySemanticQuery for UnusedSemanticQuery {
    fn candidates(
        &mut self,
        _query: &str,
        _filters: &ctx_history_index_query::EventSearchFilters,
        _candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError> {
        panic!("lexical application query must not request semantic candidates")
    }
}

fn source() -> SourceKey {
    SourceKey::derive(
        "custom",
        "application_query_test",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("application-query.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn record(source: &SourceKey, sequence: u64, role: &str, body: &str) -> CoreRecord {
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("pinned-session").unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        "application-query-test-v1",
        body,
    )
    .unwrap();
    record.provider_session_id = Some("pinned-session".to_owned());
    record.occurred_at_unix_ms = Some(1_000 + sequence as i64);
    record.role = Some(role.to_owned());
    record.agent_scope = Some(ctx_history_core::AgentScope::Primary);
    record
}

fn certificate(source: &SourceKey, documents: usize) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "application-query-test-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: documents as u64,
            retained_records: documents as u64,
            indexed_documents: documents as u64,
            certified_bytes: documents as u64 * 10,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn publish(root: &Path) -> (VerifiedIndex, Vec<CoreRecord>) {
    let source = source();
    let mut records = vec![
        record(&source, 1, "user", "needle first"),
        record(&source, 2, "assistant", "needle reply"),
        record(&source, 3, "user", "needle followup"),
    ];
    records[0].content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8("call-01").unwrap()),
        invocation: Some(ActivityInvocation {
            protocol: Some("native".to_owned()),
            server: None,
            tool: "lookup".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: json!({"exact": ["雪", null]}),
            },
            started_at_unix_ms: Some(900),
        }),
        result: Some(ActivityResult {
            status: Some("provider::ok".to_owned()),
            completed_at_unix_ms: Some(901),
            duration_ns: Some(10),
            text: ActivityTextCapture::NormalizedBody,
            structured_content: ActivityJsonCapture::Absent,
        }),
        facts: [
            (LiteralFactKind::File, "src/lib.rs"),
            (LiteralFactKind::Branch, "main"),
            (LiteralFactKind::File, "src/lib.rs"),
        ]
        .into_iter()
        .map(|(kind, value)| ProviderDeclaredFact {
            kind,
            value: value.to_owned(),
        })
        .collect(),
    });
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in &records {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer
        .certify_source(certificate(&source, records.len()))
        .unwrap();
    writer.commit(|_| true).unwrap();
    (VerifiedIndex::open_pinned(root).unwrap(), records)
}

fn lexical_request() -> SearchRequest {
    SearchRequest {
        query: "needle".to_owned(),
        terms: Vec::new(),
        limit: 10,
        provider: None,
        history_source: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        source_roots: Vec::new(),
        source_groups: Vec::new(),
        workspace: None,
        since: None,
        primary_only: false,
        content_scope: SearchContentScope::All,
        event_type: None,
        file: None,
        session: None,
        exclude_sessions: Vec::new(),
        events: true,
        include_current_session: false,
        backend: Some(SearchBackend::Lexical),
        semantic_weight: 0.35,
    }
}

struct RecordingGenerationPort {
    index: Option<VerifiedIndex>,
    calls: Cell<usize>,
    retained_peer: Cell<Option<RetainedPeerRead>>,
    target: RefCell<Option<GenerationReadTarget>>,
}

impl RecordingGenerationPort {
    fn new(index: VerifiedIndex) -> Self {
        Self {
            index: Some(index),
            calls: Cell::new(0),
            retained_peer: Cell::new(None),
            target: RefCell::new(None),
        }
    }
}

impl GenerationReadPort for RecordingGenerationPort {
    type Error = Infallible;

    fn read_generation(
        &mut self,
        request: &GenerationReadRequest,
    ) -> Result<GenerationRead, Self::Error> {
        self.calls.set(self.calls.get() + 1);
        self.retained_peer.set(Some(request.retained_peer));
        *self.target.borrow_mut() = Some(request.target.clone());
        Ok(GenerationRead::new(
            self.index.take().expect("generation read only once"),
            None,
        ))
    }
}

struct CountingSemanticPort(AtomicUsize);

struct CountingSemanticQuery;

impl HistorySemanticPort for CountingSemanticPort {
    type Query<'a> = CountingSemanticQuery;

    fn begin_query<'a>(
        &'a self,
        _index: &'a VerifiedIndex,
    ) -> Result<Self::Query<'a>, HistorySemanticError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(CountingSemanticQuery)
    }
}

impl HistorySemanticQuery for CountingSemanticQuery {
    fn candidates(
        &mut self,
        _query: &str,
        _filters: &ctx_history_index_query::EventSearchFilters,
        _candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError> {
        Ok(HistorySemanticBatch {
            candidates: Vec::new(),
            diagnostics: json!({"adapter": "test"}),
        })
    }
}

#[test]
fn search_application_pins_once_opens_semantics_once_and_requests_peer_lazily() {
    let temp = tempdir().unwrap();
    let (index, _) = publish(temp.path());
    let mut request = lexical_request();
    request.backend = Some(SearchBackend::Hybrid);
    let plan = plan_search(
        request,
        SearchPolicy {
            default_backend: SearchBackend::Hybrid,
            semantic: SemanticAvailability::Available,
        },
    )
    .unwrap();
    let mut generation = RecordingGenerationPort::new(index);
    let semantic = CountingSemanticPort(AtomicUsize::new(0));

    let result = execute_search(
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
    assert_eq!(generation.retained_peer.get(), Some(RetainedPeerRead::Omit));
    assert_eq!(semantic.0.load(Ordering::Relaxed), 1);
    assert_eq!(result.query().collection.result_window.hits.len(), 3);
    assert_eq!(
        result.receipt().generation_id,
        result.index().generation_id()
    );
    let commands = result
        .query()
        .collection
        .result_window
        .hits
        .iter()
        .map(|_| SearchResultCommands {
            suggested_next_commands: Vec::new(),
        })
        .collect::<Vec<_>>();
    let read_model = result
        .render_read_model(SearchApplicationReadModelInput {
            commands: &commands,
            freshness_mode: "test",
            generated_at: "2026-08-11T12:00:00.000Z",
            semantic_fallback_code: None,
            semantic_fallback_detail: None,
            metrics: SearchRenderMetrics {
                refresh_status: "existing_generation",
                refresh_source_count: 1,
                query_duration: result.query_duration(),
            },
        })
        .unwrap();
    assert_eq!(read_model["results"].as_array().unwrap().len(), 3);
    assert_eq!(
        read_model["retrieval"]["generation_id"],
        result.receipt().generation_id
    );

    let compact_index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut compact_generation = RecordingGenerationPort::new(compact_index);
    let compact = execute_search(
        SearchApplicationRequest {
            plan: plan_search(
                lexical_request(),
                SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
            )
            .unwrap(),
            generation_target: GenerationReadTarget::Active,
            compact_projection: true,
            active_session: None,
        },
        &mut compact_generation,
        &UnusedSemanticPort,
    )
    .unwrap();
    assert_eq!(compact_generation.calls.get(), 1);
    assert_eq!(
        compact_generation.retained_peer.get(),
        Some(RetainedPeerRead::IfAvailable)
    );
    assert_eq!(compact.query().collection.result_window.hits.len(), 3);
}

#[test]
fn manual_session_exclusions_resolve_and_dedupe_full_and_compact_ids() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let session_id = records[0].session_id.as_uuid();
    let compact = session_id.simple().to_string()[..8].to_owned();
    let mut request = lexical_request();
    request.exclude_sessions = vec![format!("  {session_id}  "), compact, session_id.to_string()];
    normalize_search_request(&mut request).unwrap();

    let filters = search_filters(&request, &index, None).unwrap();
    assert_eq!(filters.excluded_session_ids, vec![session_id]);
    assert!(filters.exclude_session_tree.is_none());
}

#[test]
fn manual_session_exclusions_request_retained_peer_and_render_original_selectors() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let session_id = records[0].session_id.as_uuid();
    let compact = session_id.simple().to_string()[..8].to_owned();
    let mut request = lexical_request();
    request.exclude_sessions = vec![compact.clone()];
    let plan = plan_search(
        request,
        SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
    )
    .unwrap();
    let mut generation = RecordingGenerationPort::new(index);
    let result = execute_search(
        SearchApplicationRequest {
            plan,
            generation_target: GenerationReadTarget::Active,
            compact_projection: false,
            active_session: None,
        },
        &mut generation,
        &UnusedSemanticPort,
    )
    .unwrap();
    assert_eq!(
        generation.retained_peer.get(),
        Some(RetainedPeerRead::IfAvailable)
    );
    assert!(result.query().collection.result_window.hits.is_empty());

    let read_model = result
        .render_read_model(SearchApplicationReadModelInput {
            commands: &[],
            freshness_mode: "test",
            generated_at: "2026-08-17T00:00:00.000Z",
            semantic_fallback_code: None,
            semantic_fallback_detail: None,
            metrics: SearchRenderMetrics {
                refresh_status: "existing_generation",
                refresh_source_count: 1,
                query_duration: result.query_duration(),
            },
        })
        .unwrap();
    assert_eq!(read_model["filters"]["exclude_session"], json!([compact]));
}

#[test]
fn search_rejects_root_and_group_selectors_absent_from_the_pinned_generation() {
    let temp = tempdir().unwrap();
    let (_index, _) = publish(temp.path());
    for (roots, source_groups, expected, secret) in [
        (
            vec!["personal".to_owned()],
            Vec::new(),
            "unknown provider root",
            "personal",
        ),
        (
            Vec::new(),
            vec!["work".to_owned()],
            "unknown provider root group",
            "work",
        ),
    ] {
        let mut request = lexical_request();
        request.source_roots = roots;
        request.source_groups = source_groups;
        let plan = plan_search(
            request,
            SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
        )
        .unwrap();
        let mut generation =
            RecordingGenerationPort::new(VerifiedIndex::open_pinned(temp.path()).unwrap());
        let error = match execute_search(
            SearchApplicationRequest {
                plan,
                generation_target: GenerationReadTarget::Active,
                compact_projection: false,
                active_session: None,
            },
            &mut generation,
            &UnusedSemanticPort,
        ) {
            Err(SearchApplicationError::Query(error)) => error,
            Err(other) => panic!("expected query error, got {other:?}"),
            Ok(_) => panic!("expected unknown provider-root selector to fail"),
        };
        let error = error.to_string();
        assert!(error.contains(expected));
        assert!(!error.contains(secret));
    }
}

#[test]
fn locate_application_pins_once_and_assembles_the_neutral_read_model() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let mut generation = RecordingGenerationPort::new(index);
    let located = execute_locate(
        LocateApplicationRequest {
            request: LocateRequest::Event {
                selector: records[1].event_id.to_string(),
            },
            generation_target: GenerationReadTarget::Active,
            compact_projection: false,
        },
        &mut generation,
    )
    .unwrap();

    assert_eq!(generation.calls.get(), 1);
    assert_eq!(generation.retained_peer.get(), Some(RetainedPeerRead::Omit));
    assert_eq!(located.read_model["payload_type"], "event_location");
    assert_eq!(
        located.read_model["ctx_event_id"],
        records[1].event_id.as_uuid().to_string()
    );
}

#[test]
fn exact_generation_authority_is_checked_before_semantic_or_record_reads() {
    let temp = tempdir().unwrap();
    let (index, _) = publish(temp.path());
    let mut request = lexical_request();
    request.backend = Some(SearchBackend::Semantic);
    let plan = plan_search(
        request,
        SearchPolicy {
            default_backend: SearchBackend::Semantic,
            semantic: SemanticAvailability::Available,
        },
    )
    .unwrap();
    let mut generation = RecordingGenerationPort::new(index);
    let semantic = CountingSemanticPort(AtomicUsize::new(0));

    let error = execute_search(
        SearchApplicationRequest {
            plan,
            generation_target: GenerationReadTarget::Exact("cursor-generation".to_owned()),
            compact_projection: false,
            active_session: None,
        },
        &mut generation,
        &semantic,
    )
    .err()
    .expect("mismatched exact generation must be rejected");

    assert!(matches!(error, SearchApplicationError::Generation(_)));
    assert_eq!(generation.calls.get(), 1);
    assert_eq!(semantic.0.load(Ordering::Relaxed), 0);
}

#[derive(Default)]
struct RecordingShowStream {
    starts: usize,
    page_sizes: Vec<usize>,
    stop_after_first: bool,
}

impl ShowSessionStreamCallback for RecordingShowStream {
    type Error = Infallible;

    fn start(&mut self, start: ShowSessionStreamStart<'_>) -> Result<(), Self::Error> {
        assert_eq!(
            start.session.provider_session_id.as_deref(),
            Some("pinned-session")
        );
        self.starts += 1;
        Ok(())
    }

    fn page(
        &mut self,
        page: ShowSessionStreamPage<'_>,
    ) -> Result<ShowSessionStreamControl, Self::Error> {
        self.page_sizes.push(page.events.len());
        Ok(if self.stop_after_first && self.page_sizes.len() == 1 {
            ShowSessionStreamControl::Stop
        } else {
            ShowSessionStreamControl::Continue
        })
    }
}

#[test]
fn show_operations_pin_once_and_cursor_target_precedes_session_reads() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let mut event_generation = RecordingGenerationPort::new(index);
    let shown = execute_show_event(
        ShowEventApplicationRequest {
            request: ShowEventRequest {
                selector: records[1].event_id.to_string(),
                before: 1,
                after: 1,
                window: None,
                budget: EventWindowBudget::default(),
            },
            generation_target: GenerationReadTarget::Active,
            compact_projection: false,
        },
        &mut event_generation,
    )
    .unwrap();
    assert_eq!(shown.result().events.len(), 3);
    assert_eq!(event_generation.calls.get(), 1);
    assert_eq!(
        event_generation.retained_peer.get(),
        Some(RetainedPeerRead::Omit)
    );

    let cursor_index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let cursor_page = PinnedHistoryQuery::new(&cursor_index, None)
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
    let cursor = cursor_page.next_cursor.unwrap();
    let encoded_cursor = encode_session_event_cursor(&cursor).unwrap();
    let mut page_generation =
        RecordingGenerationPort::new(VerifiedIndex::open_pinned(temp.path()).unwrap());
    let page = execute_show_session_page(
        ShowSessionApplicationRequest {
            selector: Some(records[0].session_id.to_string()),
            provider_session_id: None,
            provider: None,
            provider_key: None,
            source_id: None,
            mode: SessionEventMode::Full,
            cursor: Some(encoded_cursor),
            limit: 1,
            page_items: 1,
            page_budget: CoreEventPageBudget::new(
                ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
                ctx_history_core::MAX_CORE_CONTENT_BYTES,
            ),
            compact_projection: false,
        },
        &mut page_generation,
    )
    .unwrap();
    assert_eq!(page.page().events[0].event.event_id, records[1].event_id);
    assert_eq!(page_generation.calls.get(), 1);
    assert_eq!(
        page_generation.target.borrow().as_ref(),
        Some(&GenerationReadTarget::Exact(
            cursor.generation_id().to_owned()
        ))
    );
}

#[test]
fn show_stream_is_page_bounded_and_honors_callback_control() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let mut generation = RecordingGenerationPort::new(index);
    let mut stream = RecordingShowStream {
        stop_after_first: true,
        ..RecordingShowStream::default()
    };
    let result = execute_show_session_stream(
        ShowSessionStreamRequest {
            selector: Some(records[0].session_id.to_string()),
            provider_session_id: None,
            provider: None,
            provider_key: None,
            source_id: None,
            mode: SessionEventMode::Full,
            cursor: None,
            max_events: None,
            page_items: 1,
            page_budget: CoreEventPageBudget::new(
                ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
                ctx_history_core::MAX_CORE_CONTENT_BYTES,
            ),
            compact_projection: false,
        },
        &mut generation,
        &mut stream,
    )
    .unwrap();
    assert_eq!(generation.calls.get(), 1);
    assert_eq!(stream.starts, 1);
    assert_eq!(stream.page_sizes, [1]);
    assert_eq!(result.events_returned, 1);
    assert!(result.truncated);
}

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
    assert_eq!(search.copied_lineages.len(), 3);

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
    let copied_lineages = search
        .copied_lineages
        .iter()
        .map(copied_lineage_read_model)
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    let search_value = render_search_json(SearchJsonInput {
        request: &search.request,
        index: &index,
        collection: &search.collection,
        filters: &search.filters,
        presentations: &search.presentations,
        copied_lineages: &copied_lineages,
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
    assert_eq!(search_value["schema_version"], 1);
    assert_eq!(search_value["payload_type"], "search_results");
    assert_eq!(search_value["generated_at"], "2026-08-11T12:00:00.000Z");
    assert_eq!(search_value["freshness"]["mode"], "checkpoint");
    assert_eq!(search_value["phase_attribution"]["query_seconds"], 0.125);
    assert_eq!(search_value["results"].as_array().unwrap().len(), 3);
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

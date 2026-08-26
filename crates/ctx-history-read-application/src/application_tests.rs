use std::{
    cell::{Cell, RefCell},
    convert::Infallible,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, CertifiedSource, CoreActivity, CoreRecord, EventIdentityInput,
    LiteralFactKind, NativeItemKey, NativeSessionKey, ProviderDeclaredFact,
    ProviderNativeCopyProof, ProviderNativeEventCopy, ProviderNativeSessionRelationship,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
    TypedKey, CORE_ACTIVITY_REVISION,
};
use ctx_history_index::{
    AppliedProviderRoot, GenerationWriter, ProviderRootDefinition, SourceRouteIdentity,
    SourceRouteSnapshot, WriterOptions,
};
use ctx_history_index_format::{provider_source_config_digest, source_token};
use ctx_history_index_query::{
    CompiledSearchFilter, CoreEventPageBudget, CoreEventRangeFilters, CoreEventRangeSelection,
    EventSearchCandidate, EventSearchFilters, RankedEventRef, SearchContentScope, VerifiedIndex,
};
use serde_json::{json, Value};
use tempfile::tempdir;
use uuid::Uuid;

use crate::{
    decode_session_event_cursor, encode_session_event_cursor, event_query_event_read_model,
    event_query_receipt, event_query_wire_request, event_window_with_lineage_read_model,
    execute_list_events_stream, execute_locate, execute_search_observed, execute_show_event,
    execute_show_session_page, execute_show_session_stream, history_health_report,
    normalize_search_request, normalize_uuid_prefix, paginated_session_transcript_read_model,
    plan_search, render_event_read_model, render_search_json, retain_structured_session_page,
    ActiveSessionExclusion, CompactPresentationProjection, CompactRefResolver,
    EventContentProjection, EventWindowBudget, GenerationRead, GenerationReadPort,
    GenerationReadRequest, GenerationReadTarget, HistorySemanticBatch, HistorySemanticError,
    HistorySemanticPort, HistorySemanticQuery, ListEventsPageRequest, ListEventsRequest,
    ListEventsStreamCallback, ListEventsStreamCompletion, ListEventsStreamControl,
    ListEventsStreamPage, LocateApplicationRequest, LocateRequest, LocateResult,
    NormalizedSearchQuery, PinnedHistoryQuery, RetainedPeerRead, SearchApplicationError,
    SearchApplicationReadModelInput, SearchApplicationRequest, SearchApplicationResult,
    SearchBackend, SearchCollection, SearchDiversificationStatus, SearchExecutionResult,
    SearchFailurePhase, SearchJsonInput, SearchPolicy, SearchRenderMetrics, SearchRequest,
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
    fn prepare_alternative(&mut self, _query: &str) -> Result<Value, HistorySemanticError> {
        panic!("lexical application query must not prepare semantic alternatives")
    }

    fn candidates(
        &mut self,
        _filter: &ctx_history_index_query::CompiledSearchFilter,
        _candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError> {
        panic!("lexical application query must not request semantic candidates")
    }
}

fn execute_search<Generation, Semantic>(
    request: SearchApplicationRequest,
    generation_port: &mut Generation,
    semantic_port: &Semantic,
) -> std::result::Result<SearchApplicationResult, SearchApplicationError<Generation::Error>>
where
    Generation: GenerationReadPort,
    Semantic: HistorySemanticPort,
{
    execute_search_observed(request, generation_port, semantic_port)
        .map_err(crate::ObservedSearchApplicationError::into_error)
}

fn search_filters(
    request: &SearchRequest,
    index: &VerifiedIndex,
    active_session: Option<&ActiveSessionExclusion>,
) -> anyhow::Result<EventSearchFilters> {
    let references = CompactRefResolver::new(index, None);
    crate::search::search_filters_with_refs(request, index, &references, active_session)
}

fn collect_search_hits<P: HistorySemanticPort>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    expected_filters: &EventSearchFilters,
    semantic: SemanticAvailability,
    semantic_port: &P,
) -> SearchExecutionResult<SearchCollection> {
    let policy = SearchPolicy {
        default_backend: request.backend.unwrap_or(SearchBackend::Lexical),
        semantic,
    };
    let plan = plan_search(request.clone(), policy)?;
    let query = PinnedHistoryQuery::new(index, None)
        .search(plan, None, semantic_port)
        .map_err(|failure| *failure.error)?;
    assert_eq!(&query.filters, expected_filters);
    Ok(query.collection)
}

struct ClosureSemanticPort<SemanticSearch>(Mutex<SemanticSearch>);

struct ClosureSemanticQuery<'port, SemanticSearch> {
    search: &'port Mutex<SemanticSearch>,
    queries: Vec<String>,
}

impl<SemanticSearch> HistorySemanticPort for ClosureSemanticPort<SemanticSearch>
where
    SemanticSearch: FnMut(
            &[&str],
            &CompiledSearchFilter,
            usize,
        ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError>
        + Send,
{
    type Query<'a>
        = ClosureSemanticQuery<'a, SemanticSearch>
    where
        Self: 'a;

    fn begin_query<'a>(
        &'a self,
        _index: &'a VerifiedIndex,
    ) -> std::result::Result<Self::Query<'a>, HistorySemanticError> {
        Ok(ClosureSemanticQuery {
            search: &self.0,
            queries: Vec::new(),
        })
    }
}

impl<SemanticSearch> HistorySemanticQuery for ClosureSemanticQuery<'_, SemanticSearch>
where
    SemanticSearch: FnMut(
            &[&str],
            &CompiledSearchFilter,
            usize,
        ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError>
        + Send,
{
    fn prepare_alternative(
        &mut self,
        query: &str,
    ) -> std::result::Result<Value, HistorySemanticError> {
        self.queries.push(query.to_owned());
        Ok(Value::Null)
    }

    fn candidates(
        &mut self,
        filter: &CompiledSearchFilter,
        candidate_limit: usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError> {
        let queries = self.queries.iter().map(String::as_str).collect::<Vec<_>>();
        (self.search.lock().unwrap())(&queries, filter, candidate_limit)
    }
}

fn collect_search_hits_using<SemanticSearch>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    expected_filters: &EventSearchFilters,
    semantic: SemanticAvailability,
    semantic_search: SemanticSearch,
) -> SearchExecutionResult<SearchCollection>
where
    SemanticSearch: FnMut(
            &[&str],
            &CompiledSearchFilter,
            usize,
        ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError>
        + Send,
{
    collect_search_hits(
        request,
        index,
        expected_filters,
        semantic,
        &ClosureSemanticPort(Mutex::new(semantic_search)),
    )
}

fn source_named(name: &str) -> SourceKey {
    SourceKey::derive(
        "custom",
        "application_query_test",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn provider_root_source(name: &str) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn source() -> SourceKey {
    source_named("application-query.jsonl")
}

fn record(source: &SourceKey, sequence: u64, role: &str, body: &str) -> CoreRecord {
    record_for_session(source, "pinned-session", sequence, role, body)
}

fn record_for_session(
    source: &SourceKey,
    native_session_id: &str,
    sequence: u64,
    role: &str,
    body: &str,
) -> CoreRecord {
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8(native_session_id).unwrap()).unwrap();
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
    record.provider_session_id = Some(native_session_id.to_owned());
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

fn publish_grouped_search(root: &Path) -> (VerifiedIndex, Vec<CoreRecord>) {
    let source = source_named("coalesced-grouping.jsonl");
    let mut family_root =
        record_for_session(&source, "family-root", 1, "user", "groupneedle root body");
    family_root.root_session_id = Some(family_root.session_id);
    family_root.session_relationship = Some(ProviderNativeSessionRelationship::Root);
    let child_absent = record_for_session(
        &source,
        "family-child",
        1,
        "assistant",
        "groupneedle child body",
    );
    let mut child_positive = record_for_session(
        &source,
        "family-child",
        2,
        "assistant",
        "groupneedle child witness",
    );
    child_positive.parent_session_id = Some(family_root.session_id);
    child_positive.root_session_id = Some(family_root.session_id);
    child_positive.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    let mut sibling = record_for_session(
        &source,
        "family-sibling",
        1,
        "assistant",
        "groupneedle sibling body",
    );
    sibling.parent_session_id = Some(family_root.session_id);
    sibling.root_session_id = Some(family_root.session_id);
    sibling.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    let independent = record_for_session(
        &source,
        "independent",
        1,
        "assistant",
        "groupneedle independent body",
    );
    let records = vec![
        family_root,
        child_absent,
        child_positive,
        sibling,
        independent,
    ];
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

fn publish_provider_root_search(root: &Path) -> (VerifiedIndex, Vec<CoreRecord>) {
    let personal_source = provider_root_source("personal-history.jsonl");
    let archive_source = provider_root_source("archive-history.jsonl");
    let mut family_root = record_for_session(
        &personal_source,
        "personal-family-root",
        1,
        "user",
        "root context",
    );
    family_root.root_session_id = Some(family_root.session_id);
    family_root.session_relationship = Some(ProviderNativeSessionRelationship::Root);
    let mut family_strong = record_for_session(
        &personal_source,
        "personal-family-strong",
        4,
        "user",
        "parityneedle",
    );
    family_strong.root_session_id = Some(family_root.session_id);
    family_strong.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    let mut family_repeat = record_for_session(
        &personal_source,
        "personal-family-repeat",
        3,
        "user",
        "parityneedle",
    );
    family_repeat.root_session_id = Some(family_root.session_id);
    family_repeat.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    let independent = record_for_session(
        &personal_source,
        "personal-independent",
        2,
        "user",
        "parityneedle",
    );
    let archive = record_for_session(
        &archive_source,
        "archive-higher-ranked",
        100,
        "user",
        "parityneedle",
    );
    let records = vec![
        family_root,
        family_strong,
        family_repeat,
        independent,
        archive,
    ];

    let archive_route = SourceRouteIdentity::from_sha256("a1".repeat(32)).unwrap();
    let personal_route = SourceRouteIdentity::from_sha256("b2".repeat(32)).unwrap();
    let definitions = vec![
        ProviderRootDefinition {
            id: "archive".to_owned(),
            provider: "codex".parse().unwrap(),
            path: root.join("archive"),
            group: Some("cold".to_owned()),
            kind: None,
        },
        ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: "codex".parse().unwrap(),
            path: root.join("personal"),
            group: Some("work".to_owned()),
            kind: None,
        },
    ];
    let applied_roots = vec![
        AppliedProviderRoot::new(definitions[0].clone(), vec![archive_route.clone()]).unwrap(),
        AppliedProviderRoot::new(definitions[1].clone(), vec![personal_route.clone()]).unwrap(),
    ];
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
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
    for record in &records[..4] {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer
        .certify_source(certificate(&personal_source, 4))
        .unwrap();
    writer.begin_source(archive_source.clone()).unwrap();
    writer.add_core_record(records[4].clone()).unwrap();
    writer
        .certify_source(certificate(&archive_source, 1))
        .unwrap();
    writer
        .set_present_source_routes(vec![
            SourceRouteSnapshot::present(archive_route, vec![archive_source]).unwrap(),
            SourceRouteSnapshot::present(personal_route, vec![personal_source]).unwrap(),
        ])
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
    fn prepare_alternative(&mut self, _query: &str) -> Result<Value, HistorySemanticError> {
        Ok(json!({"adapter": "test-query"}))
    }

    fn candidates(
        &mut self,
        _filter: &ctx_history_index_query::CompiledSearchFilter,
        _candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError> {
        Ok(HistorySemanticBatch {
            candidates: Vec::new(),
            diagnostics: json!({"adapter": "test"}),
        })
    }
}

struct FixedSemanticPort(Vec<EventSearchCandidate>);

struct FixedSemanticQuery<'a>(&'a [EventSearchCandidate]);

impl HistorySemanticPort for FixedSemanticPort {
    type Query<'a> = FixedSemanticQuery<'a>;

    fn begin_query<'a>(
        &'a self,
        _index: &'a VerifiedIndex,
    ) -> Result<Self::Query<'a>, HistorySemanticError> {
        Ok(FixedSemanticQuery(&self.0))
    }
}

impl HistorySemanticQuery for FixedSemanticQuery<'_> {
    fn prepare_alternative(&mut self, _query: &str) -> Result<Value, HistorySemanticError> {
        Ok(json!({"adapter": "fixed-test-query"}))
    }

    fn candidates(
        &mut self,
        _filter: &ctx_history_index_query::CompiledSearchFilter,
        _candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError> {
        Ok(HistorySemanticBatch {
            candidates: self.0.to_vec(),
            diagnostics: json!({"adapter": "fixed-test"}),
        })
    }
}

#[test]
fn canonical_winner_hydration_is_single_bounded_and_fail_closed() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let request = lexical_request();
    let filter = CompiledSearchFilter::compile(Default::default()).unwrap();
    let query = NormalizedSearchQuery::from_request(&request);
    let ranked = || {
        crate::search::collect_search_hits_observed(
            &request,
            &index,
            &filter,
            SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled),
            &UnusedSemanticPort,
        )
        .unwrap()
    };

    ctx_history_index_query::reset_stored_event_record_materializations();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    let ranked_collection = ranked();
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0,
        "ranking and shaping must not hydrate Core winners early"
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);
    let (collection, presentations) =
        crate::presentation::hydrate_ranked_search_collection_with_budget(
            &index,
            ranked_collection,
            &query,
            &filter,
            crate::SearchPresentationHydrationBudget {
                maximum_retained_snippet_bytes:
                    crate::SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
            },
        )
        .unwrap();
    assert_eq!(collection.result_window.hits.len(), records.len());
    assert_eq!(presentations.len(), records.len());
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        records.len(),
        "only final winners are hydrated"
    );
    assert_eq!(
        ctx_history_index_query::core_record_decodes(),
        records.len()
    );
    let retained_snippet_bytes = presentations
        .iter()
        .map(|presentation| presentation.snippet.len())
        .sum::<usize>();

    let retention_error = crate::presentation::hydrate_ranked_search_collection_with_budget(
        &index,
        ranked(),
        &query,
        &filter,
        crate::SearchPresentationHydrationBudget {
            maximum_retained_snippet_bytes: retained_snippet_bytes - 1,
        },
    )
    .unwrap_err();
    let typed = retention_error
        .downcast_ref::<crate::SearchPresentationRetentionBudgetExceeded>()
        .expect("snippet retention failure must stay typed");
    assert_eq!(typed.retained_snippet_bytes, retained_snippet_bytes);

    let mut excessive = ranked();
    let first = excessive.result_window.hits[0].clone();
    excessive.result_window.hits = vec![first.clone(); crate::MAX_SEARCH_RESULTS + 1];
    let excessive_error =
        crate::presentation::hydrate_ranked_search_collection(&index, excessive, &query, &filter)
            .unwrap_err();
    assert!(excessive_error
        .to_string()
        .contains("search presentation cannot hydrate more than 200 hits"));

    let mut duplicate = ranked();
    duplicate.result_window.hits = vec![first.clone(), first.clone()];
    let duplicate_error =
        crate::presentation::hydrate_ranked_search_collection(&index, duplicate, &query, &filter)
            .unwrap_err();
    assert!(duplicate_error
        .to_string()
        .contains("search result duplicated Core event"));

    let mut missing = ranked();
    missing.result_window.hits.truncate(1);
    missing.result_window.hits[0].event.event_id = uuid::Uuid::nil();
    let missing_error =
        crate::presentation::hydrate_ranked_search_collection(&index, missing, &query, &filter)
            .unwrap_err();
    assert_eq!(
        missing_error.to_string(),
        "pinned Core lookup omitted search event 00000000-0000-0000-0000-000000000000"
    );

    let mutations: [fn(&mut RankedEventRef); 5] = [
        |event| event.event_identity_digest[31] ^= 1,
        |event| event.session_id = Uuid::nil(),
        |event| event.source_owner_digest[31] ^= 1,
        |event| event.event_sequence = event.event_sequence.saturating_add(1),
        |event| event.has_event_copy = !event.has_event_copy,
    ];
    for mutate in mutations {
        let mut misaligned = ranked();
        misaligned.result_window.hits.truncate(1);
        mutate(&mut misaligned.result_window.hits[0].event);
        let misaligned_error = crate::presentation::hydrate_ranked_search_collection(
            &index, misaligned, &query, &filter,
        )
        .unwrap_err();
        assert!(misaligned_error
            .to_string()
            .contains("misaligned ranked metadata"));
    }
    let mut misaligned_time = ranked();
    misaligned_time.result_window.hits.truncate(1);
    misaligned_time.result_window.hits[0]
        .event
        .occurred_at_unix_ms = Some(i64::MIN);
    let misaligned_time_error = crate::presentation::hydrate_ranked_search_collection(
        &index,
        misaligned_time,
        &query,
        &filter,
    )
    .unwrap_err();
    assert!(misaligned_time_error
        .to_string()
        .contains("misaligned ranked metadata"));

    let rejecting_filter = CompiledSearchFilter::compile(EventSearchFilters {
        provider: Some("provider-with-no-events".to_owned()),
        ..EventSearchFilters::default()
    })
    .unwrap();
    let filter_error = crate::presentation::hydrate_ranked_search_collection(
        &index,
        ranked(),
        &query,
        &rejecting_filter,
    )
    .unwrap_err();
    assert!(filter_error
        .to_string()
        .contains("no longer matches the compiled Search filter"));
}

#[test]
fn dense_limit_windows_preserve_exact_order_more_available_and_winner_only_hydration() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let filters = EventSearchFilters::default();
    let full = collect_search_hits(
        &lexical_request(),
        &index,
        &filters,
        SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled),
        &UnusedSemanticPort,
    )
    .unwrap();
    let exact_order = full
        .result_window
        .hits
        .iter()
        .map(|hit| hit.event.event_id)
        .collect::<Vec<_>>();
    assert_eq!(exact_order.len(), records.len());

    for limit in 1..=records.len() {
        let mut request = lexical_request();
        request.limit = limit;
        ctx_history_index_query::reset_stored_event_record_materializations();
        ctx_history_index_query::reset_stored_core_event_record_materializations();
        ctx_history_index_query::reset_core_record_decodes();
        let collection = collect_search_hits(
            &request,
            &index,
            &filters,
            SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled),
            &UnusedSemanticPort,
        )
        .unwrap();
        assert_eq!(
            collection
                .result_window
                .hits
                .iter()
                .map(|hit| hit.event.event_id)
                .collect::<Vec<_>>(),
            exact_order[..limit]
        );
        assert_eq!(
            collection.result_window.more_available,
            limit < exact_order.len()
        );
        assert_eq!(
            ctx_history_index_query::stored_event_record_materializations(),
            0
        );
        assert_eq!(
            ctx_history_index_query::stored_core_event_record_materializations(),
            limit
        );
        assert_eq!(ctx_history_index_query::core_record_decodes(), limit);
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
        result.query().collection.diversification.status,
        SearchDiversificationStatus::NotApplicable
    );
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
    ctx_history_index_query::reset_stored_event_record_materializations();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let compact_read_model = result.project_read_model(&read_model).unwrap();
    assert_eq!(compact_read_model["results"].as_array().unwrap().len(), 3);
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0,
        "compact Search rendering must resolve indexed IDs without reloading EventRecord"
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0,
        "compact Search rendering must not hydrate Core after final winners"
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
fn candidate_reference_failure_reaches_the_application_terminal_error_with_partial_work() {
    let temp = tempdir().unwrap();
    let (index, _) = publish(temp.path());
    let plan = plan_search(
        lexical_request(),
        SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
    )
    .unwrap();
    let mut generation = RecordingGenerationPort::new(index);
    ctx_history_index_query::fail_lexical_candidate_materialization_after(1);

    let failure = match execute_search_observed(
        SearchApplicationRequest {
            plan,
            generation_target: GenerationReadTarget::Active,
            compact_projection: false,
            active_session: None,
        },
        &mut generation,
        &UnusedSemanticPort,
    ) {
        Ok(_) => {
            panic!("injected candidate-reference failure must reach the application terminal error")
        }
        Err(failure) => failure,
    };

    assert_eq!(
        failure.failure_phase(),
        SearchFailurePhase::IndexQueryDecode
    );
    let work = failure.work();
    assert_eq!(work.retrieval_rounds, Some(1));
    assert_eq!(work.query_executions, Some(1));
    assert_eq!(work.candidate_rows, Some(3));
    assert_eq!(work.records_decoded, Some(0));
    assert_eq!(work.encoded_core_bytes_decoded, Some(0));
}

#[test]
fn explicit_session_lexical_search_is_dense_and_never_diversified() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let mut request = lexical_request();
    request.events = false;
    request.session = Some(records[0].session_id.to_string());
    let filters = search_filters(&request, &index, None).unwrap();
    let collection = collect_search_hits(
        &request,
        &index,
        &filters,
        SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled),
        &UnusedSemanticPort,
    )
    .unwrap();

    assert_eq!(collection.result_window.hits.len(), 3);
    assert!(collection
        .result_window
        .hits
        .iter()
        .all(|hit| hit.more_matches_in_session == 0));
    assert_eq!(
        collection.diversification.status,
        SearchDiversificationStatus::NotApplicable
    );
}

#[test]
fn semantic_and_hybrid_share_coalesced_family_shaping_and_remain_indeterminate() {
    let temp = tempdir().unwrap();
    let (index, records) = publish_grouped_search(temp.path());
    let child_absent = index
        .event_by_id(records[1].event_id.as_uuid())
        .unwrap()
        .unwrap();
    let sibling = index
        .event_by_id(records[3].event_id.as_uuid())
        .unwrap()
        .unwrap();
    let independent = index
        .event_by_id(records[4].event_id.as_uuid())
        .unwrap()
        .unwrap();
    let semantic_candidates = vec![
        EventSearchCandidate {
            event: RankedEventRef::from(&child_absent),
            score: 100.0,
        },
        EventSearchCandidate {
            event: RankedEventRef::from(&sibling),
            score: 90.0,
        },
        EventSearchCandidate {
            event: RankedEventRef::from(&independent),
            score: 80.0,
        },
    ];
    let filters = ctx_history_index_query::EventSearchFilters::default();

    for backend in [SearchBackend::Semantic, SearchBackend::Hybrid] {
        let mut request = lexical_request();
        request.query = "zzzz-backend-only-token".to_owned();
        request.events = false;
        request.limit = 3;
        request.backend = Some(backend);
        request.semantic_weight = 1.0;
        let collection = collect_search_hits_using(
            &request,
            &index,
            &filters,
            SemanticAvailability::Available,
            |_, _, _| {
                Ok(HistorySemanticBatch {
                    candidates: semantic_candidates.clone(),
                    diagnostics: json!({"candidate_count": 3}),
                })
            },
        )
        .unwrap();

        assert_eq!(
            collection
                .result_window
                .hits
                .iter()
                .map(|hit| hit.event.provider_session_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["family-child", "independent", "family-sibling"]
        );
        assert_eq!(
            collection.diversification.status,
            SearchDiversificationStatus::Indeterminate
        );
        assert_eq!(collection.diversification.changed_final_top_n, None);
    }
}

#[test]
fn grouped_search_decodes_only_final_winners_for_every_backend() {
    let temp = tempdir().unwrap();
    let (index, records) = publish_grouped_search(temp.path());
    let semantic_candidates = records
        .iter()
        .enumerate()
        .map(|(position, record)| {
            let event = index
                .event_by_id(record.event_id.as_uuid())
                .unwrap()
                .unwrap();
            EventSearchCandidate {
                event: RankedEventRef::from(&event),
                score: (records.len() - position) as f32,
            }
        })
        .collect::<Vec<_>>();
    let semantic = FixedSemanticPort(semantic_candidates);
    let query = PinnedHistoryQuery::new(&index, None);

    for backend in [
        SearchBackend::Lexical,
        SearchBackend::Semantic,
        SearchBackend::Hybrid,
    ] {
        let mut request = lexical_request();
        request.query = "groupneedle".to_owned();
        request.events = false;
        request.limit = 3;
        request.backend = Some(backend);
        request.semantic_weight = 0.5;
        let plan = plan_search(
            request,
            SearchPolicy {
                default_backend: backend,
                semantic: SemanticAvailability::Available,
            },
        )
        .unwrap();

        ctx_history_index_query::reset_stored_event_record_materializations();
        ctx_history_index_query::reset_stored_core_event_record_materializations();
        ctx_history_index_query::reset_core_record_decodes();
        let search = query.search(plan, None, &semantic).unwrap();
        let winners = search.collection.result_window.hits.len();

        assert_eq!(winners, 3);
        assert_eq!(
            ctx_history_index_query::stored_event_record_materializations(),
            0
        );
        assert_eq!(
            ctx_history_index_query::stored_core_event_record_materializations(),
            winners
        );
        assert_eq!(ctx_history_index_query::core_record_decodes(), winners);
    }
}

#[test]
fn provider_root_and_group_selectors_share_one_source_predicate_across_backends() {
    let temp = tempdir().unwrap();
    let (index, records) = publish_provider_root_search(temp.path());
    let semantic_candidates = [(4, 400.0), (1, 300.0), (2, 200.0), (3, 100.0)]
        .into_iter()
        .map(|(record, score)| EventSearchCandidate {
            event: RankedEventRef::from(
                &index
                    .event_by_id(records[record].event_id.as_uuid())
                    .unwrap()
                    .unwrap(),
            ),
            score,
        })
        .collect::<Vec<_>>();
    let expected_sources = vec![source_token(&records[0].source)];
    let mut expected_events = vec![records[1].event_id.as_uuid(), records[3].event_id.as_uuid()];
    expected_events.sort();
    let mut expected_sessions = vec![
        records[1].session_id.as_uuid(),
        records[3].session_id.as_uuid(),
    ];
    expected_sessions.sort();

    for (source_roots, source_groups) in [
        (vec!["personal".to_owned()], Vec::new()),
        (Vec::new(), vec!["work".to_owned()]),
    ] {
        for backend in [
            SearchBackend::Lexical,
            SearchBackend::Semantic,
            SearchBackend::Hybrid,
        ] {
            let mut request = lexical_request();
            request.query = "parityneedle".to_owned();
            request.limit = 2;
            request.events = false;
            request.backend = Some(backend);
            request.semantic_weight = 0.5;
            request.source_roots.clone_from(&source_roots);
            request.source_groups.clone_from(&source_groups);
            let filters = search_filters(&request, &index, None).unwrap();
            assert_eq!(
                filters.allowed_source_keys.as_ref(),
                Some(&expected_sources)
            );

            let collection = collect_search_hits_using(
                &request,
                &index,
                &filters,
                SemanticAvailability::Available,
                |_, semantic_filters, _| {
                    assert_eq!(
                        semantic_filters.filters().allowed_source_keys.as_ref(),
                        Some(&expected_sources)
                    );
                    let projection = index.semantic_filter_projection(semantic_filters).unwrap();
                    assert!(projection.contains(records[1].event_id.as_uuid()));
                    assert!(!projection.contains(records[4].event_id.as_uuid()));
                    Ok(HistorySemanticBatch {
                        candidates: semantic_candidates
                            .iter()
                            .filter(|candidate| projection.contains(candidate.event.event_id))
                            .cloned()
                            .collect(),
                        diagnostics: json!({"adapter": "semantic-filter-projection-test"}),
                    })
                },
            )
            .unwrap();

            let mut selected_events = collection
                .result_window
                .hits
                .iter()
                .map(|hit| hit.event.event_id)
                .collect::<Vec<_>>();
            selected_events.sort();
            assert_eq!(selected_events, expected_events);
            let mut selected_sessions = collection
                .result_window
                .hits
                .iter()
                .map(|hit| hit.event.session_id)
                .collect::<Vec<_>>();
            selected_sessions.sort();
            assert_eq!(selected_sessions, expected_sessions);
            match backend {
                SearchBackend::Lexical => {
                    assert_eq!(
                        collection.diversification.status,
                        SearchDiversificationStatus::Applied
                    );
                    assert_eq!(collection.diversification.changed_final_top_n, Some(false));
                }
                SearchBackend::Semantic | SearchBackend::Hybrid => {
                    assert_eq!(
                        collection.diversification.status,
                        SearchDiversificationStatus::Indeterminate
                    );
                    assert_eq!(collection.diversification.changed_final_top_n, None);
                }
            }
        }
    }
}

#[test]
fn health_report_uses_user_concepts_without_claiming_inventory_coverage() {
    let temp = tempdir().unwrap();
    let (index, _records) = publish_provider_root_search(temp.path());

    let health = history_health_report(&index).unwrap();

    assert_eq!(health.contributing_agent_histories, ["codex"]);
    assert_eq!(health.provider_roots, None);
    assert_eq!(health.sessions, 5);
    assert_eq!(health.messages, 5);
    assert_eq!(health.tool_calls, 0);
    assert_eq!(health.data.processed, 50);
    assert_eq!(health.data.excluded, None);
    assert!(!health.is_partial());
}

#[test]
fn rendered_semantic_result_preserves_exact_ids() {
    let temp = tempdir().unwrap();
    let source = source_named("semantic-rendered-identities.jsonl");
    let semantic_event = record_for_session(
        &source,
        "semantic-result",
        1,
        "user",
        "recalled semantic event",
    );
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(semantic_event.clone()).unwrap();
    writer.certify_source(certificate(&source, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let candidate = index
        .event_by_id(semantic_event.event_id.as_uuid())
        .unwrap()
        .unwrap();
    let semantic = FixedSemanticPort(vec![EventSearchCandidate {
        event: RankedEventRef::from(&candidate),
        score: 1.0,
    }]);
    let mut request = lexical_request();
    request.query = "semantic meaning".to_owned();
    request.limit = 1;
    request.events = false;
    request.backend = Some(SearchBackend::Semantic);
    request.semantic_weight = 1.0;
    let query = PinnedHistoryQuery::new(&index, None);
    let search = query
        .search(
            plan_search(
                request,
                SearchPolicy {
                    default_backend: SearchBackend::Semantic,
                    semantic: SemanticAvailability::Available,
                },
            )
            .unwrap(),
            None,
            &semantic,
        )
        .unwrap();

    assert_eq!(search.collection.result_window.hits.len(), 1);
    assert_eq!(
        search.collection.result_window.hits[0].event.event_id,
        semantic_event.event_id.as_uuid()
    );
    assert_eq!(
        search.collection.result_window.hits[0].event.session_id,
        semantic_event.session_id.as_uuid()
    );
    assert_eq!(
        search.collection.diversification.status,
        SearchDiversificationStatus::Indeterminate
    );
    assert_eq!(search.collection.diversification.changed_final_top_n, None);

    let value = render_search_json(SearchJsonInput {
        request: &search.request,
        index: &index,
        collection: &search.collection,
        filters: &search.filters,
        presentations: &search.presentations,
        commands: &[SearchResultCommands {
            suggested_next_commands: Vec::new(),
        }],
        freshness_mode: "test",
        generated_at: "2026-08-25T00:00:00.000Z",
        semantic_fallback_code: None,
        semantic_fallback_detail: None,
        metrics: SearchRenderMetrics {
            refresh_status: "unchanged",
            refresh_source_count: 1,
            query_duration: Duration::ZERO,
        },
    })
    .unwrap();
    assert_eq!(value["diversification"]["status"], "indeterminate");
    let result = &value["results"][0];
    assert_eq!(
        result["item_id"],
        semantic_event.session_id.as_uuid().to_string()
    );
    assert_eq!(
        result["ctx_event_id"],
        semantic_event.event_id.as_uuid().to_string()
    );
    assert_eq!(
        result["ctx_session_id"],
        semantic_event.session_id.as_uuid().to_string()
    );
    assert_eq!(
        result["event_id"],
        semantic_event.event_id.as_uuid().to_string()
    );
    assert_eq!(
        result["session_id"],
        semantic_event.session_id.as_uuid().to_string()
    );
    let citation = &result["citations"][0];
    assert_eq!(
        citation["item_id"],
        semantic_event.event_id.as_uuid().to_string()
    );
    assert_eq!(
        citation["ctx_event_id"],
        semantic_event.event_id.as_uuid().to_string()
    );
    assert_eq!(
        citation["ctx_session_id"],
        semantic_event.session_id.as_uuid().to_string()
    );
}

#[test]
fn explicit_show_keeps_one_hop_direct_lineage() {
    let temp = tempdir().unwrap();
    let source = source_named("show-event-copy.jsonl");
    let mut ancestor = record_for_session(&source, "copy-root", 1, "user", "ancestor body");
    ancestor.root_session_id = Some(ancestor.session_id);
    ancestor.session_relationship = Some(ProviderNativeSessionRelationship::Root);
    let mut copied = record_for_session(&source, "copy-child", 1, "assistant", "copiedneedle body");
    copied.parent_session_id = Some(ancestor.session_id);
    copied.root_session_id = Some(ancestor.session_id);
    copied.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
    copied.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: ancestor.session_id,
        ancestor_event_id: ancestor.event_id,
        proof: ProviderNativeCopyProof::NativeEventIdentity,
    });
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [&ancestor, &copied] {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer.certify_source(certificate(&source, 2)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let query = PinnedHistoryQuery::new(&index, None);
    let shown = query
        .show_event(&ShowEventRequest {
            selector: copied.event_id.to_string(),
            before: 0,
            after: 0,
            window: None,
            budget: EventWindowBudget::default(),
        })
        .unwrap();
    assert_eq!(shown.copied_lineage.resolution.state_str(), "resolved");
    assert_eq!(shown.copied_lineage.selected_depth, 1);
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
fn active_session_exclusion_requires_one_source_scoped_session() {
    let temp = tempdir().unwrap();
    let first_source = source_named("duplicate-provider-session-first.jsonl");
    let second_source = source_named("duplicate-provider-session-second.jsonl");
    let first = record(&first_source, 1, "user", "needle first root");
    let second = record(&second_source, 1, "user", "needle second root");
    assert_ne!(first.session_id, second.session_id);

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, record) in [
        (first_source, first.clone()),
        (second_source, second.clone()),
    ] {
        writer.begin_source(source.clone()).unwrap();
        writer.add_core_record(record).unwrap();
        writer.certify_source(certificate(&source, 1)).unwrap();
    }
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let filters = search_filters(
        &lexical_request(),
        &index,
        Some(&ActiveSessionExclusion {
            provider: "custom".to_owned(),
            provider_session_id: "pinned-session".to_owned(),
        }),
    )
    .unwrap();
    assert!(filters.exclude_session_tree.is_none());
    assert!(filters.excluded_session_ids.is_empty());
}

#[test]
fn active_session_exclusion_contains_only_the_proven_exact_tree() {
    let temp = tempdir().unwrap();
    let source = source_named("exact-active-tree.jsonl");
    let root = record_for_session(&source, "active-root", 1, "user", "needle root");
    let mut child = record_for_session(&source, "active-child", 2, "user", "needle child");
    child.parent_session_id = Some(root.session_id);
    child.root_session_id = Some(root.session_id);
    child.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [root.clone(), child.clone()] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 2)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let filters = search_filters(
        &lexical_request(),
        &index,
        Some(&ActiveSessionExclusion {
            provider: "custom".to_owned(),
            provider_session_id: "active-root".to_owned(),
        }),
    )
    .unwrap();
    let excluded = filters.exclude_session_tree.unwrap();
    let mut expected = vec![root.session_id.as_uuid(), child.session_id.as_uuid()];
    expected.sort();
    assert_eq!(excluded.session_ids, expected);
}

#[test]
fn active_session_exclusion_follows_exact_claims_across_session_sources() {
    let temp = tempdir().unwrap();
    let active_source = source_named("active-source.jsonl");
    let foreign_source = source_named("foreign-source.jsonl");
    let active = record_for_session(&active_source, "active-session", 1, "user", "needle active");
    let mut foreign = record_for_session(
        &foreign_source,
        "foreign-session",
        1,
        "user",
        "needle foreign",
    );
    foreign.parent_session_id = Some(active.session_id);
    foreign.root_session_id = Some(active.session_id);
    foreign.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, record) in [
        (active_source, active.clone()),
        (foreign_source, foreign.clone()),
    ] {
        writer.begin_source(source.clone()).unwrap();
        writer.add_core_record(record).unwrap();
        writer.certify_source(certificate(&source, 1)).unwrap();
    }
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let filters = search_filters(
        &lexical_request(),
        &index,
        Some(&ActiveSessionExclusion {
            provider: "custom".to_owned(),
            provider_session_id: "active-session".to_owned(),
        }),
    )
    .unwrap();
    let excluded = filters.exclude_session_tree.unwrap();
    let mut expected = vec![active.session_id.as_uuid(), foreign.session_id.as_uuid()];
    expected.sort();
    assert_eq!(excluded.session_ids, expected);
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

mod pinned_workflows;

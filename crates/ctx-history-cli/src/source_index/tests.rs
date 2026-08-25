mod activity;
mod semantic_fallback;
mod show_lineage;

use std::{
    cell::Cell,
    fs,
    io::{self, Write},
    sync::{Arc, Mutex},
};

use ctx_daemon_cli::SourceBackedRefreshMode;
use ctx_history_capture::{
    provider_source_for_path, refresh_source_backed_generation,
    register_landed_source_backed_route, SourceBackedProviderRegistry, SourceBackedRouteSelection,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope as CoreAgentScope, CertifiedSource, CoreActivity,
    CoreContentPolicyStatus, CoreRecord, EventIdentityInput, LiteralFactKind, NativeItemKey,
    NativeSessionKey, ProviderDeclaredFact, ProviderNativeCopyProof, ProviderNativeEventCopy,
    ProviderNativeSessionRelationship, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey, CORE_ACTIVITY_REVISION, MAX_CORE_CONTENT_BYTES,
};
use ctx_history_index::{
    AgentScope, EventSearchFilters, GenerationWriter, IndexError, SearchContentScope,
    SessionRecord, WriterOptions, LEXICAL_QUERY_LIMITS,
};
use serde_json::{json, Value};
use tempfile::tempdir;

use crate::{
    analytics::{RenderFormat, ShowTelemetry, TargetKind},
    local_usage::CliUsage,
    output::{JsonOutputFormat, OutputFormat},
    test_query_authority::{
        publish_empty_generation, republish_active_as_legacy_v1, EmptyPublicationAuthority,
    },
    ui::{RenderContext, StreamKind, TestContext, Ui},
    ContentScopeArg, HistoryCliConfig, HistoryProvider, LocateArgs, LocateEventArgs, LocateTarget,
    SearchArgs, SearchRequest, ShowArgs, ShowEventArgs, ShowSessionArgs, ShowTarget,
    TranscriptMode,
};

use super::*;
use super::{
    render::{
        render_show_document, search_json, SEARCH_SNIPPET_MAX_BYTES, SEARCH_SNIPPET_MAX_CHARS,
    },
    search::{
        presentations_for_search_hits_with_budget, resolve_source_search_backend,
        semantic_reason_code, NormalizedSearchQuery, SearchCollection, SearchEventMetadata,
        SearchHit, SearchPresentation, SearchPresentationHydrationBudget,
        SearchPresentationRetentionBudgetExceeded, SearchResultWindow,
        SEARCH_PRESENTATION_HYDRATION_BUDGET, SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
    },
    show::{
        canonical_show_output_bytes, event_window_value, mcp_show_event, mcp_show_session,
        render_event_value, render_event_values, render_show_error, session_transcript_value,
        stream_cli_session, validate_show_target,
    },
};

mod recovery;

const TEST_SESSION_ID: &str = "019fa000-0000-7000-8000-0000000000d1";
const TEST_QUERY: &str = "pinnedgenerationrouting";
const TEST_MCP_OUTPUT_LIMIT: usize = crate::presentation_limit::CLI_PRESENTATION_MAX_OUTPUT_BYTES;

fn history_config(daemon_enabled: bool, semantic_search_enabled: bool) -> config::AppConfig {
    config::AppConfig::from_snapshot(HistoryCliConfig {
        daemon_enabled,
        semantic_search_enabled,
        local_usage_enabled: false,
        automatic_provider_discovery: true,
        provider_roots: Vec::new(),
    })
}

fn history_snapshot(daemon_enabled: bool, semantic_search_enabled: bool) -> HistoryCliConfig {
    HistoryCliConfig {
        daemon_enabled,
        semantic_search_enabled,
        local_usage_enabled: false,
        automatic_provider_discovery: true,
        provider_roots: Vec::new(),
    }
}

include!("tests/fixtures.rs");

fn assert_query_authority_error(error: &anyhow::Error, expected_code: &str) {
    let authority = error
        .downcast_ref::<ctx_history_refresh::GenerationQueryAuthorityError>()
        .expect("query gateway must preserve the typed publication-authority error");
    assert_eq!(authority.error_code(), expected_code);
}

fn generation_with_retained_peer(
    data_root: &std::path::Path,
    index: ctx_history_index::VerifiedIndex,
) -> anyhow::Result<ctx_history_read_application::GenerationRead> {
    super::compact_presentation::generation_read(
        index,
        &index_root(data_root),
        &ctx_history_read_application::GenerationReadRequest {
            target: ctx_history_read_application::GenerationReadTarget::Active,
            retained_peer: ctx_history_read_application::RetainedPeerRead::IfAvailable,
        },
    )
}

#[test]
fn source_identity_error_restores_exact_cli_flag_spelling() {
    let error = anyhow::Error::new(
        ctx_history_read_application::SourceIdentityFilterError::InvalidHistorySource,
    );

    assert_eq!(
        externalize_query_error(error).to_string(),
        "--history-source expects plugin/source or provider_key/source_id"
    );

    let error = anyhow::Error::new(
        ctx_history_read_application::SourceIdentityFilterError::CustomProviderRequired,
    );
    assert_eq!(
        externalize_query_error(error).to_string(),
        "custom history source filters can only be combined with --provider custom"
    );
}

#[test]
fn query_authority_search_and_shared_gateways_accept_legacy_nonempty_and_authoritative_current_empty(
) {
    let nonempty = tempdir().unwrap();
    write_test_generation(nonempty.path());
    let generation_id = republish_active_as_legacy_v1(nonempty.path());
    assert_eq!(
        open_index(nonempty.path()).unwrap().generation_id(),
        generation_id
    );
    assert_eq!(
        crate::semantic::pin_active_verified_generation(nonempty.path())
            .unwrap()
            .generation_id(),
        generation_id
    );

    let empty = tempdir().unwrap();
    let generation_id = publish_empty_generation(
        empty.path(),
        EmptyPublicationAuthority::AuthoritativeCurrent,
    );
    let opened = open_index(empty.path()).unwrap();
    assert_eq!(opened.generation_id(), generation_id);
    assert_eq!(opened.document_count(), 0);
    assert_eq!(
        crate::semantic::pin_active_verified_generation(empty.path())
            .unwrap()
            .generation_id(),
        generation_id
    );
}

#[test]
fn query_authority_search_and_shared_gateways_reject_uncertified_and_invalid_empty_generations() {
    for authority in [
        EmptyPublicationAuthority::Missing,
        EmptyPublicationAuthority::LegacyV1,
    ] {
        let temp = tempdir().unwrap();
        publish_empty_generation(temp.path(), authority);
        let error = match open_index(temp.path()) {
            Ok(_) => panic!("uncertified empty generation must not open for query"),
            Err(error) => error,
        };
        assert_query_authority_error(&error, "source_unavailable");
        let error = match crate::semantic::pin_active_verified_generation(temp.path()) {
            Ok(_) => panic!("uncertified empty generation must not pin for search"),
            Err(error) => error,
        };
        assert_query_authority_error(&error, "source_unavailable");
    }

    for authority in [
        EmptyPublicationAuthority::Malformed,
        EmptyPublicationAuthority::UnknownVersion,
    ] {
        let temp = tempdir().unwrap();
        publish_empty_generation(temp.path(), authority);
        let error = match open_index(temp.path()) {
            Ok(_) => panic!("invalid publication authority must not open for query"),
            Err(error) => error,
        };
        assert_query_authority_error(&error, "publication_authority_invalid");
        let error = match crate::semantic::pin_active_verified_generation(temp.path()) {
            Ok(_) => panic!("invalid publication authority must not pin for search"),
            Err(error) => error,
        };
        assert_query_authority_error(&error, "publication_authority_invalid");
    }
}

#[test]
fn retained_compact_peer_enforces_generation_query_authority() {
    let legacy_nonempty = tempdir().unwrap();
    write_test_generation(legacy_nonempty.path());
    let peer_generation = republish_active_as_legacy_v1(legacy_nonempty.path());
    let successor = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 94, 1),
        "legacy nonempty retained peer successor",
    );
    append_fixture_session(legacy_nonempty.path(), &[successor], 94);
    let current = open_index(legacy_nonempty.path()).unwrap();
    assert_ne!(current.generation_id(), peer_generation);
    let compact = generation_with_retained_peer(legacy_nonempty.path(), current).unwrap();
    assert!(compact.retained_peer().is_some());
    assert_eq!(
        ctx_history_read_application::CompactPresentationProjection::new(
            compact.index(),
            compact.retained_peer(),
        )
        .project(&json!({"kind": "probe"}))
        .unwrap()["kind"],
        "probe"
    );

    let authoritative_empty = tempdir().unwrap();
    let peer_generation = publish_empty_generation(
        authoritative_empty.path(),
        EmptyPublicationAuthority::AuthoritativeCurrent,
    );
    write_test_generation(authoritative_empty.path());
    let current = open_index(authoritative_empty.path()).unwrap();
    assert_ne!(current.generation_id(), peer_generation);
    generation_with_retained_peer(authoritative_empty.path(), current).unwrap();

    for (authority, error_code) in [
        (EmptyPublicationAuthority::Missing, "source_unavailable"),
        (EmptyPublicationAuthority::LegacyV1, "source_unavailable"),
        (
            EmptyPublicationAuthority::Malformed,
            "publication_authority_invalid",
        ),
        (
            EmptyPublicationAuthority::UnknownVersion,
            "publication_authority_invalid",
        ),
    ] {
        let temp = tempdir().unwrap();
        publish_empty_generation(temp.path(), authority);
        write_test_generation(temp.path());
        let current = open_index(temp.path()).unwrap();
        let error = generation_with_retained_peer(temp.path(), current)
            .err()
            .expect("invalid retained peer must fail before compact resolution");
        assert_query_authority_error(&error, error_code);
    }
}

#[test]
fn query_authority_show_locate_and_mcp_show_reject_empty_before_not_found() {
    let temp = tempdir().unwrap();
    publish_empty_generation(temp.path(), EmptyPublicationAuthority::LegacyV1);
    let missing_id = "00000000-0000-0000-0000-000000000001";

    let (mut ui, _) = test_ui();
    let mut telemetry = ShowTelemetry {
        target_kind: TargetKind::Event,
        transcript_mode: None,
        output_format: RenderFormat::Json,
        writes_out_file: false,
        provider_lookup: false,
        window: None,
        events_returned: None,
    };
    let mut usage = CliUsage::excluded();
    let show_error = run_show(
        ShowArgs {
            target: ShowTarget::Event(show_event_args(missing_id)),
        },
        temp.path().to_path_buf(),
        &mut telemetry,
        &mut usage,
        &mut ui,
    )
    .unwrap_err();
    assert_query_authority_error(&show_error, "source_unavailable");

    let (mut ui, _) = test_ui();
    let mut usage = CliUsage::excluded();
    let locate_error = run_locate(
        LocateArgs {
            target: LocateTarget::Event(LocateEventArgs {
                id: missing_id.to_owned(),
                format: JsonOutputFormat::Json,
            }),
        },
        temp.path().to_path_buf(),
        &mut usage,
        &mut ui,
    )
    .unwrap_err();
    assert_query_authority_error(&locate_error, "source_unavailable");

    let mcp_error =
        mcp_show_event(temp.path(), missing_id, 0, 0, None, TEST_MCP_OUTPUT_LIMIT).unwrap_err();
    assert_query_authority_error(&mcp_error, "source_unavailable");
}

struct FailingWriter(&'static str);

impl Write for FailingWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::other(self.0))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn lexical_search_args() -> SearchArgs {
    SearchArgs {
        query: Some(TEST_QUERY.to_owned()),
        term: Vec::new(),
        limit: 10,
        provider: Some(crate::ProviderArg(HistoryProvider::Native(
            CaptureProvider::Codex,
        ))),
        history_source: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        source_roots: Vec::new(),
        source_groups: Vec::new(),
        workspace: None,
        since: None,
        primary_only: false,
        content_scope: None,
        event_type: None,
        file: None,
        session: None,
        exclude_sessions: Vec::new(),
        events: false,
        backend: Some(crate::SearchBackendArg::Lexical),
        semantic_weight: 0.35,
        refresh: RefreshArg::Off,
        include_current_session: true,
        format: JsonOutputFormat::Text,
        verbose: false,
    }
}

#[test]
fn search_reports_terminal_observation_after_output_failure() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    let stderr_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    let mut ui = Ui::with_writers(
        FailingWriter("injected search output failure"),
        context,
        SharedWriter::default(),
        stderr_context,
    );
    let mut local_usage = CliUsage::excluded();
    let mut observed = None;
    let error = run_search(
        lexical_search_args(),
        temp.path().to_path_buf(),
        history_snapshot(false, false),
        &mut local_usage,
        &mut ui,
        |observation| observed = Some(observation),
    )
    .unwrap_err();

    assert!(error.to_string().contains("injected search output failure"));
    let observation = observed.expect("search attempt must emit one terminal observation");
    assert_eq!(
        observation.backend_effective,
        Some(ctx_history_read_application::SearchBackend::Lexical)
    );
    assert!(observation.result_count.is_some_and(|count| count > 0));
    assert!(observation.render_duration.is_some());
    assert_eq!(
        observation.failure_phase,
        Some(crate::SearchFailurePhase::Output)
    );
}

#[test]
fn search_not_ready_diagnostic_write_failure_reports_output_phase() {
    let mut ui = Ui::with_writers(
        SharedWriter::default(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
        FailingWriter("injected not-ready diagnostic failure"),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
    );
    let mut observation = crate::SearchExecutionObservation::default();

    let error =
        search::render_not_ready_at_search_boundary(&mut ui, Some(&mut observation)).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("injected not-ready diagnostic failure"),
        "{error:#}"
    );
    assert_eq!(
        observation.failure_phase,
        Some(crate::SearchFailurePhase::Output)
    );
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
    assert_eq!(normalized.positional(), Some("build failure"));
    assert_eq!(normalized.terms(), &["release's checksum", "BUILD FAILURE"]);

    source_request.query.clear();
    source_request.terms = vec!["  term-only  ".to_owned()];
    let term_only = NormalizedSearchQuery::from_request(&source_request);
    assert_eq!(term_only.display(), "term-only");
    assert_eq!(term_only.positional(), None);
    assert_eq!(term_only.terms(), &["term-only"]);
    source_request.terms = vec!["--option-like".to_owned()];
    assert_eq!(
        NormalizedSearchQuery::from_request(&source_request).terms(),
        &["--option-like"]
    );
}

#[test]
fn omitted_and_explicit_all_resolve_to_identical_weighted_retrieval() {
    let search_args = |content_scope| SearchArgs {
        query: Some(TEST_QUERY.to_owned()),
        term: Vec::new(),
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
        content_scope,
        event_type: None,
        file: None,
        session: None,
        exclude_sessions: Vec::new(),
        events: false,
        backend: Some(crate::SearchBackendArg::Hybrid),
        semantic_weight: 0.35,
        refresh: RefreshArg::Off,
        include_current_session: true,
        format: JsonOutputFormat::Json,
        verbose: false,
    };
    let omitted = SourceSearchRequest::from(SearchRequest::from(search_args(None)));
    let explicit =
        SourceSearchRequest::from(SearchRequest::from(search_args(Some(ContentScopeArg::All))));
    assert_eq!(omitted.content_scope, SearchContentScope::All);
    assert_eq!(explicit.content_scope, SearchContentScope::All);

    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();
    let omitted_filters = index_search_filters(&omitted, &index).unwrap();
    let explicit_filters = index_search_filters(&explicit, &index).unwrap();
    assert_eq!(omitted_filters, explicit_filters);

    let collect = |request: &SourceSearchRequest, filters: &EventSearchFilters| {
        collect_search_hits_with_backend_using(
            request,
            &index,
            temp.path(),
            request.semantic_weight,
            filters,
            |index, _data_root, query, filters, candidate_limit| {
                Ok((
                    index.search_event_candidates_any_with_filters(
                        &[query],
                        filters,
                        candidate_limit,
                    )?,
                    json!({"fixture": "weighted"}),
                ))
            },
        )
        .unwrap()
    };
    let omitted_collection = collect(&omitted, &omitted_filters);
    let explicit_collection = collect(&explicit, &explicit_filters);
    assert_eq!(
        omitted_collection
            .result_window
            .hits
            .iter()
            .map(|hit| (hit.event.event_id, hit.score))
            .collect::<Vec<_>>(),
        explicit_collection
            .result_window
            .hits
            .iter()
            .map(|hit| (hit.event.event_id, hit.score))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        omitted_collection.effective_backend,
        ctx_history_read_application::SearchBackend::Hybrid
    );
    assert_eq!(
        explicit_collection.effective_backend,
        ctx_history_read_application::SearchBackend::Hybrid
    );
    assert_eq!(
        omitted_collection.semantic_weight,
        explicit_collection.semantic_weight
    );
}

#[test]
fn content_scope_forwards_with_provider_workspace_since_file_agent_and_current_session_controls() {
    let request = SourceSearchRequest::from(SearchRequest::from(SearchArgs {
        query: Some(TEST_QUERY.to_owned()),
        term: Vec::new(),
        limit: 10,
        provider: Some(crate::ProviderArg(HistoryProvider::Native(
            CaptureProvider::Codex,
        ))),
        history_source: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        source_roots: Vec::new(),
        source_groups: Vec::new(),
        workspace: Some("/workspace/pinned".to_owned()),
        since: Some("30d".to_owned()),
        primary_only: false,
        content_scope: Some(ContentScopeArg::Calls),
        event_type: None,
        file: Some("src/lib.rs".into()),
        session: None,
        exclude_sessions: Vec::new(),
        events: true,
        backend: None,
        semantic_weight: 0.35,
        refresh: RefreshArg::Off,
        include_current_session: true,
        format: JsonOutputFormat::Json,
        verbose: false,
    }));
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();
    let filters = index_search_filters(&request, &index).unwrap();

    assert_eq!(request.content_scope, SearchContentScope::Calls);
    assert!(request.events);
    assert_eq!(filters.content_scope, SearchContentScope::Calls);
    assert_eq!(filters.provider.as_deref(), Some("codex"));
    assert_eq!(filters.workspace.as_deref(), Some("/workspace/pinned"));
    assert!(filters.since_unix_ms.is_some());
    assert_eq!(filters.file.as_deref(), Some("src/lib.rs"));
    assert_eq!(filters.agent_scope, AgentScope::All);
    assert!(filters.exclude_session_tree.is_none());

    let mut primary_only_request = request.clone();
    primary_only_request.primary_only = true;
    let primary_only_filters = index_search_filters(&primary_only_request, &index).unwrap();
    assert_eq!(primary_only_filters.agent_scope, AgentScope::Primary);
}

#[test]
fn oversized_single_query_is_rejected_before_refresh_coordination() {
    let mut source_request = request(RefreshArg::Off);
    source_request.query = "x".repeat(LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes + 1);
    let coordinated = Cell::new(false);

    let error = refresh_for_search_with(
        &source_request,
        RefreshArg::Off,
        Path::new("/query-limit-test-does-not-open"),
        |_, _| {
            coordinated.set(true);
            panic!("oversized query must fail before refresh coordination")
        },
    )
    .err()
    .expect("oversized query must be rejected");

    let error = error.into_anyhow();
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
        RefreshArg::Off,
        Path::new("/query-limit-test-does-not-open"),
        |_, _| {
            coordinated.set(true);
            panic!("repeated terms must fail before refresh coordination")
        },
    )
    .err()
    .expect("repeated terms must be rejected");

    let error = error.into_anyhow();
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
    let core_event = fixture_core_event(&event, "Core-owned search snippet");
    let mut source_request = request(RefreshArg::Off);
    source_request.query = "  primary query ".to_owned();
    source_request.terms = vec!["term with spaces".to_owned()];
    source_request.limit = 1;
    let collection = SearchCollection {
        result_window: SearchResultWindow {
            limit: 1,
            hits: vec![SearchHit {
                event: SearchEventMetadata::from(&event),
                score: 1.0,
                more_matches_in_session: 0,
            }],
            more_available: false,
        },
        candidate_pool: 1,
        candidate_pool_truncated: false,
        lexical_diagnostics: None,
        diversification: ctx_history_read_application::SearchDiversificationDecision {
            status: ctx_history_read_application::SearchDiversificationStatus::Applied,
            top_n: 1,
            changed_final_top_n: Some(false),
        },
        requested_backend: SearchBackendArg::Lexical,
        effective_backend: SearchBackendArg::Lexical,
        semantic_weight: 0.0,
        semantic_status: "skipped",
        semantic_fallback: None,
        semantic_diagnostics: None,
        work: ctx_history_read_application::SearchWorkReceipt::default(),
        stop_reason: ctx_history_read_application::SearchStopReason::FixedPool,
    };
    let follow_up_root = std::path::Path::new("/tmp/ctx root/owner's history");
    let value = search_json(
        &source_request,
        follow_up_root,
        &index,
        &collection,
        &EventSearchFilters::default(),
        &[fixture_search_presentation(
            &collection.result_window.hits[0].event,
            core_event,
            false,
        )],
        "existing_generation",
        1,
        std::time::Duration::ZERO,
    )
    .unwrap();

    assert_eq!(
        sorted_json_keys(&value),
        vec![
            "diversification",
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
    assert!(chrono::DateTime::parse_from_rfc3339(value["generated_at"].as_str().unwrap()).is_ok());
    assert_eq!(value["query"], "primary query OR term with spaces");
    assert_eq!(value["filters"]["content_scope"], "all");
    assert!(value["results"][0].get("session_relationship").is_none());
    assert!(value["results"][0].get("event_copy").is_none());
    assert!(value["results"][0].get("copied_lineage").is_none());
    assert_eq!(
        value["diversification"],
        json!({
            "status": "applied",
            "top_n": 1,
            "changed_final_top_n": false,
        })
    );
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
    let first_session_id = first.session_id.as_uuid();
    let second_session_id = second.session_id.as_uuid();
    let first_core = fixture_core_event(&first, "first shaped result");
    let second_core = fixture_core_event(&second, "second shaped result");
    let mut source_request = request(RefreshArg::Off);
    source_request.limit = 2;
    let collection = SearchCollection {
        result_window: SearchResultWindow {
            limit: 2,
            hits: vec![
                SearchHit {
                    event: SearchEventMetadata::from(&first),
                    score: 0.25,
                    more_matches_in_session: 0,
                },
                SearchHit {
                    event: SearchEventMetadata::from(&second),
                    score: 9.5,
                    more_matches_in_session: 0,
                },
            ],
            more_available: false,
        },
        candidate_pool: 2,
        candidate_pool_truncated: false,
        lexical_diagnostics: None,
        diversification: ctx_history_read_application::SearchDiversificationDecision {
            status: ctx_history_read_application::SearchDiversificationStatus::Applied,
            top_n: 2,
            changed_final_top_n: Some(true),
        },
        requested_backend: SearchBackendArg::Lexical,
        effective_backend: SearchBackendArg::Lexical,
        semantic_weight: 0.0,
        semantic_status: "skipped",
        semantic_fallback: None,
        semantic_diagnostics: None,
        work: ctx_history_read_application::SearchWorkReceipt::default(),
        stop_reason: ctx_history_read_application::SearchStopReason::FixedPool,
    };
    let mut presentations = [
        fixture_search_presentation(&collection.result_window.hits[0].event, first_core, false),
        fixture_search_presentation(&collection.result_window.hits[1].event, second_core, false),
    ];
    let value = search_json(
        &source_request,
        temp.path(),
        &index,
        &collection,
        &EventSearchFilters::default(),
        &presentations,
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
    for (result, event_id, session_id) in [
        (&results[0], first_id, first_session_id),
        (&results[1], second_id, second_session_id),
    ] {
        assert_eq!(result["item_id"], session_id.to_string());
        assert_eq!(result["ctx_event_id"], event_id.to_string());
        assert_eq!(result["ctx_session_id"], session_id.to_string());
        assert_eq!(result["event_id"], event_id.to_string());
        assert_eq!(result["session_id"], session_id.to_string());
        let citation = &result["citations"][0];
        assert_eq!(citation["item_id"], event_id.to_string());
        assert_eq!(citation["ctx_event_id"], event_id.to_string());
        assert_eq!(citation["ctx_session_id"], session_id.to_string());
        assert_eq!(citation["session_id"], session_id.to_string());
        assert!(result["suggested_next_commands"][1]
            .as_str()
            .unwrap()
            .ends_with(&format!("show event {event_id} --window 10")));
    }

    presentations.swap(0, 1);
    let error = search_json(
        &source_request,
        temp.path(),
        &index,
        &collection,
        &EventSearchFilters::default(),
        &presentations,
        "existing_generation",
        1,
        std::time::Duration::ZERO,
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("out-of-order search presentation"));
}

#[test]
fn show_schema_v1_reads_complete_normalized_core_content() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();
    let session = index
        .sessions_by_provider_session_id(TEST_SESSION_ID, Some("codex"), None, None)
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
    assert_eq!(
        event_value["event"]["text"],
        selected
            .core_record
            .content
            .normalized_body
            .as_deref()
            .unwrap()
    );
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
    assert!(selected.get("activity").is_none());
    assert!(redacted.get("activity").is_none());
    assert!(omitted.get("activity").is_none());
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
fn result_window_discards_non_render_event_metadata() {
    let (event_id, expected, window) = {
        let mut event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 90, 1);
        event.native_event_id = Some(TypedKey::utf8("x".repeat(60 * 1024)).unwrap());
        let event_id = event.event_id.as_uuid();
        let expected = SearchEventMetadata::from(&event);
        let candidate = EventSearchCandidate { event, score: 1.0 };
        let window = shape_search_result_window(std::iter::once(&candidate), 1, true);
        assert_eq!(window.hits[0].event, expected);
        (event_id, expected, window)
    };

    assert_eq!(window.hits.len(), 1);
    assert_eq!(window.hits[0].event, expected);
    assert_eq!(window.hits[0].event.event_id, event_id);
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
        .sessions_by_provider_session_id(TEST_SESSION_ID, None, None, None)
        .unwrap();
    assert_eq!(matches.len(), 2);
    let error = resolve_show_session(&index, None, Some(TEST_SESSION_ID), None).unwrap_err();
    let detail = error.to_string();
    assert!(detail.contains("is ambiguous"), "{detail}");
    for session in matches {
        assert!(detail.contains(&session.session_id.to_string()), "{detail}");
    }
    assert!(
        detail.contains(
            "pass --provider, --provider-key/--source-id for custom history, or a ctx session ID"
        ),
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
fn missing_root_does_not_reclassify_an_unrelated_refresh_io_error() {
    let temp = tempdir().unwrap();
    let error = match refresh_for_search_with(
        &request(RefreshArg::Off),
        RefreshArg::Off,
        temp.path(),
        |_, _| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected refresh authority read failure",
            )
            .into())
        },
    ) {
        Ok(_) => panic!("an unrelated refresh I/O failure must remain an application failure"),
        Err(error) => error.into_anyhow(),
    };

    assert!(matches!(
        error.downcast_ref::<io::Error>(),
        Some(error) if error.kind() == io::ErrorKind::PermissionDenied
    ));
    assert!(error
        .downcast_ref::<ctx_history_refresh::MissingActiveGeneration>()
        .is_none());
}

mod additional;

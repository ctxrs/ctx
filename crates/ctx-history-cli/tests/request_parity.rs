use std::path::PathBuf;

use ctx_history_cli::{
    list_events_selection_from_request, ContentScopeArg, EventContentProjection,
    EventContentProjectionArg, EventQueryDirection, EventQueryScope, EventQueryWireRequest,
    JsonOutputFormat, ListEventsArgs, ListEventsContentProjection, ListEventsDirection,
    ListEventsRequest, ListEventsScope, LocateRequest, OutputFormat, RefreshMode, SearchArgs,
    SearchBackend, SearchBackendArg, SearchRequest, ShowRequest, TranscriptMode,
};
use ctx_history_index::{CoreEventRangeDirection, CoreEventRangeScope, SearchContentScope};
use ctx_history_read_application::SearchBackend as ExecutionSearchBackend;

#[test]
fn search_request_preserves_every_content_scope_and_backend_into_execution() {
    for (scope, expected_scope) in [
        (ContentScopeArg::All, SearchContentScope::All),
        (ContentScopeArg::Transcript, SearchContentScope::Transcript),
        (ContentScopeArg::Calls, SearchContentScope::Calls),
        (ContentScopeArg::Outputs, SearchContentScope::Outputs),
    ] {
        for (backend, expected_backend) in [
            (SearchBackendArg::Hybrid, ExecutionSearchBackend::Hybrid),
            (SearchBackendArg::Lexical, ExecutionSearchBackend::Lexical),
            (SearchBackendArg::Semantic, ExecutionSearchBackend::Semantic),
        ] {
            let neutral = SearchRequest::from(SearchArgs {
                query: Some("needle".to_owned()),
                term: vec!["other".to_owned()],
                limit: 17,
                provider: None,
                history_source: Some("history".to_owned()),
                provider_key: Some("key".to_owned()),
                source_id: Some("source".to_owned()),
                source_format: Some("jsonl".to_owned()),
                source_roots: vec!["personal".to_owned()],
                source_groups: vec!["work".to_owned()],
                workspace: Some("workspace".to_owned()),
                since: Some("2026-01-01".to_owned()),
                primary_only: true,
                content_scope: Some(scope),
                event_type: Some("message".to_owned()),
                file: Some(PathBuf::from("src/lib.rs")),
                session: Some("session".to_owned()),
                exclude_sessions: vec!["excluded".to_owned()],
                events: false,
                backend: Some(backend),
                semantic_weight: 0.25,
                refresh: RefreshMode::Wait,
                include_current_session: true,
                format: JsonOutputFormat::Json,
                verbose: true,
            });
            let execution = ctx_history_read_application::SearchRequest::from(neutral);
            assert_eq!(execution.content_scope, expected_scope);
            assert_eq!(execution.backend, Some(expected_backend));
            assert_eq!(execution.source_roots, ["personal"]);
            assert_eq!(execution.source_groups, ["work"]);
            assert!(execution.events);
        }
    }
}

#[test]
fn list_request_preserves_every_scope_direction_and_content_projection_into_execution() {
    for (scope, expected_scope, core_scope) in [
        (
            EventQueryScope::All,
            ListEventsScope::All,
            CoreEventRangeScope::All,
        ),
        (
            EventQueryScope::Primary,
            ListEventsScope::Primary,
            CoreEventRangeScope::Primary,
        ),
        (
            EventQueryScope::Subagent,
            ListEventsScope::Subagent,
            CoreEventRangeScope::Subagent,
        ),
    ] {
        for (direction, expected_direction, core_direction) in [
            (
                EventQueryDirection::Ascending,
                ListEventsDirection::Ascending,
                CoreEventRangeDirection::Ascending,
            ),
            (
                EventQueryDirection::Descending,
                ListEventsDirection::Descending,
                CoreEventRangeDirection::Descending,
            ),
        ] {
            for (content, expected_content, wire_content) in [
                (
                    EventContentProjectionArg::Full,
                    ListEventsContentProjection::Full,
                    EventContentProjection::Full,
                ),
                (
                    EventContentProjectionArg::Text,
                    ListEventsContentProjection::Text,
                    EventContentProjection::Text,
                ),
                (
                    EventContentProjectionArg::None,
                    ListEventsContentProjection::None,
                    EventContentProjection::None,
                ),
            ] {
                let parsed = ListEventsArgs {
                    scope,
                    direction,
                    content,
                    limit: 11,
                    provider: vec!["codex".to_owned()],
                    workspace: Some("workspace".to_owned()),
                    ..ListEventsArgs::default()
                };
                let neutral = ListEventsRequest::from(parsed);
                assert_eq!(neutral.scope, expected_scope);
                assert_eq!(neutral.direction, expected_direction);
                assert_eq!(neutral.content, expected_content);

                let projection = match neutral.content {
                    ListEventsContentProjection::Full => EventContentProjection::Full,
                    ListEventsContentProjection::Text => EventContentProjection::Text,
                    ListEventsContentProjection::None => EventContentProjection::None,
                };
                let selection = list_events_selection_from_request(neutral).unwrap();
                assert_eq!(selection.filters().scope, core_scope);
                assert_eq!(selection.filters().direction, core_direction);
                let wire = EventQueryWireRequest::from_selection(&selection, projection, 11);
                assert_eq!(wire.content, wire_content);
            }
        }
    }
}

#[test]
fn neutral_search_backend_is_explicit_and_not_reconstructed_from_defaults() {
    let request = SearchRequest {
        query: Some("needle".to_owned()),
        terms: Vec::new(),
        limit: 1,
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
        content_scope: ctx_history_cli::SearchContentScope::Calls,
        event_type: None,
        file: None,
        session: None,
        exclude_sessions: Vec::new(),
        events: true,
        backend: Some(SearchBackend::Semantic),
        semantic_weight: 1.0,
        refresh: RefreshMode::Off,
        include_current_session: false,
        format: ctx_history_cli::OutputFormat::Json,
        verbose: false,
    };
    let execution = ctx_history_read_application::SearchRequest::from(request);
    assert_eq!(execution.backend, Some(ExecutionSearchBackend::Semantic));
    assert_eq!(execution.content_scope, SearchContentScope::Calls);
}

#[test]
fn show_and_locate_requests_preserve_custom_route_qualifiers() {
    let show = ShowRequest::Session {
        id: None,
        provider: None,
        provider_session: Some("provider-session".to_owned()),
        provider_key: Some("amp".to_owned()),
        source_id: Some("threads".to_owned()),
        mode: TranscriptMode::Lite,
        max_events: None,
        format: OutputFormat::Json,
        out: None,
    };
    assert!(matches!(
        show,
        ShowRequest::Session {
            provider_key: Some(ref provider_key),
            source_id: Some(ref source_id),
            ..
        } if provider_key == "amp" && source_id == "threads"
    ));

    let locate = LocateRequest::Session {
        id: None,
        provider: None,
        provider_session: Some("provider-session".to_owned()),
        provider_key: Some("amp".to_owned()),
        source_id: Some("threads".to_owned()),
        format: OutputFormat::Json,
    };
    assert!(matches!(
        locate,
        LocateRequest::Session {
            provider_key: Some(ref provider_key),
            source_id: Some(ref source_id),
            ..
        } if provider_key == "amp" && source_id == "threads"
    ));
}

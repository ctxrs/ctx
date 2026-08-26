use std::path::PathBuf;

use ctx_history_core::CaptureProvider;

/// Provider identity after parsing. Parser spelling and aliases remain a final
/// `ctx` concern; this value preserves the canonical provider identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryProvider {
    Native(CaptureProvider),
    Custom,
}

impl HistoryProvider {
    pub const fn capture_provider(self) -> CaptureProvider {
        match self {
            Self::Native(provider) => provider,
            Self::Custom => CaptureProvider::Custom,
        }
    }
}

impl From<CaptureProvider> for HistoryProvider {
    fn from(provider: CaptureProvider) -> Self {
        if provider == CaptureProvider::Custom {
            Self::Custom
        } else {
            Self::Native(provider)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
    Jsonl,
    Markdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    Auto,
    Plain,
    Json,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptMode {
    Full,
    Lite,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshMode {
    Background,
    Off,
    Wait,
}

impl RefreshMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Off => "off",
            Self::Wait => "wait",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBackend {
    Hybrid,
    Lexical,
    Semantic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchContentScope {
    All,
    Transcript,
    Calls,
    Outputs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListEventsScope {
    All,
    Primary,
    Subagent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListEventsDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListEventsContentProjection {
    Full,
    Text,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFormat {
    CtxHistoryJsonlV2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchRequest {
    pub query: Option<String>,
    pub terms: Vec<String>,
    pub limit: usize,
    pub provider: Option<HistoryProvider>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub source_roots: Vec<String>,
    pub source_groups: Vec<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub primary_only: bool,
    pub content_scope: SearchContentScope,
    pub event_type: Option<String>,
    pub file: Option<PathBuf>,
    pub session: Option<String>,
    pub exclude_sessions: Vec<String>,
    pub events: bool,
    pub backend: Option<SearchBackend>,
    pub semantic_weight: f32,
    pub refresh: RefreshMode,
    pub include_current_session: bool,
    pub format: OutputFormat,
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShowRequest {
    Session {
        id: Option<String>,
        provider: Option<HistoryProvider>,
        provider_session: Option<String>,
        provider_key: Option<String>,
        source_id: Option<String>,
        mode: TranscriptMode,
        max_events: Option<usize>,
        format: OutputFormat,
        out: Option<PathBuf>,
    },
    Event {
        id: String,
        before: usize,
        after: usize,
        window: Option<usize>,
        format: OutputFormat,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateRequest {
    Session {
        id: Option<String>,
        provider: Option<HistoryProvider>,
        provider_session: Option<String>,
        provider_key: Option<String>,
        source_id: Option<String>,
        format: OutputFormat,
    },
    Event {
        id: String,
        format: OutputFormat,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRequest {
    pub events: ListEventsRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEventsRequest {
    pub since: Option<String>,
    pub until: Option<String>,
    pub providers: Vec<String>,
    pub source: Option<String>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub provider_session: Option<String>,
    pub session: Option<String>,
    pub parent_session: Option<String>,
    pub root_session: Option<String>,
    pub branch: Option<String>,
    pub workspace: Option<String>,
    pub event_type: Option<String>,
    pub role: Option<String>,
    pub file: Option<String>,
    pub cursor: Option<String>,
    pub limit: u64,
    pub format: OutputFormat,
    pub scope: ListEventsScope,
    pub direction: ListEventsDirection,
    pub content: ListEventsContentProjection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    pub provider: Option<HistoryProvider>,
    pub path: Option<PathBuf>,
    pub relocate_from: Option<PathBuf>,
    pub history_source: Option<String>,
    pub history_source_manifests: Vec<PathBuf>,
    pub reset_cursor: bool,
    pub input_format: Option<ImportFormat>,
    pub all: bool,
    pub resume: bool,
    pub partial: bool,
    pub no_daemon: bool,
    pub format: OutputFormat,
    pub progress: ProgressMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupRequest {
    pub catalog_only: bool,
    pub semantic: bool,
    pub no_daemon: bool,
    pub wait: bool,
    pub format: OutputFormat,
    pub progress: ProgressMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcesRequest {
    pub provider: Option<HistoryProvider>,
    pub all: bool,
    pub show_missing: bool,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceIndexRequest {
    Search(SearchRequest),
    Show(ShowRequest),
    Locate(LocateRequest),
    List(ListRequest),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_index_request_keeps_transport_modes_neutral() {
        let request = SourceIndexRequest::Show(ShowRequest::Session {
            id: Some("session".to_owned()),
            provider: Some(HistoryProvider::Native(CaptureProvider::Codex)),
            provider_session: None,
            provider_key: None,
            source_id: None,
            mode: TranscriptMode::Lite,
            max_events: None,
            format: OutputFormat::Markdown,
            out: Some(PathBuf::from("out.md")),
        });

        assert!(matches!(request, SourceIndexRequest::Show(_)));
    }

    #[test]
    fn search_request_preserves_every_scope_backend_pair_for_execution() {
        use crate::cli::{ContentScopeArg, SearchArgs, SearchBackendArg};

        for (scope_arg, expected_scope) in [
            (
                ContentScopeArg::All,
                ctx_history_index::SearchContentScope::All,
            ),
            (
                ContentScopeArg::Transcript,
                ctx_history_index::SearchContentScope::Transcript,
            ),
            (
                ContentScopeArg::Calls,
                ctx_history_index::SearchContentScope::Calls,
            ),
            (
                ContentScopeArg::Outputs,
                ctx_history_index::SearchContentScope::Outputs,
            ),
        ] {
            for (backend_arg, expected_backend) in [
                (
                    SearchBackendArg::Hybrid,
                    ctx_history_read_application::SearchBackend::Hybrid,
                ),
                (
                    SearchBackendArg::Lexical,
                    ctx_history_read_application::SearchBackend::Lexical,
                ),
                (
                    SearchBackendArg::Semantic,
                    ctx_history_read_application::SearchBackend::Semantic,
                ),
            ] {
                let request = SearchRequest::from(SearchArgs {
                    query: Some("needle".to_owned()),
                    term: vec!["extra".to_owned()],
                    limit: 7,
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
                    content_scope: Some(scope_arg),
                    event_type: Some("message".to_owned()),
                    file: Some(PathBuf::from("src/lib.rs")),
                    session: Some("session".to_owned()),
                    exclude_sessions: vec!["excluded".to_owned()],
                    events: false,
                    backend: Some(backend_arg),
                    semantic_weight: 0.25,
                    refresh: RefreshMode::Wait,
                    include_current_session: true,
                    format: crate::JsonOutputFormat::Json,
                    verbose: true,
                });
                let execution = ctx_history_read_application::SearchRequest::from(request);
                assert_eq!(execution.content_scope, expected_scope);
                assert_eq!(execution.backend, Some(expected_backend));
                assert_eq!(execution.source_roots, ["personal"]);
                assert_eq!(execution.source_groups, ["work"]);
                assert!(
                    execution.events,
                    "a session selector must retain event-result semantics"
                );
                assert_eq!(execution.exclude_sessions, ["excluded"]);
            }
        }
    }

    #[test]
    fn list_events_request_preserves_every_scope_direction_content_pair_for_execution() {
        use crate::{
            list_events::{
                selection_from_request, EventContentProjection, EventContentProjectionArg,
                EventQueryDirection, EventQueryScope, EventQueryWireRequest, ListEventsArgs,
            },
            ListEventsContentProjection, ListEventsDirection, ListEventsScope,
        };
        use ctx_history_index::{CoreEventRangeDirection, CoreEventRangeScope};

        for (scope, expected_scope, expected_core_scope) in [
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
            for (direction, expected_direction, expected_core_direction) in [
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
                for (content, expected_content, expected_wire_content) in [
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
                        limit: 9,
                        provider: vec!["codex".to_owned()],
                        workspace: Some("workspace".to_owned()),
                        ..ListEventsArgs::default()
                    };
                    let request = ListEventsRequest::from(parsed);
                    assert_eq!(request.scope, expected_scope);
                    assert_eq!(request.direction, expected_direction);
                    assert_eq!(request.content, expected_content);

                    let projection = match request.content {
                        ListEventsContentProjection::Full => EventContentProjection::Full,
                        ListEventsContentProjection::Text => EventContentProjection::Text,
                        ListEventsContentProjection::None => EventContentProjection::None,
                    };
                    let selection = selection_from_request(request).unwrap();
                    assert_eq!(selection.filters().scope, expected_core_scope);
                    assert_eq!(selection.filters().direction, expected_core_direction);
                    let wire = EventQueryWireRequest::from_selection(&selection, projection, 9);
                    assert_eq!(wire.content, expected_wire_content);
                }
            }
        }
    }
}

//! Plain parsed command values. Clap conversion remains in the final binary.

use std::path::PathBuf;

use crate::provider_args::ProviderArg;
use crate::{
    output::JsonOutputFormat, OutputFormat, RefreshMode, SearchBackend, SearchContentScope,
    SearchRequest, TranscriptMode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBackendArg {
    Hybrid,
    Lexical,
    Semantic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentScopeArg {
    All,
    Transcript,
    Calls,
    Outputs,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchArgs {
    pub query: Option<String>,
    pub term: Vec<String>,
    pub limit: usize,
    pub provider: Option<ProviderArg>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub source_roots: Vec<String>,
    pub source_groups: Vec<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub primary_only: bool,
    pub content_scope: Option<ContentScopeArg>,
    pub event_type: Option<String>,
    pub file: Option<PathBuf>,
    pub session: Option<String>,
    pub exclude_sessions: Vec<String>,
    pub events: bool,
    pub backend: Option<SearchBackendArg>,
    pub semantic_weight: f32,
    pub refresh: RefreshMode,
    pub include_current_session: bool,
    pub format: JsonOutputFormat,
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowArgs {
    pub target: ShowTarget,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShowTarget {
    Session(ShowSessionArgs),
    Event(ShowEventArgs),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowSessionArgs {
    pub id: Option<String>,
    pub provider: Option<ProviderArg>,
    pub provider_session: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub mode: TranscriptMode,
    pub max_events: Option<usize>,
    pub format: OutputFormat,
    pub out: Option<PathBuf>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowEventArgs {
    pub id: String,
    pub before: usize,
    pub after: usize,
    pub window: Option<usize>,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocateArgs {
    pub target: LocateTarget,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateTarget {
    Session(LocateSessionArgs),
    Event(LocateEventArgs),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocateSessionArgs {
    pub id: Option<String>,
    pub provider: Option<ProviderArg>,
    pub provider_session: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub format: JsonOutputFormat,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocateEventArgs {
    pub id: String,
    pub format: JsonOutputFormat,
}

impl From<SearchArgs> for SearchRequest {
    fn from(args: SearchArgs) -> Self {
        Self {
            query: args.query,
            terms: args.term,
            limit: args.limit,
            provider: args.provider.map(|value| value.0),
            history_source: args.history_source,
            provider_key: args.provider_key,
            source_id: args.source_id,
            source_format: args.source_format,
            source_roots: args.source_roots,
            source_groups: args.source_groups,
            workspace: args.workspace,
            since: args.since,
            primary_only: args.primary_only,
            content_scope: match args.content_scope.unwrap_or(ContentScopeArg::All) {
                ContentScopeArg::All => SearchContentScope::All,
                ContentScopeArg::Transcript => SearchContentScope::Transcript,
                ContentScopeArg::Calls => SearchContentScope::Calls,
                ContentScopeArg::Outputs => SearchContentScope::Outputs,
            },
            event_type: args.event_type,
            file: args.file,
            session: args.session,
            exclude_sessions: args.exclude_sessions,
            events: args.events,
            backend: args.backend.map(|value| match value {
                SearchBackendArg::Hybrid => SearchBackend::Hybrid,
                SearchBackendArg::Lexical => SearchBackend::Lexical,
                SearchBackendArg::Semantic => SearchBackend::Semantic,
            }),
            semantic_weight: args.semantic_weight,
            refresh: args.refresh,
            include_current_session: args.include_current_session,
            format: match args.format {
                JsonOutputFormat::Text => OutputFormat::Text,
                JsonOutputFormat::Json => OutputFormat::Json,
            },
            verbose: args.verbose,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ContentScopeArg, SearchArgs};
    use crate::{JsonOutputFormat, RefreshMode, SearchBackendArg, SearchRequest};

    #[test]
    fn owned_search_conversion_reuses_request_buffers() {
        let query = "query buffer".to_owned();
        let query_pointer = query.as_ptr();
        let term = "term buffer".to_owned();
        let term_pointer = term.as_ptr();
        let workspace = "workspace buffer".to_owned();
        let workspace_pointer = workspace.as_ptr();
        let request = SearchRequest::from(SearchArgs {
            query: Some(query),
            term: vec![term],
            limit: 10,
            provider: None,
            history_source: None,
            provider_key: None,
            source_id: None,
            source_format: None,
            source_roots: Vec::new(),
            source_groups: Vec::new(),
            workspace: Some(workspace),
            since: None,
            primary_only: false,
            content_scope: Some(ContentScopeArg::All),
            event_type: None,
            file: Some(PathBuf::from("src/lib.rs")),
            session: None,
            exclude_sessions: Vec::new(),
            events: false,
            backend: Some(SearchBackendArg::Lexical),
            semantic_weight: 0.35,
            refresh: RefreshMode::Off,
            include_current_session: false,
            format: JsonOutputFormat::Json,
            verbose: false,
        });

        let execution = ctx_history_read_application::SearchRequest::from(request);
        assert_eq!(execution.query.as_ptr(), query_pointer);
        assert_eq!(execution.terms[0].as_ptr(), term_pointer);
        assert_eq!(
            execution.workspace.as_deref().unwrap().as_ptr(),
            workspace_pointer
        );
        assert_eq!(
            execution.backend,
            Some(ctx_history_read_application::SearchBackend::Lexical)
        );
    }
}

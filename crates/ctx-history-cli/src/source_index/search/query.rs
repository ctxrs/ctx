#[cfg(test)]
use anyhow::Result;

use crate::{config, SearchBackend as HistorySearchBackend, SearchContentScope, SearchRequest};

#[cfg(test)]
use super::semantic_error_into_anyhow;
use super::semantic_port::{SemanticAvailability, SemanticReason};

pub(super) use ctx_history_read_application::unsupported_semantic_scope;
pub use ctx_history_read_application::{
    NormalizedSearchQuery, SearchBackend, SearchPolicy, SearchRequest as SourceSearchRequest,
};

impl From<SearchRequest> for SourceSearchRequest {
    fn from(args: SearchRequest) -> Self {
        Self {
            query: args.query.unwrap_or_default(),
            terms: args.terms,
            limit: args.limit,
            provider: args.provider.map(crate::HistoryProvider::capture_provider),
            history_source: args.history_source,
            provider_key: args.provider_key,
            source_id: args.source_id,
            source_format: args.source_format,
            source_roots: args.source_roots,
            source_groups: args.source_groups,
            workspace: args.workspace,
            since: args.since,
            primary_only: args.primary_only,
            content_scope: match args.content_scope {
                SearchContentScope::All => ctx_history_index::SearchContentScope::All,
                SearchContentScope::Transcript => ctx_history_index::SearchContentScope::Transcript,
                SearchContentScope::Calls => ctx_history_index::SearchContentScope::Calls,
                SearchContentScope::Outputs => ctx_history_index::SearchContentScope::Outputs,
            },
            event_type: args.event_type,
            file: args.file,
            events: args.events || args.session.is_some(),
            session: args.session,
            exclude_sessions: args.exclude_sessions,
            include_current_session: args.include_current_session,
            backend: args.backend.map(|backend| match backend {
                HistorySearchBackend::Hybrid => SearchBackend::Hybrid,
                HistorySearchBackend::Lexical => SearchBackend::Lexical,
                HistorySearchBackend::Semantic => SearchBackend::Semantic,
            }),
            semantic_weight: args.semantic_weight,
        }
    }
}

pub(in crate::source_index) fn source_search_policy(
    config: &config::AppConfig,
    foreground_semantic: bool,
) -> SearchPolicy {
    let semantic_enabled = config.semantic_search_enabled();
    let semantic = if !semantic_enabled {
        SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled)
    } else if !config.semantic_executor_supported() {
        SemanticAvailability::Unavailable(SemanticReason::PlatformUnsupported)
    } else if !config.daemon.enabled && !foreground_semantic {
        SemanticAvailability::Unavailable(SemanticReason::ExecutionUnavailable)
    } else {
        SemanticAvailability::Available
    };
    SearchPolicy {
        default_backend: if semantic_enabled {
            SearchBackend::Hybrid
        } else {
            SearchBackend::Lexical
        },
        semantic,
    }
}

#[cfg(test)]
pub(in crate::source_index) fn resolve_source_search_backend(
    request: &SourceSearchRequest,
    config: &config::AppConfig,
) -> Result<SearchBackend> {
    ctx_history_read_application::resolve_search_backend(
        request,
        source_search_policy(config, false),
    )
    .map_err(semantic_error_into_anyhow)
}

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_index::{
    AgentScope, EventSearchFilters, ExcludedSessionTree, SearchContentScope, VerifiedIndex,
    LEXICAL_QUERY_LIMITS,
};

use crate::{
    cli::ContentScopeArg,
    config,
    search_filters::{
        normalize_source_identity_filters, parse_since_filter, SourceIdentityFilterArgs,
        SourceIdentityFilters,
    },
    semantic::{semantic_query_service_supported, SemanticNotReady},
    transcript::shell_quote_arg,
    RefreshArg, SearchArgs, SearchBackendArg,
};

use super::super::{compact_ref::CompactRefResolver, shared::resolve_session_with_refs};

const LEGACY_ACTIVE_SESSION_PROVIDER_ENV: &str = "CODEX_THREAD_ID";
const LEGACY_ACTIVE_SESSION_PROVIDER: CaptureProvider = CaptureProvider::Codex;

#[derive(Debug, Clone)]
pub(crate) struct SourceSearchRequest {
    pub(crate) query: String,
    pub(crate) terms: Vec<String>,
    pub(crate) limit: usize,
    pub(crate) provider: Option<CaptureProvider>,
    pub(crate) history_source: Option<String>,
    pub(crate) provider_key: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_format: Option<String>,
    pub(crate) workspace: Option<String>,
    pub(crate) since: Option<String>,
    pub(crate) primary_only: bool,
    pub(crate) include_subagents: bool,
    pub(crate) content_scope: SearchContentScope,
    pub(crate) event_type: Option<String>,
    pub(crate) file: Option<PathBuf>,
    pub(crate) session: Option<String>,
    pub(crate) events: bool,
    pub(crate) include_current_session: bool,
    pub(crate) backend: Option<SearchBackendArg>,
    pub(crate) semantic_weight: f32,
    pub(crate) semantic_enabled: bool,
    pub(crate) semantic_daemon_enabled: bool,
    pub(crate) refresh: RefreshArg,
}

impl From<&SearchArgs> for SourceSearchRequest {
    fn from(args: &SearchArgs) -> Self {
        Self {
            query: args.query.clone().unwrap_or_default(),
            terms: args.term.clone(),
            limit: args.limit,
            provider: args.provider.map(|provider| provider.capture_provider()),
            history_source: args.history_source.clone(),
            provider_key: args.provider_key.clone(),
            source_id: args.source_id.clone(),
            source_format: args.source_format.clone(),
            workspace: args.workspace.clone(),
            since: args.since.clone(),
            primary_only: args.primary_only,
            include_subagents: args.include_subagents,
            content_scope: match args.content_scope.unwrap_or(ContentScopeArg::All) {
                ContentScopeArg::All => SearchContentScope::All,
                ContentScopeArg::Transcript => SearchContentScope::Transcript,
                ContentScopeArg::Calls => SearchContentScope::Calls,
                ContentScopeArg::Outputs => SearchContentScope::Outputs,
            },
            event_type: args.event_type.clone(),
            file: args.file.clone(),
            session: args.session.clone(),
            events: args.events || args.session.is_some(),
            include_current_session: args.include_current_session,
            backend: args.backend,
            semantic_weight: args.semantic_weight,
            semantic_enabled: false,
            semantic_daemon_enabled: false,
            refresh: args.refresh,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::commands::source_index) struct NormalizedSearchQuery {
    positional: Option<String>,
    terms: Vec<String>,
    alternatives: Vec<String>,
    display: String,
}

impl NormalizedSearchQuery {
    pub(in crate::commands::source_index) fn from_request(request: &SourceSearchRequest) -> Self {
        let positional = normalized_query_alternative(&request.query);
        let terms = request
            .terms
            .iter()
            .filter_map(|term| normalized_query_alternative(term))
            .collect::<Vec<_>>();
        let alternatives = positional
            .iter()
            .chain(terms.iter())
            .cloned()
            .collect::<Vec<_>>();
        let display = alternatives.join(" OR ");
        Self {
            positional,
            terms,
            alternatives,
            display,
        }
    }

    pub(in crate::commands::source_index) fn is_empty(&self) -> bool {
        self.alternatives.is_empty()
    }

    pub(in crate::commands::source_index) fn texts(&self) -> Vec<&str> {
        self.alternatives.iter().map(String::as_str).collect()
    }

    pub(in crate::commands::source_index) fn display(&self) -> &str {
        &self.display
    }

    pub(in crate::commands::source_index) fn shell_arguments(&self) -> String {
        let mut arguments = Vec::with_capacity(self.alternatives.len().saturating_mul(2));
        if let Some(positional) = self.positional.as_deref() {
            arguments.push(shell_quote_arg(positional));
        }
        for term in &self.terms {
            arguments.push(format!("--term={}", shell_quote_arg(term)));
        }
        arguments.join(" ")
    }
}

fn normalized_query_alternative(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_owned())
}

pub(super) fn validate_search_request(request: &SourceSearchRequest) -> Result<()> {
    validate_lexical_query_limits(request)?;
    if request
        .workspace
        .as_deref()
        .is_some_and(|workspace| workspace.trim().is_empty())
    {
        return Err(anyhow!("query filter workspace is empty"));
    }
    if request
        .file
        .as_ref()
        .is_some_and(|file| file.to_str().is_some_and(|file| file.trim().is_empty()))
    {
        return Err(anyhow!("query filter file is empty"));
    }
    let source_identity = normalized_source_identity_filters(request)?;
    if !source_identity.is_empty()
        && request
            .provider
            .is_some_and(|provider| provider != CaptureProvider::Custom)
    {
        return Err(anyhow!(
            "custom history source filters can only be combined with --provider custom"
        ));
    }
    let has_query = !NormalizedSearchQuery::from_request(request).is_empty();
    if !has_query && request.file.is_none() {
        return Err(anyhow!("source-backed search needs a non-empty text query"));
    }
    if !has_query
        && request
            .backend
            .is_some_and(|backend| backend != SearchBackendArg::Lexical)
    {
        return Err(anyhow!(
            "semantic and hybrid search need a non-empty text query"
        ));
    }
    Ok(())
}

pub(super) fn normalize_search_request(request: &mut SourceSearchRequest) -> Result<()> {
    validate_lexical_query_limits(request)?;
    if request.workspace.is_some() {
        request.workspace = normalized_optional_text(request.workspace.as_deref())
            .map(Some)
            .ok_or_else(|| anyhow!("query filter workspace is empty"))?;
    }
    if let Some(file) = request.file.as_ref().and_then(|file| file.to_str()) {
        let file = normalized_optional_text(Some(file))
            .ok_or_else(|| anyhow!("query filter file is empty"))?;
        request.file = Some(PathBuf::from(file));
    }
    Ok(())
}

fn validate_lexical_query_limits(request: &SourceSearchRequest) -> Result<()> {
    let positional = (!request.query.is_empty()).then_some(request.query.as_str());
    let alternatives = positional
        .into_iter()
        .chain(request.terms.iter().map(String::as_str));
    LEXICAL_QUERY_LIMITS.validate_texts(alternatives)?;
    Ok(())
}

fn normalized_source_identity_filters(
    request: &SourceSearchRequest,
) -> Result<SourceIdentityFilters> {
    normalize_source_identity_filters(SourceIdentityFilterArgs {
        history_source: request.history_source.clone(),
        provider_key: request.provider_key.clone(),
        source_id: request.source_id.clone(),
        source_format: request.source_format.clone(),
    })
}

pub(in crate::commands::source_index) fn resolve_source_search_backend(
    request: &SourceSearchRequest,
    config: &config::AppConfig,
) -> Result<SearchBackendArg> {
    if request.backend.is_none()
        && NormalizedSearchQuery::from_request(request).is_empty()
        && request.file.is_some()
    {
        return Ok(SearchBackendArg::Lexical);
    }
    super::validate_explicit_semantic_scope(request)?;
    let semantic_enabled = config.semantic_search_enabled();
    match request.backend {
        Some(SearchBackendArg::Semantic) if !semantic_enabled => Err(anyhow::Error::new(
            SemanticNotReady::new(
                "semantic_disabled",
                "semantic search is disabled. Set [search] semantic = true in ctx config to enable local semantic search",
            ),
        )),
        Some(SearchBackendArg::Semantic) if !semantic_query_service_supported() => {
            Err(anyhow::Error::new(SemanticNotReady::new(
                "semantic_unsupported",
                "local semantic search is not supported on this platform yet. Set [search] semantic = false or use --backend lexical",
            )))
        }
        Some(SearchBackendArg::Semantic) if !config.daemon.enabled => Err(anyhow::Error::new(
            SemanticNotReady::new(
                "semantic_daemon_disabled",
                "local semantic search requires the ctx daemon. Set [daemon] enabled = true, set [search] semantic = false, or use --backend lexical",
            ),
        )),
        Some(value) => Ok(value),
        None if semantic_enabled => Ok(SearchBackendArg::Hybrid),
        None => Ok(SearchBackendArg::Lexical),
    }
}

pub(super) fn unsupported_semantic_scope(
    request: &SourceSearchRequest,
) -> Option<SemanticNotReady> {
    let content_scope = match request.content_scope {
        SearchContentScope::Calls => Some("calls"),
        SearchContentScope::Outputs => Some("outputs"),
        SearchContentScope::All | SearchContentScope::Transcript => None,
    };
    if let Some(content_scope) = content_scope {
        return Some(SemanticNotReady::new(
            "semantic_content_scope_unsupported",
            format!(
                "semantic retrieval does not support content scope '{content_scope}'; use --backend lexical or choose --content-scope all|transcript"
            ),
        ));
    }

    let event_type = request
        .event_type
        .as_deref()
        .and_then(|value| value.parse::<EventType>().ok())
        .filter(|event_type| *event_type != EventType::Message)?;
    Some(SemanticNotReady::new(
        "semantic_event_type_unsupported",
        format!(
            "semantic retrieval does not support event type '{}'; use --backend lexical or remove --event-type",
            event_type.as_str()
        ),
    ))
}

#[cfg(test)]
pub(in crate::commands::source_index) fn index_search_filters(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
) -> Result<EventSearchFilters> {
    let references = CompactRefResolver::new(index, None);
    index_search_filters_with_refs(request, index, &references)
}

pub(in crate::commands::source_index) fn index_search_filters_with_refs(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
    references: &CompactRefResolver<'_>,
) -> Result<EventSearchFilters> {
    let source_identity = normalized_source_identity_filters(request)?;
    let session_id = request
        .session
        .as_deref()
        .map(|id| {
            resolve_session_with_refs(references, id).map(|session| session.session_id.as_uuid())
        })
        .transpose()?;
    let event_type = request
        .event_type
        .as_deref()
        .map(|value| {
            value
                .parse::<EventType>()
                .map(|event_type| event_type.as_str().to_owned())
                .map_err(|error| anyhow!("{error}"))
        })
        .transpose()?;
    let since_unix_ms = request
        .since
        .as_deref()
        .map(parse_since_filter)
        .transpose()?
        .map(|since| since.timestamp_millis());
    let exclude_session_tree = (!request.include_current_session && session_id.is_none())
        .then(|| std::env::var(LEGACY_ACTIVE_SESSION_PROVIDER_ENV).ok())
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(|provider_session_id| excluded_active_session_tree(index, provider_session_id))
        .transpose()?;
    Ok(EventSearchFilters {
        session_id,
        provider: request
            .provider
            .or_else(|| (!source_identity.is_empty()).then_some(CaptureProvider::Custom))
            .map(|provider| provider.as_str().to_owned()),
        history_source: source_identity.history_source,
        provider_key: source_identity.provider_key,
        source_id: source_identity.source_id,
        source_format: source_identity.source_format,
        workspace: normalized_optional_text(request.workspace.as_deref()),
        since_unix_ms,
        content_scope: request.content_scope,
        event_type,
        agent_scope: if request.primary_only || !request.include_subagents {
            AgentScope::Primary
        } else {
            AgentScope::All
        },
        file: request
            .file
            .as_ref()
            .and_then(|path| normalized_optional_text(Some(&path.display().to_string()))),
        exclude_session_tree,
        ..EventSearchFilters::default()
    })
}

fn normalized_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn excluded_active_session_tree(
    index: &VerifiedIndex,
    provider_session_id: String,
) -> Result<ExcludedSessionTree> {
    let sessions = index.sessions_by_provider_session_id(
        &provider_session_id,
        Some(LEGACY_ACTIVE_SESSION_PROVIDER.as_str()),
    )?;
    let session_id = match sessions.as_slice() {
        [session] => Some(session.root_session_id.as_uuid()),
        [first, second] if first.root_session_id == second.root_session_id => {
            Some(first.root_session_id.as_uuid())
        }
        _ => None,
    };
    Ok(ExcludedSessionTree {
        provider: LEGACY_ACTIVE_SESSION_PROVIDER.as_str().to_owned(),
        provider_session_id,
        session_id,
    })
}

use std::{fmt, path::PathBuf, str::FromStr};

use anyhow::{anyhow, Result};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_index_query::{SearchContentScope, LEXICAL_QUERY_LIMITS};

use super::{
    active_session::{normalize_manual_session_exclusions, validate_manual_session_exclusions},
    normalized_optional_text,
};
use crate::{
    normalize_source_identity_filters, HistorySemanticError, SemanticAvailability, SemanticReason,
    SourceIdentityFilterArgs, SourceIdentityFilters,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBackend {
    Hybrid,
    Lexical,
    Semantic,
}

impl SearchBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
        }
    }
}

impl fmt::Display for SearchBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SearchBackend {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "hybrid" => Ok(Self::Hybrid),
            "lexical" => Ok(Self::Lexical),
            "semantic" => Ok(Self::Semantic),
            other => Err(format!(
                "invalid search backend {other:?}; expected hybrid, lexical, or semantic"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchRequest {
    pub query: String,
    pub terms: Vec<String>,
    pub limit: usize,
    pub provider: Option<CaptureProvider>,
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
    pub include_current_session: bool,
    pub backend: Option<SearchBackend>,
    pub semantic_weight: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSessionExclusion {
    pub provider: String,
    pub provider_session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPolicy {
    pub default_backend: SearchBackend,
    pub semantic: SemanticAvailability,
}

impl SearchPolicy {
    pub const fn lexical_only(reason: SemanticReason) -> Self {
        Self {
            default_backend: SearchBackend::Lexical,
            semantic: SemanticAvailability::Unavailable(reason),
        }
    }

    pub const fn semantic_available() -> Self {
        Self {
            default_backend: SearchBackend::Hybrid,
            semantic: SemanticAvailability::Available,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedSearchQuery {
    positional: Option<String>,
    terms: Vec<String>,
    alternatives: Vec<String>,
    display: String,
}

impl NormalizedSearchQuery {
    pub fn from_request(request: &SearchRequest) -> Self {
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

    pub fn is_empty(&self) -> bool {
        self.alternatives.is_empty()
    }

    pub fn texts(&self) -> Vec<&str> {
        self.alternatives.iter().map(String::as_str).collect()
    }

    pub fn display(&self) -> &str {
        &self.display
    }

    pub fn positional(&self) -> Option<&str> {
        self.positional.as_deref()
    }

    pub fn terms(&self) -> &[String] {
        &self.terms
    }
}

fn normalized_query_alternative(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

pub fn validate_search_request(request: &SearchRequest) -> Result<()> {
    validate_lexical_query_limits(request)?;
    validate_manual_session_exclusions(request)?;
    validate_provider_root_selectors(&request.source_roots, "source root")?;
    validate_provider_root_selectors(&request.source_groups, "source group")?;
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
    let source_identity = normalized_request_source_identity_filters(request)?;
    if !source_identity.is_empty()
        && request
            .provider
            .is_some_and(|provider| provider != CaptureProvider::Custom)
    {
        return Err(crate::SourceIdentityFilterError::CustomProviderRequired.into());
    }
    let has_query = !NormalizedSearchQuery::from_request(request).is_empty();
    if !has_query && request.file.is_none() {
        return Err(anyhow!("source-backed search needs a non-empty text query"));
    }
    if !has_query
        && request
            .backend
            .is_some_and(|backend| backend != SearchBackend::Lexical)
    {
        return Err(anyhow!(
            "semantic and hybrid search need a non-empty text query"
        ));
    }
    Ok(())
}

pub fn normalize_search_request(request: &mut SearchRequest) -> Result<()> {
    validate_lexical_query_limits(request)?;
    normalize_manual_session_exclusions(request)?;
    normalize_provider_root_selectors(&mut request.source_roots, "source root")?;
    normalize_provider_root_selectors(&mut request.source_groups, "source group")?;
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

fn normalize_provider_root_selectors(values: &mut Vec<String>, kind: &str) -> Result<()> {
    for value in values.iter_mut() {
        *value = value.trim().to_owned();
    }
    values.sort();
    values.dedup();
    validate_provider_root_selectors(values, kind)
}

fn validate_provider_root_selectors(values: &[String], kind: &str) -> Result<()> {
    if values.len() > 64 {
        return Err(anyhow!("{kind} selectors exceed the maximum of 64"));
    }
    if values.iter().any(|value| {
        value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }) {
        return Err(anyhow!(
            "invalid {kind} selector; expected 1..=64 ASCII letters, digits, hyphens, or underscores"
        ));
    }
    Ok(())
}

fn validate_lexical_query_limits(request: &SearchRequest) -> Result<()> {
    let positional = (!request.query.is_empty()).then_some(request.query.as_str());
    LEXICAL_QUERY_LIMITS.validate_texts(
        positional
            .into_iter()
            .chain(request.terms.iter().map(String::as_str)),
    )?;
    Ok(())
}

pub(super) fn normalized_request_source_identity_filters(
    request: &SearchRequest,
) -> Result<SourceIdentityFilters> {
    normalize_source_identity_filters(SourceIdentityFilterArgs {
        history_source: request.history_source.clone(),
        provider_key: request.provider_key.clone(),
        source_id: request.source_id.clone(),
        source_format: request.source_format.clone(),
    })
}

pub fn resolve_search_backend(
    request: &SearchRequest,
    policy: SearchPolicy,
) -> std::result::Result<SearchBackend, HistorySemanticError> {
    if request.backend.is_none()
        && NormalizedSearchQuery::from_request(request).is_empty()
        && request.file.is_some()
    {
        return Ok(SearchBackend::Lexical);
    }
    if request.backend == Some(SearchBackend::Semantic) {
        if let Some(not_ready) = unsupported_semantic_scope(request) {
            return Err(not_ready);
        }
    }
    match request.backend {
        Some(SearchBackend::Semantic)
            if matches!(policy.semantic, SemanticAvailability::Unavailable(_)) =>
        {
            let SemanticAvailability::Unavailable(reason) = policy.semantic else {
                unreachable!("guard requires unavailable semantic policy")
            };
            Err(unavailable_semantic_error(reason))
        }
        Some(value) => Ok(value),
        None => Ok(policy.default_backend),
    }
}

pub fn unsupported_semantic_scope(request: &SearchRequest) -> Option<HistorySemanticError> {
    let content_scope = match request.content_scope {
        SearchContentScope::Calls => Some("calls"),
        SearchContentScope::Outputs => Some("outputs"),
        SearchContentScope::All | SearchContentScope::Transcript => None,
    };
    if let Some(content_scope) = content_scope {
        return Some(HistorySemanticError::not_ready(
            SemanticReason::ContentScopeUnsupported,
            format!("semantic retrieval does not support content scope '{content_scope}'"),
            false,
        ));
    }

    let event_type = request
        .event_type
        .as_deref()
        .and_then(|value| value.parse::<EventType>().ok())
        .filter(|event_type| *event_type != EventType::Message)?;
    Some(HistorySemanticError::not_ready(
        SemanticReason::EventTypeUnsupported,
        format!(
            "semantic retrieval does not support event type '{}'",
            event_type.as_str()
        ),
        false,
    ))
}

pub(super) fn unavailable_semantic_error(reason: SemanticReason) -> HistorySemanticError {
    let detail = match reason {
        SemanticReason::PolicyDisabled => "semantic retrieval is disabled by policy",
        SemanticReason::PlatformUnsupported => {
            "semantic retrieval is unavailable for this execution capability"
        }
        SemanticReason::ExecutionUnavailable => {
            "semantic retrieval execution is unavailable by policy"
        }
        _ => "semantic retrieval is unavailable",
    };
    HistorySemanticError::not_ready(reason, detail, false)
}

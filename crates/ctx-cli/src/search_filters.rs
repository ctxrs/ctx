use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{anyhow, Context, Result};
use chrono::{Duration, Utc};

use ctx_history_core::{utc_now, CaptureProvider, EventType};
use ctx_history_store::Store;

use crate::history_source_plugins::HistorySourcePluginSource;
use crate::provider_args::ProviderArg;
use crate::transcript::{resolve_session_id, shell_quote_arg};
use crate::SearchArgs;

pub(crate) struct SearchFilterInput {
    pub(crate) session: Option<String>,
    pub(crate) provider: Option<ProviderArg>,
    pub(crate) source_identity: SourceIdentityFilterArgs,
    pub(crate) workspace: Option<String>,
    pub(crate) since: Option<String>,
    pub(crate) primary_only: bool,
    pub(crate) include_subagents: bool,
    pub(crate) event_type: Option<String>,
    pub(crate) file: Option<PathBuf>,
    pub(crate) include_current_session: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceIdentityFilterArgs {
    pub(crate) history_source: Option<String>,
    pub(crate) provider_key: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_format: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceIdentityFilters {
    history_source: Option<String>,
    provider_key: Option<String>,
    source_id: Option<String>,
    source_format: Option<String>,
}

impl SourceIdentityFilters {
    pub(crate) fn is_empty(&self) -> bool {
        self.history_source.is_none()
            && self.provider_key.is_none()
            && self.source_id.is_none()
            && self.source_format.is_none()
    }

    pub(crate) fn matches_plugin_source(&self, source: &HistorySourcePluginSource) -> bool {
        if let Some(selector) = &self.history_source {
            if !source.matches_selector(selector) {
                return false;
            }
        }
        if let Some(provider_key) = &self.provider_key {
            if source.provider_key != *provider_key {
                return false;
            }
        }
        if let Some(source_id) = &self.source_id {
            if source.source_id != *source_id {
                return false;
            }
        }
        if let Some(source_format) = &self.source_format {
            if source.source_format != *source_format {
                return false;
            }
        }
        true
    }
}

impl From<&SearchArgs> for SourceIdentityFilterArgs {
    fn from(args: &SearchArgs) -> Self {
        Self {
            history_source: args.history_source.clone(),
            provider_key: args.provider_key.clone(),
            source_id: args.source_id.clone(),
            source_format: args.source_format.clone(),
        }
    }
}

pub(crate) struct SearchIntentInput<'a> {
    pub(crate) query: Option<&'a str>,
    pub(crate) terms: &'a [String],
    pub(crate) file: Option<&'a Path>,
}

pub(crate) fn search_has_intent(input: SearchIntentInput<'_>) -> bool {
    input.query.is_some_and(has_search_token)
        || input.terms.iter().any(|term| has_search_token(term))
        || input
            .file
            .and_then(|path| path.to_str())
            .is_some_and(|file| !file.trim().is_empty())
}

pub(crate) fn has_search_token(value: &str) -> bool {
    value.split_whitespace().any(|term| {
        term.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
            .chars()
            .any(char::is_alphanumeric)
    })
}

pub(crate) fn missing_search_intent_error() -> anyhow::Error {
    anyhow!(
        "search needs a query, --term, or --file\n\nTry:\n  ctx search \"failed migration\"\n  ctx search --term \"failed migration\" --term rollback\n  ctx search --file crates/foo/src/lib.rs"
    )
}

pub(crate) fn search_no_results_target(query: &str, terms: &[String]) -> String {
    if !query.trim().is_empty() {
        return shell_quote_arg(query);
    }
    let rendered_terms = terms
        .iter()
        .filter(|term| !term.trim().is_empty())
        .map(|term| format!("--term {}", shell_quote_arg(term)))
        .collect::<Vec<_>>();
    if rendered_terms.is_empty() {
        "search".to_owned()
    } else {
        rendered_terms.join(" ")
    }
}
pub(crate) fn search_filters(
    input: SearchFilterInput,
    store: Option<&Store>,
) -> Result<ctx_history_search::SearchFilters> {
    let source_identity = normalize_source_identity_filters(input.source_identity)?;
    if !source_identity.is_empty()
        && input
            .provider
            .is_some_and(|provider| !matches!(provider, ProviderArg::Custom))
    {
        return Err(anyhow!(
            "custom history source filters can only be combined with --provider custom"
        ));
    }
    let provider = if !source_identity.is_empty() {
        Some(CaptureProvider::Custom)
    } else {
        input.provider.map(ProviderArg::capture_provider)
    };
    let session = input
        .session
        .as_deref()
        .map(|value| {
            let store = store.ok_or_else(|| {
                anyhow!("session id prefix resolution requires an open ctx store")
            })?;
            resolve_session_id(store, value)
        })
        .transpose()?;
    let exclude_provider_session = if input.include_current_session || session.is_some() {
        None
    } else {
        current_codex_provider_session_filter(store)
    };
    Ok(ctx_history_search::SearchFilters {
        session,
        provider,
        history_source: source_identity.history_source,
        provider_key: source_identity.provider_key,
        source_id: source_identity.source_id,
        source_format: source_identity.source_format,
        repo: input
            .workspace
            .and_then(|s| if s.trim().is_empty() { None } else { Some(s) }),
        since: input.since.as_deref().map(parse_since_filter).transpose()?,
        primary_only: input.primary_only,
        include_subagents: input.include_subagents && !input.primary_only,
        event_type: input
            .event_type
            .as_deref()
            .map(EventType::from_str)
            .transpose()
            .map_err(|err| anyhow!("{err}"))?,
        file: input.file.and_then(|path| {
            let s = path.display().to_string();
            if s.trim().is_empty() {
                None
            } else {
                Some(s)
            }
        }),
        exclude_provider_session,
    })
}

pub(crate) fn normalize_source_identity_filters(
    input: SourceIdentityFilterArgs,
) -> Result<SourceIdentityFilters> {
    let history_source = normalize_source_identity_filter("history-source", input.history_source)?;
    if history_source
        .as_deref()
        .is_some_and(|value| !value.contains('/'))
    {
        return Err(anyhow!(
            "--history-source expects plugin/source or provider_key/source_id"
        ));
    }
    Ok(SourceIdentityFilters {
        history_source,
        provider_key: normalize_source_identity_filter("provider-key", input.provider_key)?,
        source_id: normalize_source_identity_filter("source-id", input.source_id)?,
        source_format: normalize_source_identity_filter("source-format", input.source_format)?,
    })
}

pub(crate) fn normalize_source_identity_filter(
    label: &str,
    value: Option<String>,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("--{label} cannot be empty"));
    }
    if value.chars().any(char::is_control) {
        return Err(anyhow!("--{label} cannot contain control characters"));
    }
    Ok(Some(value.to_owned()))
}

pub(crate) fn current_codex_provider_session_filter(
    store: Option<&Store>,
) -> Option<ctx_history_search::ProviderSessionFilter> {
    let provider_session_id = std::env::var("CODEX_THREAD_ID").ok()?;
    let provider_session_id = provider_session_id.trim();
    if provider_session_id.is_empty() {
        return None;
    }
    let session_id = store
        .and_then(|store| {
            store
                .session_by_external_session(CaptureProvider::Codex, provider_session_id)
                .ok()
                .flatten()
        })
        .map(|session| session.id);
    Some(ctx_history_search::ProviderSessionFilter {
        provider: CaptureProvider::Codex,
        provider_session_id: provider_session_id.to_owned(),
        session_id,
    })
}

pub(crate) fn parse_since_filter(value: &str) -> Result<chrono::DateTime<Utc>> {
    let trimmed = value.trim();
    if let Some(days) = trimmed.strip_suffix('d') {
        let days: i64 = days
            .parse()
            .with_context(|| format!("invalid --since day window: {value}"))?;
        let duration = Duration::try_days(days)
            .ok_or_else(|| anyhow!("invalid --since day window: {value}: value too large"))?;
        let since = utc_now()
            .checked_sub_signed(duration)
            .ok_or_else(|| anyhow!("invalid --since day window: {value}: value too large"))?;
        return Ok(since);
    }
    Ok(chrono::DateTime::parse_from_rfc3339(trimmed)
        .with_context(|| format!("invalid --since value: {value}"))?
        .with_timezone(&Utc))
}

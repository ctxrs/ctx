use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{Duration, Utc};

use ctx_history_core::utc_now;

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceIdentityFilterArgs {
    pub(crate) history_source: Option<String>,
    pub(crate) provider_key: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_format: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceIdentityFilters {
    pub(crate) history_source: Option<String>,
    pub(crate) provider_key: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_format: Option<String>,
}

impl SourceIdentityFilters {
    pub(crate) fn is_empty(&self) -> bool {
        self.history_source.is_none()
            && self.provider_key.is_none()
            && self.source_id.is_none()
            && self.source_format.is_none()
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

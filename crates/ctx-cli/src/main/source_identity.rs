#[allow(unused_imports)]
use super::*;

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

pub(crate) fn refresh_before_search(
    args: &SearchArgs,
    data_root: &Path,
) -> Result<SearchRefreshReport> {
    if args.refresh == RefreshArg::Off {
        return Ok(SearchRefreshReport::skipped(RefreshArg::Off, "skipped"));
    }
    let source_identity = normalize_source_identity_filters(SourceIdentityFilterArgs::from(args))?;
    if !source_identity.is_empty()
        && args
            .provider
            .is_some_and(|provider| !matches!(provider, ProviderArg::Custom))
    {
        return Err(anyhow!(
            "custom history source filters can only be combined with --provider custom"
        ));
    }
    let sources = if source_identity.is_empty() {
        search_refresh_sources(args.provider)
    } else {
        Vec::new()
    };
    let plugin_sources =
        match search_refresh_plugin_sources(data_root, args.provider, &source_identity) {
            Ok(sources) => sources,
            Err(err) if args.refresh == RefreshArg::Auto => {
                return Ok(SearchRefreshReport::failed(
                    RefreshArg::Auto,
                    sources.len(),
                    error_summary(&err),
                ));
            }
            Err(err) => return Err(err.context("search refresh failed")),
        };
    if sources.is_empty() && plugin_sources.is_empty() {
        if args.refresh == RefreshArg::Strict {
            return Err(anyhow!(
                "strict search refresh found no supported discovered native provider or enabled auto history-source plugin sources; rerun the search with --refresh off to use the existing index"
            ));
        }
        return Ok(SearchRefreshReport::skipped(args.refresh, "no_sources"));
    }
    let source_count = sources.len().saturating_add(plugin_sources.len());
    match refresh_sources_for_search(data_root, sources, plugin_sources, args.refresh, args.json) {
        Ok(totals) => Ok(SearchRefreshReport::completed(
            args.refresh,
            source_count,
            totals,
        )),
        Err(err) if args.refresh == RefreshArg::Auto => Ok(SearchRefreshReport::failed(
            RefreshArg::Auto,
            source_count,
            error_summary(&err),
        )),
        Err(err) => Err(err.context("search refresh failed")),
    }
}

pub(crate) fn search_refresh_plugin_sources(
    data_root: &Path,
    provider: Option<ProviderArg>,
    source_identity: &SourceIdentityFilters,
) -> Result<Vec<HistorySourcePluginSource>> {
    if !matches!(provider, None | Some(ProviderArg::Custom)) {
        return Ok(Vec::new());
    }
    Ok(discover_history_source_plugins(data_root, &[])?
        .into_iter()
        .filter(|source| {
            source.enabled
                && source.refresh == HistorySourcePluginRefresh::Auto
                && source_identity.matches_plugin_source(source)
        })
        .collect())
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

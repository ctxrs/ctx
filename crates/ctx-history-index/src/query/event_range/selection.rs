use super::*;

impl CoreEventRangeSelection {
    pub fn new<I, S>(
        since_unix_ms: i64,
        until_unix_ms: i64,
        providers: I,
    ) -> CoreEventRangeResult<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::with_filters(
            since_unix_ms,
            until_unix_ms,
            CoreEventRangeFilters {
                providers: providers.into_iter().map(Into::into).collect(),
                ..CoreEventRangeFilters::default()
            },
        )
    }

    pub fn with_filters(
        since_unix_ms: i64,
        until_unix_ms: i64,
        filters: CoreEventRangeFilters,
    ) -> CoreEventRangeResult<Self> {
        if since_unix_ms >= until_unix_ms {
            return Err(CoreEventRangeError::InvalidRange {
                since_unix_ms,
                until_unix_ms,
            });
        }
        Self::for_domain(
            CoreEventRangeDomain::Timestamped {
                since_unix_ms,
                until_unix_ms,
            },
            filters,
        )
    }

    pub fn all(filters: CoreEventRangeFilters) -> CoreEventRangeResult<Self> {
        Self::for_domain(CoreEventRangeDomain::All, filters)
    }

    fn for_domain(
        domain: CoreEventRangeDomain,
        mut filters: CoreEventRangeFilters,
    ) -> CoreEventRangeResult<Self> {
        canonicalize_providers(&mut filters.providers)?;
        canonicalize_optional_filter("history_source", &mut filters.history_source, false)?;
        canonicalize_optional_filter("provider_key", &mut filters.provider_key, false)?;
        canonicalize_optional_filter("source_id", &mut filters.source_id, false)?;
        canonicalize_optional_filter("source_format", &mut filters.source_format, false)?;
        canonicalize_optional_filter(
            "provider_session_id",
            &mut filters.provider_session_id,
            false,
        )?;
        canonicalize_optional_filter("branch", &mut filters.branch, false)?;
        canonicalize_optional_filter("workspace", &mut filters.workspace, true)?;
        canonicalize_optional_filter("event_type", &mut filters.event_type, false)?;
        canonicalize_optional_filter("role", &mut filters.role, false)?;
        canonicalize_optional_filter("agent_type", &mut filters.agent_type, false)?;
        canonicalize_optional_filter("file", &mut filters.file, true)?;
        let history_source_parts = filters
            .history_source
            .as_deref()
            .map(parse_history_source)
            .transpose()?;
        let digest = selection_digest(domain, &filters);
        Ok(Self {
            domain,
            filters,
            history_source_parts,
            digest,
        })
    }

    pub fn domain(&self) -> CoreEventRangeDomain {
        self.domain
    }

    pub fn since_unix_ms(&self) -> Option<i64> {
        match self.domain {
            CoreEventRangeDomain::All => None,
            CoreEventRangeDomain::Timestamped { since_unix_ms, .. } => Some(since_unix_ms),
        }
    }

    pub fn until_unix_ms(&self) -> Option<i64> {
        match self.domain {
            CoreEventRangeDomain::All => None,
            CoreEventRangeDomain::Timestamped { until_unix_ms, .. } => Some(until_unix_ms),
        }
    }

    pub fn filters(&self) -> &CoreEventRangeFilters {
        &self.filters
    }

    pub fn cursor_for(
        &self,
        generation_id: &str,
        event: &CoreEventRecord,
    ) -> CoreEventRangeResult<CoreEventRangeCursor> {
        #[cfg(test)]
        EVENT_RANGE_CURSOR_RECORD_RESERIALIZATIONS.set(
            EVENT_RANGE_CURSOR_RECORD_RESERIALIZATIONS
                .get()
                .saturating_add(1),
        );
        let encoded_core_bytes = event
            .core_record
            .encode_stored()
            .map_err(IndexError::from)?
            .len();
        let content_bytes = core_content_bytes(&event.core_record.content)?;
        let order = EventRangeOrderKey::for_core_record(
            &event.core_record,
            encoded_core_bytes,
            content_bytes,
        )?;
        if !self.accepts_order(order) || !self.accepts_record(event) {
            return Err(CoreEventRangeError::InvalidCursorCoordinate);
        }
        CoreEventRangeCursor::new(generation_id, self.digest, order)
    }

    pub(super) fn accepts_order(&self, order: EventRangeOrderKey) -> bool {
        match self.domain {
            CoreEventRangeDomain::All => true,
            CoreEventRangeDomain::Timestamped {
                since_unix_ms,
                until_unix_ms,
            } => order
                .occurred_at_unix_ms()
                .is_some_and(|timestamp| (since_unix_ms..until_unix_ms).contains(&timestamp)),
        }
    }

    pub(super) fn accepts_record(&self, event: &CoreEventRecord) -> bool {
        let filters = &self.filters;
        let record = &event.core_record;
        if !filters.providers.is_empty()
            && filters
                .providers
                .binary_search_by(|candidate| candidate.as_str().cmp(&event.provider))
                .is_err()
        {
            return false;
        }
        if filters
            .source_identity
            .is_some_and(|expected| record.source.identity().as_uuid() != expected)
            || filters
                .source_format
                .as_deref()
                .is_some_and(|expected| event.source_format != expected)
            || filters
                .provider_session_id
                .as_deref()
                .is_some_and(|expected| event.provider_session_id.as_deref() != Some(expected))
            || filters
                .session_id
                .is_some_and(|expected| event.session_id.as_uuid() != expected)
            || filters.parent_session_id.is_some_and(|expected| {
                event.parent_session_id.map(|id| id.as_uuid()) != Some(expected)
            })
            || filters
                .root_session_id
                .is_some_and(|expected| event.root_session_id.as_uuid() != expected)
            || filters
                .branch
                .as_deref()
                .is_some_and(|expected| event.branch.as_deref() != Some(expected))
            || filters
                .event_type
                .as_deref()
                .is_some_and(|expected| event.event_type != expected)
            || filters
                .role
                .as_deref()
                .is_some_and(|expected| event.role.as_deref() != Some(expected))
            || filters
                .agent_type
                .as_deref()
                .is_some_and(|expected| event.agent_type != expected)
        {
            return false;
        }
        if (filters.scope == CoreEventRangeScope::Primary && !event.is_primary)
            || (filters.scope == CoreEventRangeScope::Subagent && event.is_primary)
        {
            return false;
        }
        if filters.workspace.as_deref().is_some_and(|expected| {
            !event
                .workspace
                .as_deref()
                .into_iter()
                .chain(event.cwd.as_deref())
                .any(|value| value.to_lowercase().contains(expected))
        }) {
            return false;
        }
        if filters.file.as_deref().is_some_and(|expected| {
            !event
                .touched_files
                .iter()
                .any(|value| value.to_lowercase().contains(expected))
        }) {
            return false;
        }
        if filters.provider_key.is_some()
            || filters.source_id.is_some()
            || self.history_source_parts.is_some()
        {
            let Some((provider_key, source_id)) = custom_source_identity(&event.event) else {
                return false;
            };
            if filters
                .provider_key
                .as_deref()
                .is_some_and(|expected| provider_key != expected)
                || filters
                    .source_id
                    .as_deref()
                    .is_some_and(|expected| source_id != expected)
                || self.history_source_parts.as_ref().is_some_and(
                    |(expected_provider, expected_source)| {
                        provider_key != expected_provider || source_id != expected_source
                    },
                )
            {
                return false;
            }
        }
        true
    }
}

fn canonicalize_providers(providers: &mut Vec<String>) -> CoreEventRangeResult<()> {
    for provider in providers.iter_mut() {
        *provider = provider.trim().to_owned();
        if provider.is_empty() || provider.len() > MAX_PROVIDER_FILTER_BYTES {
            return Err(CoreEventRangeError::InvalidFilter { field: "provider" });
        }
    }
    providers.sort_unstable();
    providers.dedup();
    if providers.len() > MAX_EVENT_RANGE_PROVIDERS {
        return Err(CoreEventRangeError::InvalidFilter { field: "provider" });
    }
    Ok(())
}

fn canonicalize_optional_filter(
    field: &'static str,
    value: &mut Option<String>,
    lowercase: bool,
) -> CoreEventRangeResult<()> {
    let Some(current) = value.take() else {
        return Ok(());
    };
    let mut current = current.trim().to_owned();
    if current.is_empty() || current.len() > MAX_DOCUMENT_METADATA_BYTES {
        return Err(CoreEventRangeError::InvalidFilter { field });
    }
    if lowercase {
        current = current.to_lowercase();
    }
    *value = Some(current);
    Ok(())
}

fn parse_history_source(value: &str) -> CoreEventRangeResult<(String, String)> {
    let Some((provider, source)) = value.split_once('/') else {
        return Err(CoreEventRangeError::InvalidFilter {
            field: "history_source",
        });
    };
    if provider.is_empty() || source.is_empty() {
        return Err(CoreEventRangeError::InvalidFilter {
            field: "history_source",
        });
    }
    Ok((provider.to_owned(), source.to_owned()))
}

fn selection_digest(domain: CoreEventRangeDomain, filters: &CoreEventRangeFilters) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SELECTION_DOMAIN);
    match domain {
        CoreEventRangeDomain::All => digest.update([0]),
        CoreEventRangeDomain::Timestamped {
            since_unix_ms,
            until_unix_ms,
        } => {
            digest.update([1]);
            digest.update(since_unix_ms.to_be_bytes());
            digest.update(until_unix_ms.to_be_bytes());
        }
    }
    digest_strings(&mut digest, &filters.providers);
    let source_identity = filters.source_identity.map(|value| value.to_string());
    let session_id = filters.session_id.map(|value| value.to_string());
    let parent_session_id = filters.parent_session_id.map(|value| value.to_string());
    let root_session_id = filters.root_session_id.map(|value| value.to_string());
    digest_option(&mut digest, source_identity.as_deref());
    for value in [
        filters.history_source.as_deref(),
        filters.provider_key.as_deref(),
        filters.source_id.as_deref(),
        filters.source_format.as_deref(),
        filters.provider_session_id.as_deref(),
        session_id.as_deref(),
        parent_session_id.as_deref(),
        root_session_id.as_deref(),
        filters.branch.as_deref(),
        filters.workspace.as_deref(),
        filters.event_type.as_deref(),
        filters.role.as_deref(),
        filters.agent_type.as_deref(),
        filters.file.as_deref(),
    ] {
        digest_option(&mut digest, value);
    }
    digest.update([match filters.scope {
        CoreEventRangeScope::All => 0,
        CoreEventRangeScope::Primary => 1,
        CoreEventRangeScope::Subagent => 2,
    }]);
    digest.update([match filters.direction {
        CoreEventRangeDirection::Ascending => 0,
        CoreEventRangeDirection::Descending => 1,
    }]);
    digest.finalize().into()
}

fn digest_strings(digest: &mut Sha256, values: &[String]) {
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
}

fn digest_option(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        }
        None => digest.update([0]),
    }
}

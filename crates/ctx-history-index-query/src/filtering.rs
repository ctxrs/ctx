use super::*;

pub(super) const TRANSCRIPT_EVENT_TYPES: &[&str] = &["message", "summary"];
pub(super) const CALL_EVENT_TYPES: &[&str] = &["tool_call", "command_started"];
pub(super) const OUTPUT_EVENT_TYPES: &[&str] =
    &["tool_output", "command_output", "command_finished"];

/// One canonical disjunction of exact posting terms. Manual lexical execution
/// intersects these groups without constructing Tantivy queries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct CanonicalAnyOfTerms {
    pub(super) terms: Vec<Term>,
}

/// Backend adapter derived from the canonical `CompiledSearchFilter` for one
/// metered manual lexical execution.
#[derive(Debug, Clone)]
pub(super) struct LexicalFilterAdapter {
    pub(super) required: Vec<CanonicalAnyOfTerms>,
    pub(super) prohibited: Vec<CanonicalAnyOfTerms>,
    pub(super) since_unix_ms: Option<i64>,
    pub(super) workspace_substring: Option<AsciiFoldSubstring>,
    pub(super) file_substring: Option<AsciiFoldSubstring>,
    pub(super) match_none: bool,
}

/// Linear-time ASCII-case-insensitive substring matcher over exact UTF-8
/// bytes. Non-ASCII bytes remain exact, matching the public filter contract.
#[derive(Debug, Clone)]
pub(super) struct AsciiFoldSubstring {
    needle: Vec<u8>,
    failure: Vec<u32>,
}

impl AsciiFoldSubstring {
    fn new(needle: &str) -> Result<Self> {
        let needle = needle
            .as_bytes()
            .iter()
            .map(|byte| byte.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let mut failure = Vec::with_capacity(needle.len());
        failure.push(0);
        for index in 1..needle.len() {
            let mut matched =
                usize::try_from(failure[index - 1]).map_err(|_| IndexError::CountOverflow)?;
            while matched > 0 && needle[index] != needle[matched] {
                matched =
                    usize::try_from(failure[matched - 1]).map_err(|_| IndexError::CountOverflow)?;
            }
            if needle[index] == needle[matched] {
                matched = matched.checked_add(1).ok_or(IndexError::CountOverflow)?;
            }
            failure.push(u32::try_from(matched).map_err(|_| IndexError::CountOverflow)?);
        }
        Ok(Self { needle, failure })
    }

    pub(super) fn matches(&self, haystack: &[u8]) -> bool {
        self.matches_with_comparison_count(haystack).0
    }

    fn matches_with_comparison_count(&self, haystack: &[u8]) -> (bool, usize) {
        let mut matched = 0_usize;
        let mut comparisons = 0_usize;
        for byte in haystack {
            let byte = byte.to_ascii_lowercase();
            while matched > 0 {
                comparisons = comparisons.saturating_add(1);
                if byte == self.needle[matched] {
                    break;
                }
                matched = self.failure[matched - 1] as usize;
            }
            if matched == 0 {
                comparisons = comparisons.saturating_add(1);
                if byte != self.needle[0] {
                    continue;
                }
            }
            matched += 1;
            if matched == self.needle.len() {
                return (true, comparisons);
            }
        }
        (false, comparisons)
    }
}

impl CompiledSearchFilter {
    /// Reapplies the canonical logical filter to one final decoded winner.
    /// This is deliberately the same authority used to derive lexical and
    /// semantic backend adapters, so hydration cannot silently widen a query.
    pub fn matches_core(&self, record: &CoreEventRecord) -> Result<bool> {
        let filters = self.filters();
        let event = &record.event;
        let core = &record.core_record;

        if core.content.discovery_exclusion.is_some()
            || filters.allowed_source_keys.as_ref().is_some_and(|allowed| {
                !allowed
                    .iter()
                    .any(|candidate| candidate == &source_token(&event.source))
            })
            || filters
                .session_id
                .is_some_and(|expected| event.session_id.as_uuid() != expected)
            || filters
                .excluded_session_ids
                .iter()
                .chain(
                    filters
                        .exclude_session_tree
                        .iter()
                        .flat_map(|tree| tree.session_ids.iter()),
                )
                .any(|excluded| event.session_id.as_uuid() == *excluded)
            || filters.parent_session_id.is_some_and(|expected| {
                event.parent_session_id.map(StableEntityId::as_uuid) != Some(expected)
            })
            || filters.root_session_id.is_some_and(|expected| {
                event.root_session_id.map(StableEntityId::as_uuid) != Some(expected)
            })
            || filters
                .provider
                .as_deref()
                .is_some_and(|expected| event.provider != expected.trim())
            || filters
                .source_format
                .as_deref()
                .is_some_and(|expected| event.source_format != expected.trim())
            || filters
                .provider_session_id
                .as_deref()
                .is_some_and(|expected| {
                    event.provider_session_id.as_deref() != Some(expected.trim())
                })
            || filters
                .event_type
                .as_deref()
                .is_some_and(|expected| event.event_type != expected.trim())
            || content_scope_event_types(filters.content_scope)
                .is_some_and(|eligible| !eligible.contains(&event.event_type.as_str()))
            || filters
                .role
                .as_deref()
                .is_some_and(|expected| event.role.as_deref() != Some(expected.trim()))
            || filters.since_unix_ms.is_some_and(|since| {
                event
                    .occurred_at_unix_ms
                    .is_none_or(|occurred_at| occurred_at < since)
            })
            || match filters.agent_scope {
                SearchAgentScope::All => false,
                SearchAgentScope::Primary => event.agent_scope != Some(CoreAgentScope::Primary),
                SearchAgentScope::Subagent => event.agent_scope != Some(CoreAgentScope::Subagent),
            }
            || !filters.matches_source_identity(event)
        {
            return Ok(false);
        }

        let facts = core
            .content
            .activity
            .iter()
            .flat_map(|activity| activity.facts.iter());
        let facts = facts.collect::<Vec<_>>();
        if filters.branch.as_deref().is_some_and(|branch| {
            !facts
                .iter()
                .any(|fact| fact.kind == LiteralFactKind::Branch && fact.value == branch.trim())
        }) {
            return Ok(false);
        }
        for (needle, kinds) in [
            (
                filters.workspace.as_deref(),
                &[
                    LiteralFactKind::Workspace,
                    LiteralFactKind::SessionCwd,
                    LiteralFactKind::ToolWorkdir,
                    LiteralFactKind::Project,
                ][..],
            ),
            (filters.file.as_deref(), &[LiteralFactKind::File][..]),
        ] {
            let Some(needle) = needle else {
                continue;
            };
            let matcher = AsciiFoldSubstring::new(needle.trim())?;
            if !facts
                .iter()
                .any(|fact| kinds.contains(&fact.kind) && matcher.matches(fact.value.as_bytes()))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Validates every manually compiled filter before any empty-query, zero-limit,
/// or work-budget short circuit can return. Admission exhaustion must never
/// hide a caller input error that the compatibility APIs historically expose.
pub(super) fn validate_manual_filter_inputs(filters: &EventSearchFilters) -> Result<()> {
    filters.validate_content_scope()?;
    for (field, value) in [
        ("provider", filters.provider.as_deref()),
        ("source_format", filters.source_format.as_deref()),
        (
            "provider_session_id",
            filters.provider_session_id.as_deref(),
        ),
        ("branch", filters.branch.as_deref()),
        ("event_type", filters.event_type.as_deref()),
        ("role", filters.role.as_deref()),
        ("workspace", filters.workspace.as_deref()),
        ("file", filters.file.as_deref()),
    ] {
        if let Some(value) = value {
            validated_filter_text(field, value)?;
        }
    }
    filters.validate_source_identity_filters()
}

/// Compiles the generic filter contract into sorted, deduplicated exact term
/// groups. Every caller-controlled byte and every exact term is charged before
/// it is retained. `None` means the meter rejected the next operation.
pub(super) fn compile_lexical_filter_adapter(
    compiled: &CompiledSearchFilter,
    fields: Fields,
    meter: &mut LexicalWorkMeter,
) -> Result<Option<LexicalFilterAdapter>> {
    let filters = compiled.filters();
    let mut required = Vec::new();
    let mut prohibited = Vec::new();
    let mut match_none = false;

    if !push_u64_group(&mut required, fields.discovery_eligible, 1, meter) {
        return Ok(None);
    }

    if let Some(source_keys) = &filters.allowed_source_keys {
        if source_keys.is_empty() {
            match_none = true;
        } else {
            let mut terms = Vec::new();
            for source_key in source_keys {
                if !charge_filter_bytes(meter, source_key.len())
                    || !push_text_term(&mut terms, fields.source_key, source_key, meter)
                {
                    return Ok(None);
                }
            }
            push_group(&mut required, terms);
        }
    }

    if !push_optional_validated_text_group(
        &mut required,
        fields.provider,
        "provider",
        filters.provider.as_deref(),
        meter,
    )? || !push_optional_validated_text_group(
        &mut required,
        fields.source_format,
        "source_format",
        filters.source_format.as_deref(),
        meter,
    )? || !push_optional_validated_text_group(
        &mut required,
        fields.provider_session_id,
        "provider_session_id",
        filters.provider_session_id.as_deref(),
        meter,
    )? || !push_optional_uuid_group(&mut required, fields.session_id, filters.session_id, meter)
        || !push_optional_uuid_group(
            &mut required,
            fields.parent_session_id,
            filters.parent_session_id,
            meter,
        )
        || !push_optional_uuid_group(
            &mut required,
            fields.root_session_id,
            filters.root_session_id,
            meter,
        )
        || !push_optional_validated_text_group(
            &mut required,
            fields.fact_branch,
            "branch",
            filters.branch.as_deref(),
            meter,
        )?
        || !push_optional_validated_text_group(
            &mut required,
            fields.event_type,
            "event_type",
            filters.event_type.as_deref(),
            meter,
        )?
    {
        return Ok(None);
    }

    if let Some(event_types) = content_scope_event_types(filters.content_scope) {
        let mut terms = Vec::with_capacity(event_types.len());
        for event_type in event_types {
            if !push_text_term(&mut terms, fields.event_type, event_type, meter) {
                return Ok(None);
            }
        }
        push_group(&mut required, terms);
    }

    if !push_optional_validated_text_group(
        &mut required,
        fields.role,
        "role",
        filters.role.as_deref(),
        meter,
    )? {
        return Ok(None);
    }

    let workspace_substring =
        match validated_substring_filter("workspace", filters.workspace.as_deref(), meter)? {
            Some(Some(value)) => Some(value),
            Some(None) => None,
            None => return Ok(None),
        };
    let file_substring = match validated_substring_filter("file", filters.file.as_deref(), meter)? {
        Some(Some(value)) => Some(value),
        Some(None) => None,
        None => return Ok(None),
    };

    if filters.since_unix_ms.is_some() && !charge_filter_bytes(meter, size_of::<i64>()) {
        return Ok(None);
    }

    if filters.agent_scope != SearchAgentScope::All {
        if !charge_filter_bytes(meter, 1) {
            return Ok(None);
        }
        let expected = match filters.agent_scope {
            SearchAgentScope::All => unreachable!(),
            SearchAgentScope::Primary => CoreAgentScope::Primary,
            SearchAgentScope::Subagent => CoreAgentScope::Subagent,
        };
        let mut terms = Vec::with_capacity(1);
        if !push_text_term(&mut terms, fields.agent_scope, expected.as_str(), meter) {
            return Ok(None);
        }
        push_group(&mut required, terms);
    }

    let mut excluded_terms = Vec::new();
    for session_id in filters.excluded_session_ids.iter().copied().chain(
        filters
            .exclude_session_tree
            .iter()
            .flat_map(|tree| tree.session_ids.iter().copied()),
    ) {
        if !charge_filter_bytes(meter, size_of::<Uuid>())
            || !push_text_term(
                &mut excluded_terms,
                fields.session_id,
                &session_id.to_string(),
                meter,
            )
        {
            return Ok(None);
        }
    }
    if !excluded_terms.is_empty() {
        push_group(&mut prohibited, excluded_terms);
    }

    let history_source =
        validated_optional_filter_text("history_source", filters.history_source.as_deref(), meter)?;
    let provider_key =
        validated_optional_filter_text("provider_key", filters.provider_key.as_deref(), meter)?;
    let source_id =
        validated_optional_filter_text("source_id", filters.source_id.as_deref(), meter)?;
    if meter.exhausted() {
        return Ok(None);
    }
    if history_source.is_some() || provider_key.is_some() || source_id.is_some() {
        let mut custom = Vec::with_capacity(1);
        if !push_text_term(&mut custom, fields.provider, "custom", meter) {
            return Ok(None);
        }
        push_group(&mut required, custom);
    }
    if let Some(history_source) = history_source {
        if let Some((history_provider_key, history_source_id)) = history_source.split_once('/') {
            if !push_single_text_group(
                &mut required,
                fields.custom_provider_key,
                history_provider_key,
                meter,
            ) || !push_single_text_group(
                &mut required,
                fields.custom_source_id,
                history_source_id,
                meter,
            ) {
                return Ok(None);
            }
        } else {
            match_none = true;
        }
    }
    if let Some(provider_key) = provider_key {
        if !push_single_text_group(
            &mut required,
            fields.custom_provider_key,
            provider_key,
            meter,
        ) {
            return Ok(None);
        }
    }
    if let Some(source_id) = source_id {
        if !push_single_text_group(&mut required, fields.custom_source_id, source_id, meter) {
            return Ok(None);
        }
    }

    required.sort();
    required.dedup();
    prohibited.sort();
    prohibited.dedup();
    Ok(Some(LexicalFilterAdapter {
        required,
        prohibited,
        since_unix_ms: filters.since_unix_ms,
        workspace_substring,
        file_substring,
        match_none,
    }))
}

fn charge_filter_bytes(meter: &mut LexicalWorkMeter, bytes: usize) -> bool {
    let Ok(bytes) = u64::try_from(bytes) else {
        return meter.charge(LexicalWorkCounter::FilterInputBytes, u64::MAX, None, None);
    };
    meter.charge(LexicalWorkCounter::FilterInputBytes, bytes, None, None)
}

fn push_text_term(
    terms: &mut Vec<Term>,
    field: Field,
    value: &str,
    meter: &mut LexicalWorkMeter,
) -> bool {
    if !meter.charge(LexicalWorkCounter::ExactFilterTerms, 1, None, None) {
        return false;
    }
    terms.push(Term::from_field_text(field, value));
    true
}

fn push_group(groups: &mut Vec<CanonicalAnyOfTerms>, mut terms: Vec<Term>) {
    terms.sort();
    terms.dedup();
    groups.push(CanonicalAnyOfTerms { terms });
}

fn push_single_text_group(
    groups: &mut Vec<CanonicalAnyOfTerms>,
    field: Field,
    value: &str,
    meter: &mut LexicalWorkMeter,
) -> bool {
    let mut terms = Vec::with_capacity(1);
    if !push_text_term(&mut terms, field, value, meter) {
        return false;
    }
    push_group(groups, terms);
    true
}

fn push_u64_group(
    groups: &mut Vec<CanonicalAnyOfTerms>,
    field: Field,
    value: u64,
    meter: &mut LexicalWorkMeter,
) -> bool {
    if !meter.charge(LexicalWorkCounter::ExactFilterTerms, 1, None, None) {
        return false;
    }
    push_group(groups, vec![Term::from_field_u64(field, value)]);
    true
}

fn push_optional_validated_text_group(
    groups: &mut Vec<CanonicalAnyOfTerms>,
    field: Field,
    field_name: &'static str,
    value: Option<&str>,
    meter: &mut LexicalWorkMeter,
) -> Result<bool> {
    let Some(raw_value) = value else {
        return Ok(true);
    };
    let value = validated_filter_text(field_name, raw_value)?;
    if !charge_filter_bytes(meter, raw_value.len()) {
        return Ok(false);
    }
    Ok(push_single_text_group(groups, field, value, meter))
}

fn push_optional_uuid_group(
    groups: &mut Vec<CanonicalAnyOfTerms>,
    field: Field,
    value: Option<Uuid>,
    meter: &mut LexicalWorkMeter,
) -> bool {
    let Some(value) = value else {
        return true;
    };
    charge_filter_bytes(meter, size_of::<Uuid>())
        && push_single_text_group(groups, field, &value.to_string(), meter)
}

fn validated_optional_filter_text<'a>(
    field: &'static str,
    value: Option<&'a str>,
    meter: &mut LexicalWorkMeter,
) -> Result<Option<&'a str>> {
    let Some(raw_value) = value else {
        return Ok(None);
    };
    let value = validated_filter_text(field, raw_value)?;
    if !charge_filter_bytes(meter, raw_value.len()) {
        return Ok(None);
    }
    Ok(Some(value))
}

fn validated_substring_filter(
    field: &'static str,
    value: Option<&str>,
    meter: &mut LexicalWorkMeter,
) -> Result<Option<Option<AsciiFoldSubstring>>> {
    let Some(raw_value) = value else {
        return Ok(Some(None));
    };
    let value = validated_filter_text(field, raw_value)?;
    let retained_bytes = value
        .len()
        .checked_mul(size_of::<u32>() + 1)
        .and_then(|bytes| bytes.checked_add(raw_value.len()))
        .ok_or(IndexError::CountOverflow)?;
    if !charge_filter_bytes(meter, retained_bytes) {
        return Ok(None);
    }
    Ok(Some(Some(AsciiFoldSubstring::new(value)?)))
}

pub(super) fn filtered_event_query(
    body_query: Box<dyn Query>,
    source_identity_query: Option<Box<dyn Query>>,
    compiled: &CompiledSearchFilter,
    fields: Fields,
) -> Result<Box<dyn Query>> {
    let filters = compiled.filters();
    let mut clauses = vec![(Occur::Must, body_query)];
    add_filter_clause(
        &mut clauses,
        Box::new(TermQuery::new(
            Term::from_field_u64(fields.discovery_eligible, 1),
            IndexRecordOption::Basic,
        )),
    );
    if let Some(query) = source_identity_query {
        add_filter_clause(&mut clauses, query);
    }
    if let Some(source_keys) = &filters.allowed_source_keys {
        if source_keys.is_empty() {
            add_filter_clause(&mut clauses, Box::new(EmptyQuery));
        } else {
            add_filter_clause(
                &mut clauses,
                Box::new(TermSetQuery::new(
                    source_keys
                        .iter()
                        .map(|source_key| Term::from_field_text(fields.source_key, source_key))
                        .collect::<Vec<_>>(),
                )),
            );
        }
    }
    add_optional_text_filter(
        &mut clauses,
        fields.provider,
        "provider",
        filters.provider.as_deref(),
    )?;
    add_optional_text_filter(
        &mut clauses,
        fields.source_format,
        "source_format",
        filters.source_format.as_deref(),
    )?;
    add_optional_text_filter(
        &mut clauses,
        fields.provider_session_id,
        "provider_session_id",
        filters.provider_session_id.as_deref(),
    )?;
    add_optional_uuid_filter(&mut clauses, fields.session_id, filters.session_id);
    if !filters.excluded_session_ids.is_empty() {
        clauses.push((
            Occur::MustNot,
            Box::new(TermSetQuery::new(
                filters
                    .excluded_session_ids
                    .iter()
                    .map(|session_id| {
                        Term::from_field_text(fields.session_id, &session_id.to_string())
                    })
                    .collect::<Vec<_>>(),
            )),
        ));
    }
    add_optional_uuid_filter(
        &mut clauses,
        fields.parent_session_id,
        filters.parent_session_id,
    );
    add_optional_uuid_filter(
        &mut clauses,
        fields.root_session_id,
        filters.root_session_id,
    );
    add_optional_text_filter(
        &mut clauses,
        fields.fact_branch,
        "branch",
        filters.branch.as_deref(),
    )?;
    add_optional_text_filter(
        &mut clauses,
        fields.event_type,
        "event_type",
        filters.event_type.as_deref(),
    )?;
    if let Some(event_types) = content_scope_event_types(filters.content_scope) {
        add_filter_clause(
            &mut clauses,
            event_type_union_query(fields.event_type, event_types),
        );
    }
    add_optional_text_filter(&mut clauses, fields.role, "role", filters.role.as_deref())?;
    if let Some(workspace) = filters.workspace.as_deref() {
        add_filter_clause(
            &mut clauses,
            literal_fact_union_contains_query(
                [
                    fields.fact_workspace,
                    fields.fact_session_cwd,
                    fields.fact_tool_workdir,
                    fields.fact_project,
                ],
                "workspace",
                workspace,
            )?,
        );
    }
    if let Some(file) = filters.file.as_deref() {
        add_filter_clause(
            &mut clauses,
            Box::new(metadata_contains_query(fields.fact_file, "file", file)?),
        );
    }
    if let Some(since_unix_ms) = filters.since_unix_ms {
        add_filter_clause(
            &mut clauses,
            Box::new(RangeQuery::new(
                Bound::Included(Term::from_field_i64(
                    fields.occurred_at_unix_ms,
                    since_unix_ms,
                )),
                Bound::Unbounded,
            )),
        );
    }
    if filters.agent_scope != SearchAgentScope::All {
        let expected = match filters.agent_scope {
            SearchAgentScope::All => unreachable!(),
            SearchAgentScope::Primary => CoreAgentScope::Primary,
            SearchAgentScope::Subagent => CoreAgentScope::Subagent,
        };
        add_filter_clause(
            &mut clauses,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.agent_scope, expected.as_str()),
                IndexRecordOption::Basic,
            )),
        );
    }
    if let Some(excluded) = filters
        .exclude_session_tree
        .as_ref()
        .and_then(|excluded| excluded_session_tree_query(excluded, fields))
    {
        clauses.push((Occur::MustNot, excluded));
    }
    Ok(Box::new(BooleanQuery::new(clauses)))
}

pub(super) fn content_scope_event_types(
    scope: SearchContentScope,
) -> Option<&'static [&'static str]> {
    match scope {
        SearchContentScope::All => None,
        SearchContentScope::Transcript => Some(TRANSCRIPT_EVENT_TYPES),
        SearchContentScope::Calls => Some(CALL_EVENT_TYPES),
        SearchContentScope::Outputs => Some(OUTPUT_EVENT_TYPES),
    }
}

pub(super) fn event_type_union_query(
    field: tantivy::schema::Field,
    event_types: &[&str],
) -> Box<dyn Query> {
    Box::new(BooleanQuery::union(
        event_types
            .iter()
            .map(|event_type| {
                Box::new(TermQuery::new(
                    Term::from_field_text(field, event_type),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>
            })
            .collect(),
    ))
}

pub(super) fn custom_source_identity(event: &EventRecord) -> Option<(&str, &str)> {
    event.custom_source_identity()
}

pub(super) fn source_identity_values_match(
    filters: &EventSearchFilters,
    provider_key: &str,
    source_id: &str,
) -> bool {
    if filters.history_source.as_deref().is_some_and(|selector| {
        selector
            .trim()
            .split_once('/')
            .is_none_or(|(provider, source)| provider != provider_key || source != source_id)
    }) {
        return false;
    }
    if filters
        .provider_key
        .as_deref()
        .is_some_and(|expected| expected.trim() != provider_key)
    {
        return false;
    }
    filters
        .source_id
        .as_deref()
        .is_none_or(|expected| expected.trim() == source_id)
}

pub(super) fn add_optional_text_filter(
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
    field: tantivy::schema::Field,
    field_name: &'static str,
    value: Option<&str>,
) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let value = validated_filter_text(field_name, value)?;
    add_filter_clause(
        clauses,
        Box::new(TermQuery::new(
            Term::from_field_text(field, value),
            IndexRecordOption::Basic,
        )),
    );
    Ok(())
}

pub(super) fn add_optional_uuid_filter(
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
    field: tantivy::schema::Field,
    value: Option<Uuid>,
) {
    if let Some(value) = value {
        add_filter_clause(
            clauses,
            Box::new(TermQuery::new(
                Term::from_field_text(field, &value.to_string()),
                IndexRecordOption::Basic,
            )),
        );
    }
}

pub(super) fn add_filter_clause(
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
    filter: Box<dyn Query>,
) {
    clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(filter, 0.0))));
}

pub(super) fn metadata_contains_query(
    field: tantivy::schema::Field,
    field_name: &'static str,
    value: &str,
) -> Result<RegexQuery> {
    let value = validated_filter_text(field_name, value)?;
    RegexQuery::from_pattern(
        &format!(".*{}.*", ascii_case_insensitive_regex_literal(value)),
        field,
    )
    .map_err(IndexError::from)
}

fn literal_fact_union_contains_query<const N: usize>(
    fields: [tantivy::schema::Field; N],
    field_name: &'static str,
    value: &str,
) -> Result<Box<dyn Query>> {
    let queries = fields
        .into_iter()
        .map(|field| {
            metadata_contains_query(field, field_name, value)
                .map(|query| Box::new(query) as Box<dyn Query>)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Box::new(BooleanQuery::union(queries)))
}

pub(super) fn excluded_session_tree_query(
    excluded: &ExcludedSessionTree,
    fields: Fields,
) -> Option<Box<dyn Query>> {
    if excluded.session_ids.is_empty() {
        return None;
    }
    // The read layer has already proven and materialized the complete tree.
    // Only those exact ctx session IDs are authority for query-time exclusion.
    Some(Box::new(TermSetQuery::new(
        excluded
            .session_ids
            .iter()
            .map(|session_id| Term::from_field_text(fields.session_id, &session_id.to_string()))
            .collect::<Vec<_>>(),
    )))
}

pub(super) fn validated_filter_text<'a>(field: &'static str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        return Err(IndexError::EmptyQueryFilter { field });
    }
    if value.len() > super::MAX_DOCUMENT_METADATA_BYTES {
        return Err(IndexError::QueryFilterTooLarge {
            field,
            actual: value.len(),
            maximum: super::MAX_DOCUMENT_METADATA_BYTES,
        });
    }
    Ok(value)
}

fn ascii_case_insensitive_regex_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_alphabetic() {
            escaped.push('[');
            escaped.push(character.to_ascii_lowercase());
            escaped.push(character.to_ascii_uppercase());
            escaped.push(']');
        } else {
            if matches!(
                character,
                '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
            ) {
                escaped.push('\\');
            }
            escaped.push(character);
        }
    }
    escaped
}

pub(super) fn canonical_uuid_prefix(prefix: &str) -> Result<String> {
    let mut digits = String::with_capacity(32);
    for character in prefix.chars() {
        if character == '-' {
            continue;
        }
        if !character.is_ascii_hexdigit() || digits.len() == 32 {
            return Err(IndexError::InvalidIdPrefix);
        }
        digits.push(character.to_ascii_lowercase());
    }
    if digits.is_empty() {
        return Err(IndexError::InvalidIdPrefix);
    }
    let mut canonical = String::with_capacity(digits.len() + 4);
    for (index, character) in digits.chars().enumerate() {
        if matches!(index, 8 | 12 | 16 | 20) {
            canonical.push('-');
        }
        canonical.push(character);
    }
    Ok(canonical)
}

pub(super) fn validate_event_sort_fast_fields(searcher: &tantivy::Searcher) -> Result<()> {
    for segment in searcher.segment_readers() {
        segment.fast_fields().u64(EVENT_ID_HIGH_FIELD)?;
        segment.fast_fields().u64(EVENT_ID_LOW_FIELD)?;
    }
    Ok(())
}

pub(super) fn validate_session_event_coordinate_fast_fields(
    searcher: &tantivy::Searcher,
) -> Result<()> {
    for segment in searcher.segment_readers() {
        segment.fast_fields().u64(EVENT_SEQUENCE_FIELD)?;
        segment.fast_fields().i64(OCCURRED_AT_UNIX_MS_FIELD)?;
        segment.fast_fields().u64(EVENT_ID_HIGH_FIELD)?;
        segment.fast_fields().u64(EVENT_ID_LOW_FIELD)?;
    }
    Ok(())
}

pub(super) fn session_event_coordinate_score(
    segment_reader: &tantivy::SegmentReader,
) -> impl Fn(tantivy::DocId, Score) -> SessionEventCoordinateSortKey {
    let sequence = segment_reader
        .fast_fields()
        .u64(EVENT_SEQUENCE_FIELD)
        .ok()
        .map(|column| column.first_or_default_col(0));
    let occurred_at = segment_reader
        .fast_fields()
        .i64(OCCURRED_AT_UNIX_MS_FIELD)
        .ok();
    let high = segment_reader
        .fast_fields()
        .u64(EVENT_ID_HIGH_FIELD)
        .ok()
        .map(|column| column.first_or_default_col(0));
    let low = segment_reader
        .fast_fields()
        .u64(EVENT_ID_LOW_FIELD)
        .ok()
        .map(|column| column.first_or_default_col(0));
    move |doc, _score| {
        (
            sequence.as_ref().map_or(0, |column| column.get_val(doc)),
            occurred_at.as_ref().and_then(|column| column.first(doc)),
            high.as_ref().map_or(0, |column| column.get_val(doc)),
            low.as_ref().map_or(0, |column| column.get_val(doc)),
        )
    }
}

pub(super) fn validate_session_event_coordinates(
    coordinates: &[SessionEventCoordinate],
) -> Result<()> {
    if let Some(pair) = coordinates
        .windows(2)
        .find(|pair| pair[0].sort_key() >= pair[1].sort_key())
    {
        if pair[0].event_id == pair[1].event_id {
            return Err(IndexError::DuplicateEventIdentity(
                pair[1].event_id.to_string(),
            ));
        }
        return Err(IndexError::InvalidStoredDocumentField("event_sequence"));
    }
    Ok(())
}

pub(super) fn sort_events_for_session(events: &mut [EventRecord]) {
    events.sort_by(compare_session_events);
}

pub(super) fn sort_core_events_for_session(events: &mut [CoreEventRecord]) {
    events.sort_by(|left, right| compare_session_events(&left.event, &right.event));
}

pub(super) fn compare_session_events(left: &EventRecord, right: &EventRecord) -> Ordering {
    left.event_sequence
        .cmp(&right.event_sequence)
        .then_with(|| left.occurred_at_unix_ms.cmp(&right.occurred_at_unix_ms))
        .then_with(|| left.event_id.as_uuid().cmp(&right.event_id.as_uuid()))
}

#[cfg(test)]
mod ascii_fold_substring_tests {
    use super::*;

    fn naive(haystack: &str, needle: &str) -> bool {
        haystack.as_bytes().windows(needle.len()).any(|window| {
            window
                .iter()
                .zip(needle.as_bytes())
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
        })
    }

    #[test]
    fn linear_matcher_preserves_exact_ascii_fold_semantics() {
        for (haystack, needle) in [
            ("/Work/CTX-Rich/Fixture", "ctx-rich"),
            ("src/ManualBudget.rs", "BUDGET.RS"),
            ("prefix-aAaAaB-suffix", "aaab"),
            ("punctuation/[literal].rs", "[LITERAL]"),
            ("unicode/CAFÉ/路径", "café/路径"),
            ("unicode/CAFÉ/路径", "café/路徑"),
        ] {
            let matcher = AsciiFoldSubstring::new(needle).unwrap();
            assert_eq!(
                matcher.matches(haystack.as_bytes()),
                naive(haystack, needle)
            );
        }
    }

    #[test]
    fn repeated_prefix_pathology_remains_linear() {
        let haystack = "a".repeat(64 * 1024);
        let needle = format!("{}b", "a".repeat(32 * 1024));
        let matcher = AsciiFoldSubstring::new(&needle).unwrap();

        let (matched, comparisons) = matcher.matches_with_comparison_count(haystack.as_bytes());

        assert!(!matched);
        assert!(
            comparisons <= haystack.len() * 2,
            "KMP comparison count {comparisons} exceeded the linear bound"
        );
    }
}

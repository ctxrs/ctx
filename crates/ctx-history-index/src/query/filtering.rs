use super::*;

pub(super) const TRANSCRIPT_EVENT_TYPES: &[&str] = &["message", "summary"];
pub(super) const CALL_EVENT_TYPES: &[&str] = &["tool_call", "command_started"];
pub(super) const OUTPUT_EVENT_TYPES: &[&str] =
    &["tool_output", "command_output", "command_finished"];
pub(super) fn filtered_event_query(
    body_query: Box<dyn Query>,
    source_identity_query: Option<Box<dyn Query>>,
    filters: &EventSearchFilters,
    fields: Fields,
) -> Result<Box<dyn Query>> {
    let mut clauses = vec![(Occur::Must, body_query)];
    clauses.push((
        Occur::MustNot,
        Box::new(TermQuery::new(
            Term::from_field_text(fields.event_origin_kind, "copied_from_ancestor"),
            IndexRecordOption::Basic,
        )),
    ));
    if let Some(query) = source_identity_query {
        add_filter_clause(&mut clauses, query);
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
        fields.branch,
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
    add_optional_text_filter(
        &mut clauses,
        fields.agent_type,
        "agent_type",
        filters.agent_type.as_deref(),
    )?;
    if let Some(workspace) = filters.workspace.as_deref() {
        add_filter_clause(
            &mut clauses,
            Box::new(metadata_contains_query(
                fields.workspace_filter,
                "workspace",
                workspace,
            )?),
        );
    }
    if let Some(file) = filters.file.as_deref() {
        add_filter_clause(
            &mut clauses,
            Box::new(metadata_contains_query(
                fields.touched_file_filter,
                "file",
                file,
            )?),
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
    if filters.agent_scope == AgentScope::Primary && filters.session_id.is_none() {
        add_filter_clause(
            &mut clauses,
            Box::new(BooleanQuery::union(vec![
                Box::new(TermQuery::new(
                    Term::from_field_u64(fields.is_primary, 1),
                    IndexRecordOption::Basic,
                )),
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.agent_type, "primary"),
                    IndexRecordOption::Basic,
                )),
            ])),
        );
    }
    if let Some(excluded) = &filters.exclude_session_tree {
        clauses.push((
            Occur::MustNot,
            excluded_session_tree_query(excluded, fields)?,
        ));
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
    if event.provider != "custom" {
        return None;
    }
    let Some(TypedKey::Composite(values)) = event.native_event_id.as_ref() else {
        return None;
    };
    let [TypedKey::Utf8(provider_key), TypedKey::Utf8(source_id), TypedKey::Utf8(_)] =
        values.as_slice()
    else {
        return None;
    };
    Some((provider_key, source_id))
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
    !filters
        .source_id
        .as_deref()
        .is_some_and(|expected| expected.trim() != source_id)
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
    let value = validated_filter_text(field_name, value)?.to_lowercase();
    RegexQuery::from_pattern(&format!(".*{}.*", escape_regex_literal(&value)), field)
        .map_err(IndexError::from)
}

pub(super) fn excluded_session_tree_query(
    excluded: &ExcludedSessionTree,
    fields: Fields,
) -> Result<Box<dyn Query>> {
    let provider = validated_filter_text("excluded_provider", &excluded.provider)?;
    let provider_session_id = validated_filter_text(
        "excluded_provider_session_id",
        &excluded.provider_session_id,
    )?;
    let provider_thread = BooleanQuery::intersection(vec![
        Box::new(TermQuery::new(
            Term::from_field_text(fields.provider, provider),
            IndexRecordOption::Basic,
        )),
        Box::new(TermQuery::new(
            Term::from_field_text(fields.provider_session_id, provider_session_id),
            IndexRecordOption::Basic,
        )),
    ]);
    let Some(session_id) = excluded.session_id else {
        return Ok(Box::new(provider_thread));
    };
    let session_id = session_id.to_string();
    let mut alternatives: Vec<Box<dyn Query>> = vec![Box::new(provider_thread)];
    for field in [
        fields.session_id,
        fields.parent_session_id,
        fields.root_session_id,
    ] {
        alternatives.push(Box::new(TermQuery::new(
            Term::from_field_text(field, &session_id),
            IndexRecordOption::Basic,
        )));
    }
    Ok(Box::new(BooleanQuery::union(alternatives)))
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

pub(super) fn escape_regex_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
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

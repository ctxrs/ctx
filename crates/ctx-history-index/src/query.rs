use std::{cmp::Reverse, collections::BTreeMap, ops::Bound};

use ctx_history_core::{SourceRecordLocator, StableEntityId, StableEntityKind};
use tantivy::{
    collector::{DocSetCollector, TopDocs},
    query::{BooleanQuery, ConstScoreQuery, Occur, Query, RangeQuery, RegexQuery, TermQuery},
    schema::{IndexRecordOption, Value as TantivyValue},
    tokenizer::TokenStream,
    DocAddress, Score, TantivyDocument, Term,
};
use uuid::Uuid;

use super::{
    fields_from_schema, hex, Fields, IndexError, Result, VerifiedIndex, MAX_BODY_PREVIEW_CHARS,
};

const ID_PREFIX_MATCH_LIMIT: usize = 2;
const BODY_ANALYZER: &str = "default";
const EVENT_ID_HIGH_FIELD: &str = "event_id_high";
const EVENT_ID_LOW_FIELD: &str = "event_id_low";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentScope {
    #[default]
    All,
    Primary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedSessionTree {
    pub provider: String,
    pub provider_session_id: String,
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventSearchFilters {
    pub session_id: Option<Uuid>,
    pub parent_session_id: Option<Uuid>,
    pub root_session_id: Option<Uuid>,
    pub provider: Option<String>,
    pub source_format: Option<String>,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub workspace: Option<String>,
    pub since_unix_ms: Option<i64>,
    pub event_type: Option<String>,
    pub role: Option<String>,
    pub agent_type: Option<String>,
    pub agent_scope: AgentScope,
    pub file: Option<String>,
    pub exclude_session_tree: Option<ExcludedSessionTree>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: StableEntityId,
    pub locator: SourceRecordLocator,
    pub provider: String,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub source_path: Option<String>,
    pub agent_type: String,
    pub is_primary: bool,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
    pub preview: String,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub touched_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventSearchCandidate {
    pub event: EventRecord,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: StableEntityId,
    pub provider: String,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub source_path: Option<String>,
    pub agent_type: String,
    pub is_primary: bool,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub first_event_sequence: u64,
    pub first_occurred_at_unix_ms: Option<i64>,
}

impl VerifiedIndex {
    /// Searches the bounded event previews using ordinary analyzed text.
    ///
    /// Every analyzed token is required. QueryParser operators and field
    /// syntax are intentionally not accepted.
    pub fn search_event_candidates(
        &self,
        natural_text: &str,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        self.search_event_candidates_with_filters(
            natural_text,
            &EventSearchFilters::default(),
            limit,
        )
    }

    /// Searches bounded event previews with conjunctive metadata filters.
    ///
    /// Exact-value fields use their canonical stored spelling. Workspace and
    /// touched-file filters use case-insensitive substring matching over
    /// bounded indexed metadata.
    pub fn search_event_candidates_with_filters(
        &self,
        natural_text: &str,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let fields = fields_from_schema(self.searcher.schema())?;
        let terms = self.body_query_terms(natural_text, fields)?;
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        validate_event_sort_fast_fields(&self.searcher)?;
        let body_query = BooleanQuery::intersection(
            terms
                .into_iter()
                .map(|term| {
                    Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)) as Box<dyn Query>
                })
                .collect(),
        );
        let query = filtered_event_query(Box::new(body_query), filters, fields)?;
        let collector = TopDocs::with_limit(limit).tweak_score(|segment_reader| {
            // These readers were checked above. The fallbacks keep this
            // infallible collector closure panic-free if Tantivy ever changes
            // when it resolves a validated fast field.
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
            move |doc, score| {
                let high = high.as_ref().map_or(0, |column| column.get_val(doc));
                let low = low.as_ref().map_or(0, |column| column.get_val(doc));
                (score, Reverse((high, low)))
            }
        });
        let hits: Vec<((Score, Reverse<(u64, u64)>), DocAddress)> =
            self.searcher.search(query.as_ref(), &collector)?;
        let mut candidates = Vec::with_capacity(hits.len());
        for ((score, _), address) in hits {
            candidates.push(EventSearchCandidate {
                event: self.event_record(address, fields)?,
                score,
            });
        }
        candidates.sort_by(|left, right| {
            right.score.total_cmp(&left.score).then_with(|| {
                left.event
                    .event_id
                    .as_uuid()
                    .cmp(&right.event.event_id.as_uuid())
            })
        });
        Ok(candidates)
    }

    pub fn event_by_id(&self, event_id: Uuid) -> Result<Option<EventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.event_id, &event_id.to_string()),
            IndexRecordOption::Basic,
        );
        let mut events = self.event_records_for_query(&query, fields)?;
        events.sort_by_key(|event| event.event_id.as_uuid());
        Ok(events.into_iter().next())
    }

    /// Returns at most two UUID-prefix matches, enough to distinguish a unique
    /// lookup from an ambiguous one.
    pub fn events_by_id_prefix(&self, prefix: &str) -> Result<Vec<EventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = RegexQuery::from_pattern(
            &format!("{}.*", canonical_uuid_prefix(prefix)?),
            fields.event_id,
        )?;
        let mut events = self.event_records_for_query(&query, fields)?;
        events.sort_by_key(|event| event.event_id.as_uuid());
        events.truncate(ID_PREFIX_MATCH_LIMIT);
        Ok(events)
    }

    pub fn session_by_id(&self, session_id: Uuid) -> Result<Option<SessionRecord>> {
        let events = self.events_for_session(session_id)?;
        Ok(events.first().map(SessionRecord::from))
    }

    /// Returns at most two UUID-prefix matches, enough to distinguish a unique
    /// lookup from an ambiguous one.
    pub fn sessions_by_id_prefix(&self, prefix: &str) -> Result<Vec<SessionRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = RegexQuery::from_pattern(
            &format!("{}.*", canonical_uuid_prefix(prefix)?),
            fields.session_id,
        )?;
        let mut events = self.event_records_for_query(&query, fields)?;
        sort_events_for_session(&mut events);
        let mut sessions = BTreeMap::new();
        for event in &events {
            sessions
                .entry(event.session_id.as_uuid())
                .or_insert_with(|| SessionRecord::from(event));
        }
        Ok(sessions.into_values().take(ID_PREFIX_MATCH_LIMIT).collect())
    }

    pub fn events_for_session(&self, session_id: Uuid) -> Result<Vec<EventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let mut events = self.event_records_for_query(&query, fields)?;
        sort_events_for_session(&mut events);
        Ok(events)
    }

    fn body_query_terms(&self, natural_text: &str, fields: Fields) -> Result<Vec<Term>> {
        let mut analyzer = self
            .searcher
            .index()
            .tokenizers()
            .get(BODY_ANALYZER)
            .ok_or(IndexError::MissingAnalyzer(BODY_ANALYZER))?;
        let mut stream = analyzer.token_stream(natural_text);
        let mut tokens = BTreeMap::<String, ()>::new();
        while stream.advance() {
            tokens.insert(stream.token().text.clone(), ());
        }
        Ok(tokens
            .into_keys()
            .map(|token| Term::from_field_text(fields.body_preview, &token))
            .collect())
    }

    fn event_records_for_query(
        &self,
        query: &dyn Query,
        fields: Fields,
    ) -> Result<Vec<EventRecord>> {
        let addresses = self.searcher.search(query, &DocSetCollector)?;
        let mut events = Vec::with_capacity(addresses.len());
        for address in addresses {
            events.push(self.event_record(address, fields)?);
        }
        Ok(events)
    }

    fn event_record(&self, address: DocAddress, fields: Fields) -> Result<EventRecord> {
        let document: TantivyDocument = self.searcher.doc(address)?;
        let event_id = stored_identity(
            &document,
            fields.event_identity,
            fields.event_id,
            fields.event_identity_digest,
            StableEntityKind::Event,
            "event_identity",
        )?;
        let session_id = stored_identity(
            &document,
            fields.session_identity,
            fields.session_id,
            fields.session_identity_digest,
            StableEntityKind::Session,
            "session_identity",
        )?;
        let locator: SourceRecordLocator = serde_json::from_slice(required_bytes(
            &document,
            fields.native_locator,
            "native_locator",
        )?)?;
        locator.validate_contract()?;
        if event_id.source_digest() != locator.source().identity().digest()
            || session_id.source_digest() != locator.source().identity().digest()
            || event_id.source_descriptor_digest() != locator.source().exact_descriptor_digest()
            || session_id.source_descriptor_digest() != locator.source().exact_descriptor_digest()
        {
            return Err(IndexError::InvalidStoredDocumentField("native_locator"));
        }

        let provider = required_string(&document, fields.provider, "provider")?;
        let source_format = required_string(&document, fields.source_format, "source_format")?;
        if provider != locator.source().provider()
            || source_format != locator.source().source_format()
        {
            return Err(IndexError::InvalidStoredDocumentField("provider"));
        }
        let preview = required_string(&document, fields.body_preview, "body_preview")?;
        if preview.is_empty() || preview.chars().count() > MAX_BODY_PREVIEW_CHARS {
            return Err(IndexError::InvalidStoredDocumentField("body_preview"));
        }
        let touched_files = document
            .get_all(fields.touched_file)
            .map(|value| {
                value
                    .as_str()
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .ok_or(IndexError::InvalidStoredDocumentField("touched_file"))
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(EventRecord {
            event_id,
            session_id,
            parent_session_id: optional_stored_identity(
                &document,
                fields.parent_session_identity,
                fields.parent_session_id,
                "parent_session_identity",
            )?,
            root_session_id: stored_identity_without_digest(
                &document,
                fields.root_session_identity,
                fields.root_session_id,
                "root_session_identity",
            )?,
            locator,
            provider,
            source_format,
            provider_session_id: optional_string(&document, fields.provider_session_id)?,
            branch: optional_string(&document, fields.branch)?,
            source_path: optional_string(&document, fields.source_path)?,
            agent_type: required_string(&document, fields.agent_type, "agent_type")?,
            is_primary: required_bool(&document, fields.is_primary, "is_primary")?,
            event_sequence: required_u64(&document, fields.event_sequence, "event_sequence")?,
            occurred_at_unix_ms: optional_i64(&document, fields.occurred_at_unix_ms)?,
            event_type: required_string(&document, fields.event_type, "event_type")?,
            role: optional_string(&document, fields.role)?,
            preview,
            workspace: optional_string(&document, fields.workspace)?,
            cwd: optional_string(&document, fields.cwd)?,
            touched_files,
        })
    }
}

impl From<&EventRecord> for SessionRecord {
    fn from(event: &EventRecord) -> Self {
        Self {
            session_id: event.session_id,
            parent_session_id: event.parent_session_id,
            root_session_id: event.root_session_id,
            provider: event.provider.clone(),
            source_format: event.source_format.clone(),
            provider_session_id: event.provider_session_id.clone(),
            branch: event.branch.clone(),
            source_path: event.source_path.clone(),
            agent_type: event.agent_type.clone(),
            is_primary: event.is_primary,
            workspace: event.workspace.clone(),
            cwd: event.cwd.clone(),
            first_event_sequence: event.event_sequence,
            first_occurred_at_unix_ms: event.occurred_at_unix_ms,
        }
    }
}

fn filtered_event_query(
    body_query: Box<dyn Query>,
    filters: &EventSearchFilters,
    fields: Fields,
) -> Result<Box<dyn Query>> {
    let mut clauses = vec![(Occur::Must, body_query)];
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

fn add_optional_text_filter(
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

fn add_optional_uuid_filter(
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

fn add_filter_clause(clauses: &mut Vec<(Occur, Box<dyn Query>)>, filter: Box<dyn Query>) {
    clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(filter, 0.0))));
}

fn metadata_contains_query(
    field: tantivy::schema::Field,
    field_name: &'static str,
    value: &str,
) -> Result<RegexQuery> {
    let value = validated_filter_text(field_name, value)?.to_lowercase();
    RegexQuery::from_pattern(&format!(".*{}.*", escape_regex_literal(&value)), field)
        .map_err(IndexError::from)
}

fn excluded_session_tree_query(
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

fn validated_filter_text<'a>(field: &'static str, value: &'a str) -> Result<&'a str> {
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

fn escape_regex_literal(value: &str) -> String {
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

fn stored_identity(
    document: &TantivyDocument,
    identity_field: tantivy::schema::Field,
    uuid_field: tantivy::schema::Field,
    digest_field: tantivy::schema::Field,
    expected_kind: StableEntityKind,
    field_name: &'static str,
) -> Result<StableEntityId> {
    let identity =
        StableEntityId::decode_canonical(required_bytes(document, identity_field, field_name)?)?;
    let uuid = required_string(document, uuid_field, field_name)?;
    let digest = required_string(document, digest_field, field_name)?;
    if identity.entity_kind() != expected_kind
        || uuid != identity.as_uuid().to_string()
        || digest != hex(&identity.digest())
    {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(identity)
}

fn stored_identity_without_digest(
    document: &TantivyDocument,
    identity_field: tantivy::schema::Field,
    uuid_field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<StableEntityId> {
    decode_stored_session_identity(
        required_bytes(document, identity_field, field_name)?,
        required_string(document, uuid_field, field_name)?,
        field_name,
    )
}

fn optional_stored_identity(
    document: &TantivyDocument,
    identity_field: tantivy::schema::Field,
    uuid_field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<Option<StableEntityId>> {
    let identity = document
        .get_first(identity_field)
        .and_then(|value| value.as_bytes());
    let uuid = document
        .get_first(uuid_field)
        .and_then(|value| value.as_str());
    match (identity, uuid) {
        (None, None) => Ok(None),
        (Some(identity), Some(uuid)) => {
            decode_stored_session_identity(identity, uuid.to_owned(), field_name).map(Some)
        }
        _ => Err(IndexError::InvalidStoredDocumentField(field_name)),
    }
}

fn decode_stored_session_identity(
    identity: &[u8],
    uuid: String,
    field_name: &'static str,
) -> Result<StableEntityId> {
    let identity = StableEntityId::decode_canonical(identity)?;
    if identity.entity_kind() != StableEntityKind::Session || uuid != identity.as_uuid().to_string()
    {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(identity)
}

fn required_string(
    document: &TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<String> {
    document
        .get_first(field)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))
}

fn optional_string(
    document: &TantivyDocument,
    field: tantivy::schema::Field,
) -> Result<Option<String>> {
    document
        .get_first(field)
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or(IndexError::InvalidStoredDocumentField("optional_text"))
        })
        .transpose()
}

fn required_bytes<'a>(
    document: &'a TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<&'a [u8]> {
    document
        .get_first(field)
        .and_then(|value| value.as_bytes())
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))
}

fn required_u64(
    document: &TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<u64> {
    document
        .get_first(field)
        .and_then(|value| value.as_u64())
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))
}

fn required_bool(
    document: &TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<bool> {
    match required_u64(document, field, field_name)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(IndexError::InvalidStoredDocumentField(field_name)),
    }
}

fn optional_i64(document: &TantivyDocument, field: tantivy::schema::Field) -> Result<Option<i64>> {
    document
        .get_first(field)
        .map(|value| {
            value.as_i64().ok_or(IndexError::InvalidStoredDocumentField(
                "occurred_at_unix_ms",
            ))
        })
        .transpose()
}

fn canonical_uuid_prefix(prefix: &str) -> Result<String> {
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

fn validate_event_sort_fast_fields(searcher: &tantivy::Searcher) -> Result<()> {
    for segment in searcher.segment_readers() {
        segment.fast_fields().u64(EVENT_ID_HIGH_FIELD)?;
        segment.fast_fields().u64(EVENT_ID_LOW_FIELD)?;
    }
    Ok(())
}

fn sort_events_for_session(events: &mut [EventRecord]) {
    events.sort_by(|left, right| {
        left.event_sequence
            .cmp(&right.event_sequence)
            .then_with(|| left.occurred_at_unix_ms.cmp(&right.occurred_at_unix_ms))
            .then_with(|| left.event_id.as_uuid().cmp(&right.event_id.as_uuid()))
    });
}

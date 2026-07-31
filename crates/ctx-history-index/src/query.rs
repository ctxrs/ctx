mod execution;
mod verification;

pub(super) use verification::{stored_verification_record, validate_verification_projection};

#[cfg(test)]
use std::cell::Cell;
use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BTreeSet},
    ops::Bound,
};

use ctx_history_core::{
    CoreRecord, SourceKey, StableEntityId, StableEntityKind, TypedKey, MAX_CORE_CONTENT_BYTES,
    MAX_ENCODED_CORE_RECORD_BYTES,
};
use serde::{Deserialize, Serialize};
use tantivy::{
    collector::{Count, DocSetCollector, TopDocs},
    query::{
        AllQuery, BooleanQuery, ConstScoreQuery, EmptyQuery, Occur, Query, RangeQuery, RegexQuery,
        TermQuery, TermSetQuery,
    },
    schema::{IndexRecordOption, Value as TantivyValue},
    tokenizer::TokenStream,
    DocAddress, DocSet, Score, TantivyDocument, Term, TERMINATED,
};
use uuid::Uuid;

use super::{fields_from_schema, hex, source_token, Fields, IndexError, Result, VerifiedIndex};
use crate::index_document::{core_content_bytes, SourceEventOrderKey};

const ID_PREFIX_MATCH_LIMIT: usize = 2;
use crate::analyzer::BODY_ANALYZER;
const EVENT_ID_HIGH_FIELD: &str = "event_id_high";
const EVENT_ID_LOW_FIELD: &str = "event_id_low";
const EVENT_SEQUENCE_FIELD: &str = "event_sequence";
const OCCURRED_AT_UNIX_MS_FIELD: &str = "occurred_at_unix_ms";
const EVENT_IDENTITY_DIGEST_FIELD: &str = "event_identity_digest";
const SOURCE_EVENT_ORDER_FIELD: &str = "source_event_order";

#[cfg(test)]
thread_local! {
    static STORED_CORE_EVENT_RECORD_MATERIALIZATIONS: Cell<usize> = const { Cell::new(0) };
    static SOURCE_EVENT_ORDER_TERM_VISITS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_stored_core_event_record_materializations() {
    STORED_CORE_EVENT_RECORD_MATERIALIZATIONS.set(0);
}

#[cfg(test)]
pub(crate) fn stored_core_event_record_materializations() -> usize {
    STORED_CORE_EVENT_RECORD_MATERIALIZATIONS.get()
}

#[cfg(test)]
pub(crate) fn reset_source_event_order_term_visits() {
    SOURCE_EVENT_ORDER_TERM_VISITS.set(0);
}

#[cfg(test)]
pub(crate) fn source_event_order_term_visits() -> usize {
    SOURCE_EVENT_ORDER_TERM_VISITS.get()
}

/// Maximum number of complete semantic event records retained in one page.
pub const MAX_SEMANTIC_EVENT_PAGE_ITEMS: usize = 64;

/// Maximum number of complete records retained for one exact source page.
pub const MAX_SOURCE_EVENT_PAGE_ITEMS: usize = 4_096;

/// Maximum retained coordinate prefix, including one truncation lookahead.
pub const MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS: usize = 4_097;

/// Maximum retained centered event-window coordinates.
pub const MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS: usize = 101;

/// Default retained-byte ceiling for complete Core pages.
///
/// One individually valid Core record always makes progress even when it is
/// larger than a caller's chosen page budget. These defaults therefore also
/// define the absolute maximum resident singleton page.
pub const DEFAULT_CORE_EVENT_PAGE_BUDGET: CoreEventPageBudget = CoreEventPageBudget {
    maximum_encoded_core_bytes: MAX_ENCODED_CORE_RECORD_BYTES,
    maximum_content_bytes: MAX_CORE_CONTENT_BYTES,
};

/// Retained complete-Core byte ceilings for one source or semantic page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreEventPageBudget {
    pub maximum_encoded_core_bytes: usize,
    pub maximum_content_bytes: usize,
}

impl CoreEventPageBudget {
    pub const fn new(maximum_encoded_core_bytes: usize, maximum_content_bytes: usize) -> Self {
        Self {
            maximum_encoded_core_bytes,
            maximum_content_bytes,
        }
    }
}

/// Exclusive full-identity keyset cursor for one source in one generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEventCursor {
    generation_id: String,
    source: SourceKey,
    after: StableEntityId,
}

impl SourceEventCursor {
    pub fn new(generation_id: impl Into<String>, source: SourceKey, after: StableEntityId) -> Self {
        Self {
            generation_id: generation_id.into(),
            source,
            after,
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn after(&self) -> StableEntityId {
        self.after
    }
}

/// One deterministic page of existing bounded records for an exact source.
#[derive(Debug, Clone)]
pub struct SourceEventPage {
    pub generation_id: String,
    pub source: SourceKey,
    pub items: Vec<EventRecord>,
    pub next_cursor: Option<SourceEventCursor>,
    pub terminal: bool,
}

/// One deterministic page of complete Core records for an exact source.
#[derive(Debug, Clone)]
pub struct CoreSourceEventPage {
    pub generation_id: String,
    pub source: SourceKey,
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<SourceEventCursor>,
    pub terminal: bool,
}

/// Complete requested-order Core records plus exact retained byte totals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventBatch {
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
}

impl From<CoreSourceEventPage> for SourceEventPage {
    fn from(page: CoreSourceEventPage) -> Self {
        Self {
            generation_id: page.generation_id,
            source: page.source,
            items: page.items.into_iter().map(|record| record.event).collect(),
            next_cursor: page.next_cursor,
            terminal: page.terminal,
        }
    }
}

/// Stable metadata-only candidate policy for semantic projection from Core.
///
/// Candidate enumeration remains metadata-only. Downstream semantic projection
/// reads complete stored Core content and applies the generation policy's Core
/// content filter before chunking or embedding. This contract is independent
/// of lexical query terms, scores, and ranking. Future candidate changes must
/// add a new enum variant instead of changing the meaning of this variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticEligibility {
    UserMessageCandidateV2,
}

impl SemanticEligibility {
    pub const CURRENT: Self = Self::UserMessageCandidateV2;

    pub fn includes(self, event: &EventRecord) -> bool {
        match self {
            Self::UserMessageCandidateV2 => {
                event.event_type == "message" && event.role.as_deref() == Some("user")
            }
        }
    }
}

/// Exclusive full-identity keyset cursor bound to one verified generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticEventCursor {
    generation_id: String,
    eligibility: SemanticEligibility,
    after: StableEntityId,
}

impl SemanticEventCursor {
    pub fn new(generation_id: impl Into<String>, after: StableEntityId) -> Self {
        Self {
            generation_id: generation_id.into(),
            eligibility: SemanticEligibility::CURRENT,
            after,
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn eligibility(&self) -> SemanticEligibility {
        self.eligibility
    }

    pub fn after(&self) -> StableEntityId {
        self.after
    }
}

/// One deterministic page of metadata-selected semantic candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEventPage {
    pub generation_id: String,
    pub eligibility: SemanticEligibility,
    /// Exact count of metadata candidates before Core content filtering.
    pub eligible_total: u64,
    pub items: Vec<EventRecord>,
    pub next_cursor: Option<SemanticEventCursor>,
    pub terminal: bool,
}

impl SemanticEventPage {
    pub fn eligible_count(&self) -> usize {
        self.items.len()
    }
}

/// One deterministic page of complete Core semantic candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreSemanticEventPage {
    pub generation_id: String,
    pub eligibility: SemanticEligibility,
    pub eligible_total: u64,
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<SemanticEventCursor>,
    pub terminal: bool,
}

impl CoreSemanticEventPage {
    pub fn eligible_count(&self) -> usize {
        self.items.len()
    }
}

impl From<CoreSemanticEventPage> for SemanticEventPage {
    fn from(page: CoreSemanticEventPage) -> Self {
        Self {
            generation_id: page.generation_id,
            eligibility: page.eligibility,
            eligible_total: page.eligible_total,
            items: page.items.into_iter().map(|record| record.event).collect(),
            next_cursor: page.next_cursor,
            terminal: page.terminal,
        }
    }
}

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
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
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

impl EventSearchFilters {
    pub fn matches_source_identity(&self, event: &EventRecord) -> bool {
        if !self.has_source_identity_filter() {
            return true;
        }
        custom_source_identity(event).is_some_and(|(provider_key, source_id)| {
            source_identity_values_match(self, provider_key, source_id)
        })
    }

    fn has_source_identity_filter(&self) -> bool {
        self.history_source.is_some() || self.provider_key.is_some() || self.source_id.is_some()
    }

    fn validate_source_identity_filters(&self) -> Result<()> {
        for (field, value) in [
            ("history_source", self.history_source.as_deref()),
            ("provider_key", self.provider_key.as_deref()),
            ("source_id", self.source_id.as_deref()),
        ] {
            if let Some(value) = value {
                validated_filter_text(field, value)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: StableEntityId,
    pub source: SourceKey,
    pub provider: String,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub native_event_id: Option<TypedKey>,
    pub branch: Option<String>,
    pub source_path: Option<String>,
    pub agent_type: String,
    pub is_primary: bool,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub touched_files: Vec<String>,
}

/// One verified event plus its complete generation-owned Core data.
///
/// The wrapper preserves the compact query metadata alongside the complete
/// self-contained record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventRecord {
    pub event: EventRecord,
    pub core_record: CoreRecord,
}

impl std::ops::Deref for CoreEventRecord {
    type Target = EventRecord;

    fn deref(&self) -> &Self::Target {
        &self.event
    }
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

/// Small body-free session coordinate used to select bounded Core batches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEventCoordinate {
    pub event_id: Uuid,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
}

type SessionEventCoordinateSortKey = (u64, Option<i64>, u64, u64);

impl SessionEventCoordinate {
    fn from_sort_key(sort_key: SessionEventCoordinateSortKey) -> Self {
        let (event_sequence, occurred_at_unix_ms, event_id_high, event_id_low) = sort_key;
        Self {
            event_id: Uuid::from_u128((u128::from(event_id_high) << 64) | u128::from(event_id_low)),
            event_sequence,
            occurred_at_unix_ms,
        }
    }

    fn sort_key(&self) -> SessionEventCoordinateSortKey {
        let event_id = self.event_id.as_u128();
        (
            self.event_sequence,
            self.occurred_at_unix_ms,
            (event_id >> 64) as u64,
            event_id as u64,
        )
    }
}

pub(super) fn stored_event_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<EventRecord> {
    Ok(stored_core_event_record(searcher, address, fields)?.event)
}

pub(super) fn stored_core_event_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<CoreEventRecord> {
    stored_core_event_record_with_size(searcher, address, fields).map(|(record, _)| record)
}

pub(super) fn stored_core_event_record_with_size(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<(CoreEventRecord, usize)> {
    #[cfg(test)]
    STORED_CORE_EVENT_RECORD_MATERIALIZATIONS.set(
        STORED_CORE_EVENT_RECORD_MATERIALIZATIONS
            .get()
            .saturating_add(1),
    );
    let document: TantivyDocument = searcher.doc(address)?;
    let encoded_core_record = required_bytes(&document, fields.core_record, "core_record")?;
    let stored_core_bytes = encoded_core_record.len();
    let core_record = CoreRecord::decode_stored(encoded_core_record)?;
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
    let stored_source = required_string(&document, fields.source_key, "source_key")?;
    if stored_source != source_token(&core_record.source)
        || event_id != core_record.event_id
        || session_id != core_record.session_id
    {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }

    let provider = required_string(&document, fields.provider, "provider")?;
    let source_format = required_string(&document, fields.source_format, "source_format")?;
    if provider != core_record.source.provider()
        || source_format != core_record.source.source_format()
    {
        return Err(IndexError::InvalidStoredDocumentField("provider"));
    }
    let parent_session_id = optional_stored_identity(
        &document,
        fields.parent_session_identity,
        fields.parent_session_id,
        "parent_session_identity",
    )?;
    let root_session_id = stored_identity_without_digest(
        &document,
        fields.root_session_identity,
        fields.root_session_id,
        "root_session_identity",
    )?;
    let provider_session_id = optional_string(&document, fields.provider_session_id)?;
    let branch = optional_string(&document, fields.branch)?;
    let agent_type = required_string(&document, fields.agent_type, "agent_type")?;
    let is_primary = required_bool(&document, fields.is_primary, "is_primary")?;
    let event_sequence = required_u64(&document, fields.event_sequence, "event_sequence")?;
    let occurred_at_unix_ms = optional_i64(&document, fields.occurred_at_unix_ms)?;
    let event_type = required_string(&document, fields.event_type, "event_type")?;
    let role = optional_string(&document, fields.role)?;
    let workspace = optional_string(&document, fields.workspace)?;
    let cwd = optional_string(&document, fields.cwd)?;
    if parent_session_id != core_record.parent_session_id
        || root_session_id != core_record.root_session_id
        || provider_session_id != core_record.provider_session_id
        || branch != core_record.branch
        || agent_type != core_record.agent_type
        || is_primary != core_record.is_primary
        || event_sequence != core_record.event_sequence
        || occurred_at_unix_ms != core_record.occurred_at_unix_ms
        || event_type != core_record.event_type
        || role != core_record.role
        || workspace != core_record.workspace
        || cwd != core_record.cwd
    {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
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

    Ok((
        CoreEventRecord {
            event: EventRecord {
                event_id,
                session_id,
                parent_session_id,
                root_session_id,
                source: core_record.source.clone(),
                provider,
                source_format,
                provider_session_id,
                native_event_id: core_record.native_event_id.clone(),
                branch,
                source_path: optional_string(&document, fields.source_path)?,
                agent_type,
                is_primary,
                event_sequence,
                occurred_at_unix_ms,
                event_type,
                role,
                workspace,
                cwd,
                touched_files,
            },
            core_record,
        },
        stored_core_bytes,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EventAddressCandidate {
    identity_digest: [u8; 32],
    address: DocAddress,
    source_order: Option<SourceEventOrderKey>,
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
    source_identity_query: Option<Box<dyn Query>>,
    filters: &EventSearchFilters,
    fields: Fields,
) -> Result<Box<dyn Query>> {
    let mut clauses = vec![(Occur::Must, body_query)];
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

fn custom_source_identity(event: &EventRecord) -> Option<(&str, &str)> {
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

fn source_identity_values_match(
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

fn validate_session_event_coordinate_fast_fields(searcher: &tantivy::Searcher) -> Result<()> {
    for segment in searcher.segment_readers() {
        segment.fast_fields().u64(EVENT_SEQUENCE_FIELD)?;
        segment.fast_fields().i64(OCCURRED_AT_UNIX_MS_FIELD)?;
        segment.fast_fields().u64(EVENT_ID_HIGH_FIELD)?;
        segment.fast_fields().u64(EVENT_ID_LOW_FIELD)?;
    }
    Ok(())
}

fn session_event_coordinate_score(
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

fn validate_session_event_coordinates(coordinates: &[SessionEventCoordinate]) -> Result<()> {
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

fn sort_events_for_session(events: &mut [EventRecord]) {
    events.sort_by(compare_session_events);
}

fn sort_core_events_for_session(events: &mut [CoreEventRecord]) {
    events.sort_by(|left, right| compare_session_events(&left.event, &right.event));
}

fn compare_session_events(left: &EventRecord, right: &EventRecord) -> Ordering {
    left.event_sequence
        .cmp(&right.event_sequence)
        .then_with(|| left.occurred_at_unix_ms.cmp(&right.occurred_at_unix_ms))
        .then_with(|| left.event_id.as_uuid().cmp(&right.event_id.as_uuid()))
}

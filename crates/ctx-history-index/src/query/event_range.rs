use super::*;

use sha2::{Digest, Sha256};

use crate::{index_document::EventRangeOrderKey, is_generation_id};

const EVENT_RANGE_CURSOR_VERSION: u8 = 2;
const EVENT_RANGE_CURSOR_BYTES: usize = 1 + 64 + 32 + crate::index_document::EVENT_RANGE_ORDER_KEY_LEN + 32;
pub const MAX_CORE_EVENT_RANGE_PAGE_ITEMS: usize = 4_096;
const MAX_EVENT_RANGE_PROVIDERS: usize = 64;
const MAX_PROVIDER_FILTER_BYTES: usize = 256;
const CURSOR_PAYLOAD_BYTES: usize = EVENT_RANGE_CURSOR_BYTES - 32;
const CURSOR_DOMAIN: &[u8] = b"ctx-core-event-range-cursor-v2\0";
const SELECTION_DOMAIN: &[u8] = b"ctx-core-event-range-selection-v3\0";
const EVENT_RANGE_ORDER_FIELD: &str = "event_range_order";

#[derive(Debug, Clone, Copy)]
struct EventRangeAddressCandidate {
    order: EventRangeOrderKey,
    address: DocAddress,
}

enum OrderedTermMerger<'a> {
    Ascending(TermMerger<'a>),
    Descending(ReverseTermMerger<'a>),
}

impl OrderedTermMerger<'_> {
    fn advance(&mut self) -> bool {
        match self {
            Self::Ascending(merger) => merger.advance(),
            Self::Descending(merger) => merger.advance(),
        }
    }

    fn key(&self) -> &[u8] {
        match self {
            Self::Ascending(merger) => merger.key(),
            Self::Descending(merger) => merger.key(),
        }
    }

    fn current_segment_ords_and_term_infos(
        &self,
    ) -> Vec<(usize, tantivy::postings::TermInfo)> {
        match self {
            Self::Ascending(merger) => merger
                .current_segment_ords_and_term_infos()
                .collect(),
            Self::Descending(merger) => merger.current_segment_ords_and_term_infos(),
        }
    }
}

/// Tantivy exposes reverse term streams but its public multi-segment merger is
/// forward-only. This bounded merger retains one cursor per immutable segment.
struct ReverseTermMerger<'a> {
    streams: Vec<TermStreamer<'a>>,
    active: Vec<bool>,
    current_segments: Vec<usize>,
    current_key: Vec<u8>,
    initialized: bool,
}

impl<'a> ReverseTermMerger<'a> {
    fn new(streams: Vec<TermStreamer<'a>>) -> Self {
        let active = vec![false; streams.len()];
        Self {
            streams,
            active,
            current_segments: Vec::new(),
            current_key: Vec::with_capacity(crate::index_document::EVENT_RANGE_ORDER_KEY_LEN),
            initialized: false,
        }
    }

    fn advance(&mut self) -> bool {
        if self.initialized {
            for segment in self.current_segments.drain(..) {
                self.active[segment] = self.streams[segment].advance();
            }
        } else {
            for (active, stream) in self.active.iter_mut().zip(&mut self.streams) {
                *active = stream.advance();
            }
            self.initialized = true;
        }

        self.current_key.clear();
        for (segment, stream) in self.streams.iter().enumerate() {
            if self.active[segment]
                && (self.current_key.is_empty() || stream.key() > self.current_key.as_slice())
            {
                self.current_key.clear();
                self.current_key.extend_from_slice(stream.key());
            }
        }
        if self.current_key.is_empty() {
            return false;
        }
        self.current_segments.extend(
            self.streams
                .iter()
                .enumerate()
                .filter_map(|(segment, stream)| {
                    (self.active[segment] && stream.key() == self.current_key).then_some(segment)
                }),
        );
        true
    }

    fn key(&self) -> &[u8] {
        &self.current_key
    }

    fn current_segment_ords_and_term_infos(
        &self,
    ) -> Vec<(usize, tantivy::postings::TermInfo)> {
        self.current_segments
            .iter()
            .map(|segment| (*segment, self.streams[*segment].value().clone()))
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CoreEventRangeError {
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(
        "event range must be nonempty and half-open: since {since_unix_ms}, until {until_unix_ms}"
    )]
    InvalidRange {
        since_unix_ms: i64,
        until_unix_ms: i64,
    },
    #[error("invalid event range filter {field}")]
    InvalidFilter { field: &'static str },
    #[error("event range page size {requested} is outside 1..={maximum}")]
    InvalidPageSize { requested: usize, maximum: usize },
    #[error("invalid event range cursor encoding or integrity")]
    InvalidCursor,
    #[error("event range cursor selection does not match this request")]
    CursorSelectionMismatch,
    #[error("event range cursor generation {cursor_generation} does not match pinned generation {pinned_generation}")]
    CursorGenerationMismatch {
        cursor_generation: String,
        pinned_generation: String,
    },
    #[error("invalid event range cursor coordinate")]
    InvalidCursorCoordinate,
}

type CoreEventRangeResult<T> = std::result::Result<T, CoreEventRangeError>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CoreEventRangeScope {
    #[default]
    All,
    Primary,
    Subagent,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CoreEventRangeDirection {
    #[default]
    Ascending,
    Descending,
}

/// Which part of the immutable Core event corpus is enumerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreEventRangeDomain {
    /// Every event, with timestamped events first and untimestamped events in a
    /// deterministic tail (or the exact reverse for descending traversal).
    All,
    /// Timestamped events in the half-open interval `[since, until)`.
    Timestamped {
        since_unix_ms: i64,
        until_unix_ms: i64,
    },
}

/// Filters evaluated against one immutable Core generation before page limits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoreEventRangeFilters {
    pub providers: Vec<String>,
    pub source_identity: Option<Uuid>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub provider_session_id: Option<String>,
    pub session_id: Option<Uuid>,
    pub parent_session_id: Option<Uuid>,
    pub root_session_id: Option<Uuid>,
    pub branch: Option<String>,
    pub workspace: Option<String>,
    pub event_type: Option<String>,
    pub role: Option<String>,
    pub agent_type: Option<String>,
    pub scope: CoreEventRangeScope,
    pub file: Option<String>,
    pub direction: CoreEventRangeDirection,
}

/// Canonical event selection shared by first and continuation pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventRangeSelection {
    domain: CoreEventRangeDomain,
    filters: CoreEventRangeFilters,
    history_source_parts: Option<(String, String)>,
    digest: [u8; 32],
}

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

    fn accepts_order(&self, order: EventRangeOrderKey) -> bool {
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

    fn accepts_indexed(
        &self,
        segment: &SegmentReader,
        doc: DocId,
        fields: Fields,
        source_token: Option<&str>,
    ) -> Result<bool> {
        let filters = &self.filters;
        if let Some(source_token) = source_token {
            let term = Term::from_field_text(fields.source_key, source_token);
            if !indexed_term_matches(segment, doc, fields.source_key, &term)? {
                return Ok(false);
            }
        }
        if !filters.providers.is_empty()
            && !filters
                .providers
                .iter()
                .try_fold(false, |matched, provider| {
                    Ok::<_, IndexError>(
                        matched
                            || indexed_term_matches(
                                segment,
                                doc,
                                fields.provider,
                                &Term::from_field_text(fields.provider, provider),
                            )?,
                    )
                })?
        {
            return Ok(false);
        }
        for (field, expected) in [
            (fields.source_format, filters.source_format.clone()),
            (
                fields.provider_session_id,
                filters.provider_session_id.clone(),
            ),
            (
                fields.session_id,
                filters.session_id.map(|value| value.to_string()),
            ),
            (
                fields.parent_session_id,
                filters.parent_session_id.map(|value| value.to_string()),
            ),
            (
                fields.root_session_id,
                filters.root_session_id.map(|value| value.to_string()),
            ),
            (fields.branch, filters.branch.clone()),
            (fields.event_type, filters.event_type.clone()),
            (fields.role, filters.role.clone()),
            (fields.agent_type, filters.agent_type.clone()),
            (fields.custom_provider_key, filters.provider_key.clone()),
            (fields.custom_source_id, filters.source_id.clone()),
        ] {
            if let Some(expected) = expected.as_deref() {
                let term = Term::from_field_text(field, expected);
                if !indexed_term_matches(segment, doc, field, &term)? {
                    return Ok(false);
                }
            }
        }
        if let Some((provider_key, source_id)) = self.history_source_parts.as_ref() {
            let provider_term = Term::from_field_text(fields.custom_provider_key, provider_key);
            let source_term = Term::from_field_text(fields.custom_source_id, source_id);
            if !indexed_term_matches(segment, doc, fields.custom_provider_key, &provider_term)?
                || !indexed_term_matches(segment, doc, fields.custom_source_id, &source_term)?
            {
                return Ok(false);
            }
        }
        if filters.scope != CoreEventRangeScope::All {
            let expected = u64::from(filters.scope == CoreEventRangeScope::Primary);
            let term = Term::from_field_u64(fields.is_primary, expected);
            if !indexed_term_matches(segment, doc, fields.is_primary, &term)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn accepts_record(&self, event: &CoreEventRecord) -> bool {
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

/// Fixed-size, versioned cursor for one immutable generation. The checksum
/// detects accidental corruption; it is not an authentication mechanism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventRangeCursor {
    generation_id: String,
    selection_digest: [u8; 32],
    after: EventRangeOrderKey,
}

impl CoreEventRangeCursor {
    fn new(
        generation_id: &str,
        selection_digest: [u8; 32],
        after: EventRangeOrderKey,
    ) -> CoreEventRangeResult<Self> {
        if !is_generation_id(generation_id) {
            return Err(CoreEventRangeError::InvalidCursor);
        }
        Ok(Self {
            generation_id: generation_id.to_owned(),
            selection_digest,
            after,
        })
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn validate_selection(
        &self,
        selection: &CoreEventRangeSelection,
    ) -> CoreEventRangeResult<()> {
        if self.selection_digest != selection.digest {
            return Err(CoreEventRangeError::CursorSelectionMismatch);
        }
        if !selection.accepts_order(self.after) {
            return Err(CoreEventRangeError::InvalidCursorCoordinate);
        }
        Ok(())
    }

    pub fn encode(&self) -> [u8; EVENT_RANGE_CURSOR_BYTES] {
        let mut encoded = [0_u8; EVENT_RANGE_CURSOR_BYTES];
        encoded[0] = EVENT_RANGE_CURSOR_VERSION;
        encoded[1..65].copy_from_slice(self.generation_id.as_bytes());
        encoded[65..97].copy_from_slice(&self.selection_digest);
        encoded[97..CURSOR_PAYLOAD_BYTES].copy_from_slice(self.after.as_bytes());
        let checksum = cursor_checksum(&encoded[..CURSOR_PAYLOAD_BYTES]);
        encoded[CURSOR_PAYLOAD_BYTES..].copy_from_slice(&checksum);
        encoded
    }

    pub fn decode(encoded: &[u8]) -> CoreEventRangeResult<Self> {
        if encoded.len() != EVENT_RANGE_CURSOR_BYTES || encoded[0] != EVENT_RANGE_CURSOR_VERSION {
            return Err(CoreEventRangeError::InvalidCursor);
        }
        if cursor_checksum(&encoded[..CURSOR_PAYLOAD_BYTES]) != encoded[CURSOR_PAYLOAD_BYTES..] {
            return Err(CoreEventRangeError::InvalidCursor);
        }
        let generation_id =
            std::str::from_utf8(&encoded[1..65]).map_err(|_| CoreEventRangeError::InvalidCursor)?;
        let mut selection_digest = [0_u8; 32];
        selection_digest.copy_from_slice(&encoded[65..97]);
        let after = EventRangeOrderKey::decode(&encoded[97..CURSOR_PAYLOAD_BYTES])
            .map_err(|_| CoreEventRangeError::InvalidCursor)?;
        Self::new(
            generation_id,
            selection_digest,
            after,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventRangePage {
    pub generation_id: String,
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub oversized_singleton: bool,
    pub next_cursor: Option<CoreEventRangeCursor>,
    pub terminal: bool,
}

impl VerifiedIndex {
    pub fn core_event_range_page(
        &self,
        selection: &CoreEventRangeSelection,
        cursor: Option<&CoreEventRangeCursor>,
        limit: usize,
    ) -> CoreEventRangeResult<CoreEventRangePage> {
        self.core_event_range_page_with_budget(
            selection,
            cursor,
            limit,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )
    }

    pub fn core_event_range_page_with_budget(
        &self,
        selection: &CoreEventRangeSelection,
        cursor: Option<&CoreEventRangeCursor>,
        limit: usize,
        budget: CoreEventPageBudget,
    ) -> CoreEventRangeResult<CoreEventRangePage> {
        if !(1..=MAX_CORE_EVENT_RANGE_PAGE_ITEMS).contains(&limit) {
            return Err(CoreEventRangeError::InvalidPageSize {
                requested: limit,
                maximum: MAX_CORE_EVENT_RANGE_PAGE_ITEMS,
            });
        }
        super::execution::validate_core_event_page_budget(budget)?;
        let source_token = self.event_range_source_token(selection);
        if selection.filters.source_identity.is_some() && source_token.is_none() {
            if cursor.is_some() {
                return Err(CoreEventRangeError::InvalidCursorCoordinate);
            }
            return Ok(CoreEventRangePage {
                generation_id: self.generation_id.clone(),
                items: Vec::new(),
                encoded_core_bytes: 0,
                content_bytes: 0,
                oversized_singleton: false,
                next_cursor: None,
                terminal: true,
            });
        }
        let fields = fields_from_schema(self.searcher.schema())?;
        if let Some(cursor) = cursor {
            if cursor.generation_id != self.generation_id {
                return Err(CoreEventRangeError::CursorGenerationMismatch {
                    cursor_generation: cursor.generation_id.clone(),
                    pinned_generation: self.generation_id.clone(),
                });
            }
            cursor.validate_selection(selection)?;
            self.validate_event_range_cursor(selection, cursor, fields, source_token.as_deref())?;
        }

        let (candidates, has_more, encoded_core_bytes, content_bytes) = self
            .event_range_candidates(
                selection,
                cursor.map(|cursor| cursor.after),
                limit,
                budget,
                fields,
                source_token.as_deref(),
            )?;
        let items = candidates;

        let terminal = !has_more;
        let next_cursor = if terminal {
            None
        } else {
            items
                .last()
                .map(|event| selection.cursor_for(&self.generation_id, event))
                .transpose()?
        };
        if !terminal && next_cursor.is_none() {
            return Err(CoreEventRangeError::InvalidCursorCoordinate);
        }
        let oversized_singleton = items.len() == 1
            && (encoded_core_bytes > budget.maximum_encoded_core_bytes
                || content_bytes > budget.maximum_content_bytes);
        Ok(CoreEventRangePage {
            generation_id: self.generation_id.clone(),
            items,
            encoded_core_bytes,
            content_bytes,
            oversized_singleton,
            next_cursor,
            terminal,
        })
    }

    fn validate_event_range_cursor(
        &self,
        selection: &CoreEventRangeSelection,
        cursor: &CoreEventRangeCursor,
        fields: Fields,
        source_token: Option<&str>,
    ) -> CoreEventRangeResult<()> {
        let exact_order = TermQuery::new(
            Term::from_field_bytes(fields.event_range_order, cursor.after.as_bytes()),
            IndexRecordOption::Basic,
        );
        let query = BooleanQuery::new(vec![
            (Occur::Must, Box::new(exact_order)),
            (
                Occur::Must,
                event_range_filter_query(selection, fields, source_token)?,
            ),
        ]);
        if self.searcher.search(&query, &Count).map_err(IndexError::from)? != 1 {
            return Err(CoreEventRangeError::InvalidCursorCoordinate);
        }
        Ok(())
    }

    fn event_range_source_token(&self, selection: &CoreEventRangeSelection) -> Option<String> {
        let expected = selection.filters.source_identity?;
        self.manifest.sources.iter().find_map(|certified| {
            let source = certified.observation().source();
            (source.identity().as_uuid() == expected).then(|| source_token(source))
        })
    }

    fn event_range_candidates(
        &self,
        selection: &CoreEventRangeSelection,
        after: Option<EventRangeOrderKey>,
        limit: usize,
        budget: CoreEventPageBudget,
        fields: Fields,
        source_token: Option<&str>,
    ) -> CoreEventRangeResult<(Vec<CoreEventRecord>, bool, usize, usize)> {
        if selection.has_fully_indexed_filter() {
            return self.indexed_event_range_candidates(
                selection,
                after,
                limit,
                budget,
                fields,
                source_token,
            );
        }
        let segments = self.searcher.segment_readers();
        let inverted_indexes = segments
            .iter()
            .map(|segment| segment.inverted_index(fields.event_range_order))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(IndexError::from)?;
        let range_bounds = match selection.domain {
            CoreEventRangeDomain::All => None,
            CoreEventRangeDomain::Timestamped {
                since_unix_ms,
                until_unix_ms,
            } => Some((
                EventRangeOrderKey::timestamp_prefix(since_unix_ms),
                EventRangeOrderKey::timestamp_prefix(until_unix_ms),
            )),
        };
        let streams = if selection.filters.direction == CoreEventRangeDirection::Ascending {
            inverted_indexes
                .iter()
                .map(|inverted| {
                    let mut range = inverted.terms().range();
                    if let Some((since, until)) = range_bounds.as_ref() {
                        range = range.ge(since).lt(until);
                    }
                    if let Some(after) = after.as_ref() {
                        range = range.gt(after.as_bytes());
                    }
                    range.into_stream()
                })
                .collect::<std::io::Result<Vec<_>>>()
        } else {
            inverted_indexes
                .iter()
                .map(|inverted| {
                    let mut range = inverted.terms().range();
                    if let Some((since, until)) = range_bounds.as_ref() {
                        range = range.ge(since).lt(until);
                    }
                    if let Some(after) = after.as_ref() {
                        range = range.lt(after.as_bytes());
                    }
                    range.backward().into_stream()
                })
                .collect::<std::io::Result<Vec<_>>>()
        }
        .map_err(IndexError::from)?;
        let mut merged = match selection.filters.direction {
            CoreEventRangeDirection::Ascending => {
                OrderedTermMerger::Ascending(TermMerger::new(streams))
            }
            CoreEventRangeDirection::Descending => {
                OrderedTermMerger::Descending(ReverseTermMerger::new(streams))
            }
        };
        let mut candidates = Vec::with_capacity(limit);
        let mut encoded_core_bytes = 0_usize;
        let mut content_bytes = 0_usize;
        while merged.advance() {
            #[cfg(test)]
            EVENT_RANGE_ORDER_TERM_VISITS
                .set(EVENT_RANGE_ORDER_TERM_VISITS.get().saturating_add(1));
            let order = EventRangeOrderKey::decode(merged.key())?;
            let mut address = None;
            for (segment_ord, term_info) in merged.current_segment_ords_and_term_infos() {
                let inverted = inverted_indexes.get(segment_ord).ok_or(
                    IndexError::InvalidStoredDocumentField(EVENT_RANGE_ORDER_FIELD),
                )?;
                let segment =
                    segments
                        .get(segment_ord)
                        .ok_or(IndexError::InvalidStoredDocumentField(
                            EVENT_RANGE_ORDER_FIELD,
                        ))?;
                let mut postings = inverted
                    .read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)
                    .map_err(IndexError::from)?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED {
                    if !segment.is_deleted(doc_id) {
                        let segment_ord =
                            u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
                        if address
                            .replace(DocAddress::new(segment_ord, doc_id))
                            .is_some()
                        {
                            return Err(IndexError::InvalidStoredDocumentField(
                                EVENT_RANGE_ORDER_FIELD,
                            )
                            .into());
                        }
                    }
                    doc_id = postings.advance();
                }
            }
            let Some(address) = address else {
                continue;
            };
            let segment = segments.get(address.segment_ord as usize).ok_or(
                IndexError::InvalidStoredDocumentField(EVENT_RANGE_ORDER_FIELD),
            )?;
            if !selection.accepts_indexed(segment, address.doc_id, fields, source_token)? {
                continue;
            }
            let (record, actual_encoded_bytes) =
                stored_core_event_record_with_size(&self.searcher, address, fields)?;
            let actual_content_bytes = core_content_bytes(&record.core_record.content)?;
            if actual_encoded_bytes != order.encoded_core_bytes()
                || actual_content_bytes != order.content_bytes()
                || EventRangeOrderKey::for_core_record(
                    &record.core_record,
                    actual_encoded_bytes,
                    actual_content_bytes,
                )? != order
            {
                return Err(CoreEventRangeError::InvalidCursorCoordinate);
            }
            if !selection.accepts_record(&record) {
                continue;
            }
            let admitted = candidates.is_empty()
                || super::execution::core_event_page_budget_admits(
                    budget,
                    encoded_core_bytes,
                    content_bytes,
                    order.encoded_core_bytes(),
                    order.content_bytes(),
                );
            if candidates.len() >= limit || !admitted {
                return Ok((candidates, true, encoded_core_bytes, content_bytes));
            }
            encoded_core_bytes = encoded_core_bytes
                .checked_add(order.encoded_core_bytes())
                .ok_or(IndexError::CountOverflow)?;
            content_bytes = content_bytes
                .checked_add(order.content_bytes())
                .ok_or(IndexError::CountOverflow)?;
            candidates.push(record);
        }
        Ok((candidates, false, encoded_core_bytes, content_bytes))
    }

    fn indexed_event_range_candidates(
        &self,
        selection: &CoreEventRangeSelection,
        after: Option<EventRangeOrderKey>,
        limit: usize,
        budget: CoreEventPageBudget,
        fields: Fields,
        source_token: Option<&str>,
    ) -> CoreEventRangeResult<(Vec<CoreEventRecord>, bool, usize, usize)> {
        let base_query = event_range_filter_query(selection, fields, source_token)?;
        let query: Box<dyn Query> = if let Some(after) = after {
            let range = match selection.filters.direction {
                CoreEventRangeDirection::Ascending => RangeQuery::new(
                    Bound::Excluded(Term::from_field_bytes(
                        fields.event_range_order,
                        after.as_bytes(),
                    )),
                    Bound::Unbounded,
                ),
                CoreEventRangeDirection::Descending => RangeQuery::new(
                    Bound::Unbounded,
                    Bound::Excluded(Term::from_field_bytes(
                        fields.event_range_order,
                        after.as_bytes(),
                    )),
                ),
            };
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, base_query),
                (Occur::Must, Box::new(range)),
            ]))
        } else {
            base_query
        };
        let requested = limit.saturating_add(1);
        let order = match selection.filters.direction {
            CoreEventRangeDirection::Ascending => tantivy::Order::Asc,
            CoreEventRangeDirection::Descending => tantivy::Order::Desc,
        };
        let collector = TopDocs::with_limit(requested).order_by((
            tantivy::collector::sort_key::SortByBytes::for_field(EVENT_RANGE_ORDER_FIELD),
            order,
        ));
        let hits: Vec<(Option<Vec<u8>>, DocAddress)> = self
            .searcher
            .search(query.as_ref(), &collector)
            .map_err(IndexError::from)?;
        let candidates = hits
            .into_iter()
            .map(|(key, address)| {
                let key = key.ok_or(IndexError::InvalidStoredDocumentField(
                    EVENT_RANGE_ORDER_FIELD,
                ))?;
                Ok(EventRangeAddressCandidate {
                    order: EventRangeOrderKey::decode(&key)?,
                    address,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if candidates.windows(2).any(|pair| {
            pair[0].order == pair[1].order
                || (selection.filters.direction == CoreEventRangeDirection::Ascending
                    && pair[0].order > pair[1].order)
                || (selection.filters.direction == CoreEventRangeDirection::Descending
                    && pair[0].order < pair[1].order)
        }) {
            return Err(CoreEventRangeError::InvalidCursorCoordinate);
        }

        let candidate_count = candidates.len();
        let mut records = Vec::with_capacity(limit.min(candidate_count));
        let mut encoded_core_bytes = 0_usize;
        let mut content_bytes = 0_usize;
        for candidate in candidates.iter().take(limit) {
            let candidate_encoded_bytes = candidate.order.encoded_core_bytes();
            let candidate_content_bytes = candidate.order.content_bytes();
            let admitted = records.is_empty()
                || super::execution::core_event_page_budget_admits(
                    budget,
                    encoded_core_bytes,
                    content_bytes,
                    candidate_encoded_bytes,
                    candidate_content_bytes,
                );
            if !admitted {
                break;
            }
            let (record, actual_encoded_bytes) =
                stored_core_event_record_with_size(&self.searcher, candidate.address, fields)?;
            let actual_content_bytes = core_content_bytes(&record.core_record.content)?;
            if actual_encoded_bytes != candidate_encoded_bytes
                || actual_content_bytes != candidate_content_bytes
                || EventRangeOrderKey::for_core_record(
                    &record.core_record,
                    actual_encoded_bytes,
                    actual_content_bytes,
                )? != candidate.order
                || !selection.accepts_record(&record)
            {
                return Err(CoreEventRangeError::InvalidCursorCoordinate);
            }
            encoded_core_bytes = encoded_core_bytes
                .checked_add(actual_encoded_bytes)
                .ok_or(IndexError::CountOverflow)?;
            content_bytes = content_bytes
                .checked_add(actual_content_bytes)
                .ok_or(IndexError::CountOverflow)?;
            records.push(record);
        }
        let has_more = records.len() < candidate_count;
        Ok((records, has_more, encoded_core_bytes, content_bytes))
    }
}

impl CoreEventRangeSelection {
    fn has_fully_indexed_filter(&self) -> bool {
        self.filters.source_identity.is_some()
            || !self.filters.providers.is_empty()
                || self.filters.history_source.is_some()
                || self.filters.provider_key.is_some()
                || self.filters.source_id.is_some()
                || self.filters.source_format.is_some()
                || self.filters.provider_session_id.is_some()
                || self.filters.session_id.is_some()
                || self.filters.parent_session_id.is_some()
                || self.filters.root_session_id.is_some()
                || self.filters.branch.is_some()
                || self.filters.workspace.is_some()
                || self.filters.event_type.is_some()
                || self.filters.role.is_some()
                || self.filters.agent_type.is_some()
                || self.filters.scope != CoreEventRangeScope::All
                || self.filters.file.is_some()
    }
}

fn event_range_filter_query(
    selection: &CoreEventRangeSelection,
    fields: Fields,
    source_token: Option<&str>,
) -> Result<Box<dyn Query>> {
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    if let CoreEventRangeDomain::Timestamped {
        since_unix_ms,
        until_unix_ms,
    } = selection.domain
    {
        add_filter_clause(
            &mut clauses,
            Box::new(RangeQuery::new(
                Bound::Included(Term::from_field_i64(
                    fields.occurred_at_unix_ms,
                    since_unix_ms,
                )),
                Bound::Excluded(Term::from_field_i64(
                    fields.occurred_at_unix_ms,
                    until_unix_ms,
                )),
            )),
        );
    }
    if let Some(source_token) = source_token {
        add_filter_clause(
            &mut clauses,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.source_key, source_token),
                IndexRecordOption::Basic,
            )),
        );
    }
    if !selection.filters.providers.is_empty() {
        add_filter_clause(
            &mut clauses,
            Box::new(TermSetQuery::new(
                selection
                    .filters
                    .providers
                    .iter()
                    .map(|provider| Term::from_field_text(fields.provider, provider))
                    .collect::<Vec<_>>(),
            )),
        );
    }
    for (field, field_name, value) in [
        (
            fields.source_format,
            "source_format",
            selection.filters.source_format.as_deref(),
        ),
        (
            fields.provider_session_id,
            "provider_session_id",
            selection.filters.provider_session_id.as_deref(),
        ),
        (fields.branch, "branch", selection.filters.branch.as_deref()),
        (
            fields.event_type,
            "event_type",
            selection.filters.event_type.as_deref(),
        ),
        (fields.role, "role", selection.filters.role.as_deref()),
        (
            fields.agent_type,
            "agent_type",
            selection.filters.agent_type.as_deref(),
        ),
        (
            fields.custom_provider_key,
            "provider_key",
            selection.filters.provider_key.as_deref(),
        ),
        (
            fields.custom_source_id,
            "source_id",
            selection.filters.source_id.as_deref(),
        ),
    ] {
        add_optional_text_filter(&mut clauses, field, field_name, value)?;
    }
    add_optional_uuid_filter(
        &mut clauses,
        fields.session_id,
        selection.filters.session_id,
    );
    add_optional_uuid_filter(
        &mut clauses,
        fields.parent_session_id,
        selection.filters.parent_session_id,
    );
    add_optional_uuid_filter(
        &mut clauses,
        fields.root_session_id,
        selection.filters.root_session_id,
    );
    if let Some((provider_key, source_id)) = &selection.history_source_parts {
        add_optional_text_filter(
            &mut clauses,
            fields.custom_provider_key,
            "history_source",
            Some(provider_key),
        )?;
        add_optional_text_filter(
            &mut clauses,
            fields.custom_source_id,
            "history_source",
            Some(source_id),
        )?;
    }
    if selection.filters.scope != CoreEventRangeScope::All {
        let expected = u64::from(selection.filters.scope == CoreEventRangeScope::Primary);
        add_filter_clause(
            &mut clauses,
            Box::new(TermQuery::new(
                Term::from_field_u64(fields.is_primary, expected),
                IndexRecordOption::Basic,
            )),
        );
    }
    if let Some(workspace) = selection.filters.workspace.as_deref() {
        add_filter_clause(
            &mut clauses,
            Box::new(metadata_contains_query(
                fields.workspace_filter,
                "workspace",
                workspace,
            )?),
        );
    }
    if let Some(file) = selection.filters.file.as_deref() {
        add_filter_clause(
            &mut clauses,
            Box::new(metadata_contains_query(
                fields.touched_file_filter,
                "file",
                file,
            )?),
        );
    }
    if clauses.is_empty() {
        Ok(Box::new(AllQuery))
    } else {
        Ok(Box::new(BooleanQuery::new(clauses)))
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

fn indexed_term_matches(
    segment: &SegmentReader,
    doc: DocId,
    field: tantivy::schema::Field,
    term: &Term,
) -> Result<bool> {
    let inverted = segment.inverted_index(field)?;
    let Some(mut postings) = inverted.read_postings(term, IndexRecordOption::Basic)? else {
        return Ok(false);
    };
    Ok(postings.seek(doc) == doc)
}

fn selection_digest(
    domain: CoreEventRangeDomain,
    filters: &CoreEventRangeFilters,
) -> [u8; 32] {
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

fn cursor_checksum(payload: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(CURSOR_DOMAIN);
    digest.update(payload);
    digest.finalize().into()
}

#[cfg(test)]
#[path = "event_range/tests.rs"]
mod tests;

use super::*;

use sha2::{Digest, Sha256};

use crate::is_generation_id;

const EVENT_RANGE_CURSOR_VERSION: u8 = 1;
const EVENT_RANGE_CURSOR_BYTES: usize = 161;
pub const MAX_CORE_EVENT_RANGE_PAGE_ITEMS: usize = 4_096;
const MAX_EVENT_RANGE_PROVIDERS: usize = 64;

const MAX_PROVIDER_FILTER_BYTES: usize = 256;
const CURSOR_PAYLOAD_BYTES: usize = EVENT_RANGE_CURSOR_BYTES - 32;
const CURSOR_DOMAIN: &[u8] = b"ctx-core-event-range-cursor-v1\0";
const SELECTION_DOMAIN: &[u8] = b"ctx-core-event-range-selection-v1\0";

type EventRangeSortKey = (i64, u64, u64, u64);

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
    #[error("invalid event range provider selection")]
    InvalidProviderSelection,
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

/// Canonical half-open event selection shared by first and continuation pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventRangeSelection {
    since_unix_ms: i64,
    until_unix_ms: i64,
    providers: Vec<String>,
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
        if since_unix_ms >= until_unix_ms {
            return Err(CoreEventRangeError::InvalidRange {
                since_unix_ms,
                until_unix_ms,
            });
        }
        let mut providers = providers
            .into_iter()
            .map(Into::into)
            .map(|provider: String| provider.trim().to_owned())
            .collect::<Vec<_>>();
        if providers.iter().any(String::is_empty) {
            return Err(CoreEventRangeError::InvalidProviderSelection);
        }
        if providers
            .iter()
            .any(|provider| provider.len() > MAX_PROVIDER_FILTER_BYTES)
        {
            return Err(CoreEventRangeError::InvalidProviderSelection);
        }
        providers.sort_unstable();
        providers.dedup();
        if providers.len() > MAX_EVENT_RANGE_PROVIDERS {
            return Err(CoreEventRangeError::InvalidProviderSelection);
        }
        let digest = selection_digest(since_unix_ms, until_unix_ms, &providers);
        Ok(Self {
            since_unix_ms,
            until_unix_ms,
            providers,
            digest,
        })
    }

    fn accepts_provider(&self, provider: &str) -> bool {
        self.providers.is_empty()
            || self
                .providers
                .binary_search_by(|candidate| candidate.as_str().cmp(provider))
                .is_ok()
    }

    pub fn cursor_for(
        &self,
        generation_id: &str,
        event: &CoreEventRecord,
    ) -> CoreEventRangeResult<CoreEventRangeCursor> {
        let coordinate = CoreEventRangeCoordinate::from_event(event)?;
        if !self.accepts_coordinate(coordinate) || !self.accepts_provider(&event.provider) {
            return Err(CoreEventRangeError::InvalidCursorCoordinate);
        }
        CoreEventRangeCursor::new(generation_id, self.digest, coordinate)
    }

    fn accepts_coordinate(&self, coordinate: CoreEventRangeCoordinate) -> bool {
        (self.since_unix_ms..self.until_unix_ms).contains(&coordinate.occurred_at_unix_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CoreEventRangeCoordinate {
    occurred_at_unix_ms: i64,
    event_sequence: u64,
    event_id: Uuid,
}

impl CoreEventRangeCoordinate {
    fn from_event(event: &CoreEventRecord) -> CoreEventRangeResult<Self> {
        let occurred_at_unix_ms = event
            .occurred_at_unix_ms
            .ok_or(CoreEventRangeError::InvalidCursorCoordinate)?;
        Ok(Self {
            occurred_at_unix_ms,
            event_sequence: event.event_sequence,
            event_id: event.event_id.as_uuid(),
        })
    }

    fn from_sort_key(key: EventRangeSortKey) -> Self {
        Self {
            occurred_at_unix_ms: key.0,
            event_sequence: key.1,
            event_id: Uuid::from_u128((u128::from(key.2) << 64) | u128::from(key.3)),
        }
    }
}

/// Fixed-size, versioned, tamper-evident cursor for one immutable generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventRangeCursor {
    generation_id: String,
    selection_digest: [u8; 32],
    after: CoreEventRangeCoordinate,
}

impl CoreEventRangeCursor {
    fn new(
        generation_id: &str,
        selection_digest: [u8; 32],
        after: CoreEventRangeCoordinate,
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
        if !selection.accepts_coordinate(self.after) {
            return Err(CoreEventRangeError::InvalidCursorCoordinate);
        }
        Ok(())
    }

    pub fn encode(&self) -> [u8; EVENT_RANGE_CURSOR_BYTES] {
        let mut encoded = [0_u8; EVENT_RANGE_CURSOR_BYTES];
        encoded[0] = EVENT_RANGE_CURSOR_VERSION;
        encoded[1..65].copy_from_slice(self.generation_id.as_bytes());
        encoded[65..97].copy_from_slice(&self.selection_digest);
        encoded[97..105].copy_from_slice(&self.after.occurred_at_unix_ms.to_be_bytes());
        encoded[105..113].copy_from_slice(&self.after.event_sequence.to_be_bytes());
        encoded[113..129].copy_from_slice(self.after.event_id.as_bytes());
        let checksum = cursor_checksum(&encoded[..CURSOR_PAYLOAD_BYTES]);
        encoded[CURSOR_PAYLOAD_BYTES..].copy_from_slice(&checksum);
        encoded
    }

    pub fn decode(encoded: &[u8]) -> CoreEventRangeResult<Self> {
        if encoded.len() != EVENT_RANGE_CURSOR_BYTES {
            return Err(CoreEventRangeError::InvalidCursor);
        }
        if encoded[0] != EVENT_RANGE_CURSOR_VERSION {
            return Err(CoreEventRangeError::InvalidCursor);
        }
        if cursor_checksum(&encoded[..CURSOR_PAYLOAD_BYTES]) != encoded[CURSOR_PAYLOAD_BYTES..] {
            return Err(CoreEventRangeError::InvalidCursor);
        }
        let generation_id =
            std::str::from_utf8(&encoded[1..65]).map_err(|_| CoreEventRangeError::InvalidCursor)?;
        let selection_digest = fixed_cursor_bytes(&encoded[65..97])?;
        let occurred_at_unix_ms = i64::from_be_bytes(fixed_cursor_bytes(&encoded[97..105])?);
        let event_sequence = u64::from_be_bytes(fixed_cursor_bytes(&encoded[105..113])?);
        let event_id = Uuid::from_bytes(fixed_cursor_bytes(&encoded[113..129])?);
        Self::new(
            generation_id,
            selection_digest,
            CoreEventRangeCoordinate {
                occurred_at_unix_ms,
                event_sequence,
                event_id,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventRangePage {
    pub generation_id: String,
    pub items: Vec<CoreEventRecord>,
    pub next_cursor: Option<CoreEventRangeCursor>,
    pub terminal: bool,
}

#[derive(Debug, Clone, Copy)]
struct EventRangeAddressCandidate {
    coordinate: CoreEventRangeCoordinate,
    address: DocAddress,
}

impl VerifiedIndex {
    pub fn core_event_range_page(
        &self,
        selection: &CoreEventRangeSelection,
        cursor: Option<&CoreEventRangeCursor>,
        limit: usize,
    ) -> CoreEventRangeResult<CoreEventRangePage> {
        if !(1..=MAX_CORE_EVENT_RANGE_PAGE_ITEMS).contains(&limit) {
            return Err(CoreEventRangeError::InvalidPageSize {
                requested: limit,
                maximum: MAX_CORE_EVENT_RANGE_PAGE_ITEMS,
            });
        }
        if let Some(cursor) = cursor {
            if cursor.generation_id != self.generation_id {
                return Err(CoreEventRangeError::CursorGenerationMismatch {
                    cursor_generation: cursor.generation_id.clone(),
                    pinned_generation: self.generation_id.clone(),
                });
            }
            cursor.validate_selection(selection)?;
            self.validate_event_range_cursor(selection, cursor)?;
        }

        let fields = fields_from_schema(self.searcher.schema())?;
        let candidates = self.event_range_candidates(
            selection,
            cursor.map(|cursor| cursor.after),
            limit.saturating_add(1),
            fields,
        )?;
        let candidate_count = candidates.len();
        let mut items = Vec::with_capacity(limit.min(candidate_count));
        let mut encoded_core_bytes = 0_usize;
        let mut content_bytes = 0_usize;

        for candidate in candidates.iter().copied().take(limit) {
            let (preflight_id, candidate_encoded_bytes, candidate_content_bytes) =
                core_event_fast_preflight(&self.searcher, candidate.address)?;
            if preflight_id != candidate.coordinate.event_id {
                return Err(CoreEventRangeError::InvalidCursorCoordinate);
            }
            if !items.is_empty()
                && (!budget_admits(
                    encoded_core_bytes,
                    candidate_encoded_bytes,
                    DEFAULT_CORE_EVENT_PAGE_BUDGET.maximum_encoded_core_bytes,
                ) || !budget_admits(
                    content_bytes,
                    candidate_content_bytes,
                    DEFAULT_CORE_EVENT_PAGE_BUDGET.maximum_content_bytes,
                ))
            {
                break;
            }
            let (record, actual_encoded_bytes) =
                stored_core_event_record_with_size(&self.searcher, candidate.address, fields)?;
            let actual_content_bytes = core_content_bytes(&record.core_record.content)?;
            if actual_encoded_bytes != candidate_encoded_bytes
                || actual_content_bytes != candidate_content_bytes
                || CoreEventRangeCoordinate::from_event(&record)? != candidate.coordinate
                || !selection.accepts_provider(&record.provider)
            {
                return Err(CoreEventRangeError::InvalidCursorCoordinate);
            }
            encoded_core_bytes = encoded_core_bytes
                .checked_add(actual_encoded_bytes)
                .ok_or(IndexError::CountOverflow)?;
            content_bytes = content_bytes
                .checked_add(actual_content_bytes)
                .ok_or(IndexError::CountOverflow)?;
            items.push(record);
        }

        let terminal = items.len() == candidate_count;
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
        Ok(CoreEventRangePage {
            generation_id: self.generation_id.clone(),
            items,
            next_cursor,
            terminal,
        })
    }

    fn validate_event_range_cursor(
        &self,
        selection: &CoreEventRangeSelection,
        cursor: &CoreEventRangeCursor,
    ) -> CoreEventRangeResult<()> {
        let Some(event) = self.core_event_by_id(cursor.after.event_id)? else {
            return Err(CoreEventRangeError::InvalidCursorCoordinate);
        };
        if CoreEventRangeCoordinate::from_event(&event)? != cursor.after
            || !selection.accepts_provider(&event.provider)
        {
            return Err(CoreEventRangeError::InvalidCursorCoordinate);
        }
        Ok(())
    }

    fn event_range_candidates(
        &self,
        selection: &CoreEventRangeSelection,
        after: Option<CoreEventRangeCoordinate>,
        limit: usize,
        fields: Fields,
    ) -> CoreEventRangeResult<Vec<EventRangeAddressCandidate>> {
        validate_session_event_coordinate_fast_fields(&self.searcher)?;
        let query = event_range_query(selection, after, fields);
        let collector = TopDocs::with_limit(limit).tweak_score(event_range_score);
        type EventRangeHit = (Reverse<EventRangeSortKey>, DocAddress);
        let hits: Vec<EventRangeHit> = self
            .searcher
            .search(query.as_ref(), &collector)
            .map_err(IndexError::from)?;
        let mut candidates = hits
            .into_iter()
            .map(|(Reverse(key), address)| EventRangeAddressCandidate {
                coordinate: CoreEventRangeCoordinate::from_sort_key(key),
                address,
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| candidate.coordinate);
        if candidates
            .windows(2)
            .any(|pair| pair[0].coordinate >= pair[1].coordinate)
        {
            return Err(CoreEventRangeError::InvalidCursorCoordinate);
        }
        Ok(candidates)
    }
}

fn event_range_query(
    selection: &CoreEventRangeSelection,
    after: Option<CoreEventRangeCoordinate>,
    fields: Fields,
) -> Box<dyn Query> {
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
        Occur::Must,
        Box::new(RangeQuery::new(
            Bound::Included(Term::from_field_i64(
                fields.occurred_at_unix_ms,
                selection.since_unix_ms,
            )),
            Bound::Excluded(Term::from_field_i64(
                fields.occurred_at_unix_ms,
                selection.until_unix_ms,
            )),
        )),
    )];
    if !selection.providers.is_empty() {
        clauses.push((
            Occur::Must,
            Box::new(TermSetQuery::new(
                selection
                    .providers
                    .iter()
                    .map(|provider| Term::from_field_text(fields.provider, provider))
                    .collect::<Vec<_>>(),
            )),
        ));
    }
    if let Some(after) = after {
        let occurred = Term::from_field_i64(fields.occurred_at_unix_ms, after.occurred_at_unix_ms);
        let sequence = Term::from_field_u64(fields.event_sequence, after.event_sequence);
        let event_id = Term::from_field_text(fields.event_id, &after.event_id.to_string());
        clauses.push((
            Occur::Must,
            Box::new(BooleanQuery::union(vec![
                strict_after(occurred.clone(), []),
                strict_after(sequence.clone(), [occurred.clone()]),
                strict_after(event_id, [occurred, sequence]),
            ])),
        ));
    }
    Box::new(BooleanQuery::new(clauses))
}

fn strict_after<const N: usize>(term: Term, equal: [Term; N]) -> Box<dyn Query> {
    let mut clauses = equal
        .into_iter()
        .map(|term| Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>)
        .collect::<Vec<_>>();
    clauses.push(Box::new(RangeQuery::new(
        Bound::Excluded(term),
        Bound::Unbounded,
    )));
    Box::new(BooleanQuery::intersection(clauses))
}

fn event_range_score(
    segment: &SegmentReader,
) -> impl Fn(DocId, Score) -> Reverse<EventRangeSortKey> {
    let occurred_at = segment.fast_fields().i64(OCCURRED_AT_UNIX_MS_FIELD).ok();
    let sequence = segment
        .fast_fields()
        .u64(EVENT_SEQUENCE_FIELD)
        .ok()
        .map(|column| column.first_or_default_col(0));
    let high = segment
        .fast_fields()
        .u64(EVENT_ID_HIGH_FIELD)
        .ok()
        .map(|column| column.first_or_default_col(0));
    let low = segment
        .fast_fields()
        .u64(EVENT_ID_LOW_FIELD)
        .ok()
        .map(|column| column.first_or_default_col(0));
    move |doc, _score| {
        Reverse((
            occurred_at
                .as_ref()
                .and_then(|column| column.first(doc))
                .unwrap_or(i64::MIN),
            sequence.as_ref().map_or(0, |column| column.get_val(doc)),
            high.as_ref().map_or(0, |column| column.get_val(doc)),
            low.as_ref().map_or(0, |column| column.get_val(doc)),
        ))
    }
}

fn budget_admits(retained: usize, candidate: usize, maximum: usize) -> bool {
    retained
        .checked_add(candidate)
        .is_some_and(|total| total <= maximum)
}

fn selection_digest(since: i64, until: i64, providers: &[String]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SELECTION_DOMAIN);
    digest.update(since.to_be_bytes());
    digest.update(until.to_be_bytes());
    digest.update((providers.len() as u64).to_be_bytes());
    for provider in providers {
        digest.update((provider.len() as u64).to_be_bytes());
        digest.update(provider.as_bytes());
    }
    digest.finalize().into()
}

fn cursor_checksum(payload: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(CURSOR_DOMAIN);
    digest.update(payload);
    digest.finalize().into()
}

fn fixed_cursor_bytes<const N: usize>(bytes: &[u8]) -> CoreEventRangeResult<[u8; N]> {
    bytes
        .try_into()
        .map_err(|_| CoreEventRangeError::InvalidCursor)
}

#[cfg(test)]
#[path = "event_range/tests.rs"]
mod tests;

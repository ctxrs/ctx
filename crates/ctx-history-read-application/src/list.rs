use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::DateTime;
use ctx_history_core::MAX_CORE_CONTENT_BYTES;
use ctx_history_index_query::{
    CoreEventPageBudget, CoreEventRangeCursor, CoreEventRangeError, CoreEventRangeFilters,
    CoreEventRangePage, CoreEventRangeSelection, IndexError, VerifiedIndex,
};
use uuid::Uuid;

use crate::generation::PinnedGenerationRead;
use crate::{
    GenerationReadError, GenerationReadPort, GenerationReadReceipt, GenerationReadRequest,
    GenerationReadTarget, PinnedHistoryQuery, RetainedPeerRead,
};

pub const DEFAULT_EVENT_QUERY_LIMIT: u64 = 10_000;
pub const MAX_EVENT_QUERY_LIMIT: u64 = 10_000_000;
pub const MAX_EVENT_QUERY_CURSOR_CHARS: usize = 512;

#[derive(Debug, Clone)]
pub struct ListEventsRequest {
    pub since: Option<String>,
    pub until: Option<String>,
    pub filters: CoreEventRangeFilters,
    pub cursor: Option<CoreEventRangeCursor>,
    pub limit: u64,
    pub page_items: usize,
    pub byte_budget: usize,
    pub strict_budget: Option<CoreEventPageBudget>,
}

#[derive(Debug)]
pub struct ListEventsResult {
    pub selection: CoreEventRangeSelection,
    pub limit: usize,
    pub page: CoreEventRangePage,
}

#[derive(Debug, Clone)]
pub struct ListEventsPageRequest {
    pub selection: CoreEventRangeSelection,
    pub cursor: Option<CoreEventRangeCursor>,
    pub limit: u64,
    pub page_items: usize,
    pub byte_budget: usize,
    pub strict_budget: Option<CoreEventPageBudget>,
}

#[derive(Debug, thiserror::Error)]
pub enum ListEventsError {
    #[error(transparent)]
    Range(#[from] CoreEventRangeError),
    #[error("{field} must be an absolute RFC3339 timestamp: {value:?}")]
    InvalidTimestamp { field: &'static str, value: String },
    #[error("{field} must resolve exactly to a whole millisecond: {value:?}")]
    InvalidTimestampPrecision { field: &'static str, value: String },
    #[error("since and until must be supplied together")]
    IncompleteTimestampRange,
    #[error("{field} must be a full UUID: {value:?}")]
    InvalidUuid { field: &'static str, value: String },
    #[error("{field} value {requested} is outside {minimum}..={maximum}")]
    InvalidResourceLimit {
        field: &'static str,
        requested: u64,
        minimum: u64,
        maximum: u64,
    },
    #[error("event query cursor is {actual} characters, maximum {maximum}")]
    CursorTooLarge { actual: usize, maximum: usize },
    #[error("event query cursor is not canonical base64url")]
    InvalidCursorEncoding,
    #[error("nonterminal Core event page omitted its continuation cursor")]
    MissingContinuationCursor,
    #[error("nonterminal Core event page made no progress")]
    NonAdvancingPage,
}

#[derive(Debug)]
pub enum ListEventsApplicationError<GenerationError, StreamError = std::convert::Infallible> {
    Generation(GenerationReadError<GenerationError>),
    Query(ListEventsError),
    Stream(StreamError),
}

pub struct ListEventsApplicationResult {
    generation: PinnedGenerationRead,
    result: ListEventsResult,
}

impl ListEventsApplicationResult {
    pub fn receipt(&self) -> GenerationReadReceipt<'_> {
        self.generation.receipt()
    }

    pub fn archive_provenance(&self) -> &crate::ArchiveProvenanceMap {
        self.generation.archive_provenance()
    }

    pub const fn index(&self) -> &VerifiedIndex {
        self.generation.index()
    }

    pub const fn result(&self) -> &ListEventsResult {
        &self.result
    }
}

fn generation_target(cursor: Option<&CoreEventRangeCursor>) -> GenerationReadTarget {
    cursor
        .map(|cursor| GenerationReadTarget::Exact(cursor.generation_id().to_owned()))
        .unwrap_or(GenerationReadTarget::Active)
}

fn validate_page_request(request: &ListEventsPageRequest) -> Result<(), ListEventsError> {
    if let Some(cursor) = request.cursor.as_ref() {
        cursor.validate_selection(&request.selection)?;
    }
    validated_event_limit(request.limit)?;
    Ok(())
}

pub fn execute_list_events_page<Generation: GenerationReadPort>(
    request: ListEventsPageRequest,
    generation_port: &mut Generation,
) -> std::result::Result<ListEventsApplicationResult, ListEventsApplicationError<Generation::Error>>
{
    validate_page_request(&request).map_err(ListEventsApplicationError::Query)?;
    let target = generation_target(request.cursor.as_ref());
    let generation = PinnedGenerationRead::open(
        generation_port,
        GenerationReadRequest {
            target,
            retained_peer: RetainedPeerRead::Omit,
        },
    )
    .map_err(ListEventsApplicationError::Generation)?;
    let result = PinnedHistoryQuery::new(generation.index(), None)
        .list_events_page(&request)
        .map_err(ListEventsApplicationError::Query)?;
    Ok(ListEventsApplicationResult { generation, result })
}

pub struct ListEventsStreamPage<'read> {
    pub archive_provenance: &'read crate::ArchiveProvenanceMap,
    pub index: &'read VerifiedIndex,
    pub ordinal: usize,
    pub page: &'read CoreEventRangePage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListEventsStreamControl {
    Continue,
    Stop,
}

pub struct ListEventsStreamCompletion<'read> {
    pub index: &'read VerifiedIndex,
    pub generation_id: &'read str,
    pub next_cursor: Option<&'read str>,
    pub terminal: bool,
    pub truncated: bool,
    pub items: usize,
    pub pages: usize,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub oversized_singleton_pages: usize,
}

pub trait ListEventsStreamCallback {
    type Error;

    fn page(
        &mut self,
        page: ListEventsStreamPage<'_>,
    ) -> Result<ListEventsStreamControl, Self::Error>;

    fn complete(&mut self, completion: ListEventsStreamCompletion<'_>) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListEventsStreamResult {
    pub items: usize,
    pub pages: usize,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub oversized_singleton_pages: usize,
    pub terminal: bool,
    pub truncated: bool,
}

pub fn execute_list_events_stream<Generation, Stream>(
    mut request: ListEventsPageRequest,
    generation_port: &mut Generation,
    stream: &mut Stream,
) -> std::result::Result<
    ListEventsStreamResult,
    ListEventsApplicationError<Generation::Error, Stream::Error>,
>
where
    Generation: GenerationReadPort,
    Stream: ListEventsStreamCallback,
{
    validate_page_request(&request).map_err(ListEventsApplicationError::Query)?;
    let target = generation_target(request.cursor.as_ref());
    let generation = PinnedGenerationRead::open(
        generation_port,
        GenerationReadRequest {
            target,
            retained_peer: RetainedPeerRead::Omit,
        },
    )
    .map_err(ListEventsApplicationError::Generation)?;
    let query = PinnedHistoryQuery::new(generation.index(), None);
    let limit = validated_event_limit(request.limit).map_err(ListEventsApplicationError::Query)?;
    let mut items = 0_usize;
    let mut pages = 0_usize;
    let mut encoded_core_bytes = 0_usize;
    let mut content_bytes = 0_usize;
    let mut oversized_singleton_pages = 0_usize;

    let (terminal, truncated, next_cursor) = loop {
        let remaining = limit.saturating_sub(items);
        request.page_items = request.page_items.min(remaining);
        let result = query
            .list_events_page(&request)
            .map_err(ListEventsApplicationError::Query)?;
        let page = result.page;
        pages = checked_add(pages, 1).map_err(ListEventsApplicationError::Query)?;
        oversized_singleton_pages = checked_add(
            oversized_singleton_pages,
            usize::from(page.oversized_singleton),
        )
        .map_err(ListEventsApplicationError::Query)?;
        if page.items.is_empty() && !page.terminal {
            return Err(ListEventsApplicationError::Query(
                ListEventsError::NonAdvancingPage,
            ));
        }
        let page_items = page.items.len();
        let control = stream
            .page(ListEventsStreamPage {
                archive_provenance: generation.archive_provenance(),
                index: generation.index(),
                ordinal: items,
                page: &page,
            })
            .map_err(ListEventsApplicationError::Stream)?;
        items = checked_add(items, page_items).map_err(ListEventsApplicationError::Query)?;
        encoded_core_bytes = checked_add(encoded_core_bytes, page.encoded_core_bytes)
            .map_err(ListEventsApplicationError::Query)?;
        content_bytes = checked_add(content_bytes, page.content_bytes)
            .map_err(ListEventsApplicationError::Query)?;
        let terminal = page.terminal;
        let next_cursor = page.next_cursor;
        let limit_reached = items >= limit;
        if terminal || limit_reached || control == ListEventsStreamControl::Stop {
            let truncated =
                !terminal && (limit_reached || control == ListEventsStreamControl::Stop);
            break (terminal, truncated, next_cursor);
        }
        request.cursor = Some(next_cursor.ok_or(ListEventsApplicationError::Query(
            ListEventsError::MissingContinuationCursor,
        ))?);
    };

    let encoded_next_cursor = next_cursor.as_ref().map(encode_event_range_cursor);
    stream
        .complete(ListEventsStreamCompletion {
            index: generation.index(),
            generation_id: generation.index().generation_id(),
            next_cursor: encoded_next_cursor.as_deref(),
            terminal,
            truncated,
            items,
            pages,
            encoded_core_bytes,
            content_bytes,
            oversized_singleton_pages,
        })
        .map_err(ListEventsApplicationError::Stream)?;
    Ok(ListEventsStreamResult {
        items,
        pages,
        encoded_core_bytes,
        content_bytes,
        oversized_singleton_pages,
        terminal,
        truncated,
    })
}

fn checked_add(left: usize, right: usize) -> Result<usize, ListEventsError> {
    left.checked_add(right)
        .ok_or(ListEventsError::Range(CoreEventRangeError::Index(
            IndexError::CountOverflow,
        )))
}

impl PinnedHistoryQuery<'_> {
    pub fn list_events(
        &self,
        request: &ListEventsRequest,
    ) -> Result<ListEventsResult, ListEventsError> {
        let selection = event_range_selection(
            request.since.as_deref(),
            request.until.as_deref(),
            request.filters.clone(),
        )?;
        self.list_events_page(&ListEventsPageRequest {
            selection,
            cursor: request.cursor.clone(),
            limit: request.limit,
            page_items: request.page_items,
            byte_budget: request.byte_budget,
            strict_budget: request.strict_budget,
        })
    }

    pub fn list_events_page(
        &self,
        request: &ListEventsPageRequest,
    ) -> Result<ListEventsResult, ListEventsError> {
        if let Some(cursor) = request.cursor.as_ref() {
            cursor.validate_selection(&request.selection)?;
        }
        let limit = validated_event_limit(request.limit)?;
        let page_items = request.page_items.min(limit);
        let budget = CoreEventPageBudget::new(
            request.byte_budget,
            request.byte_budget.min(MAX_CORE_CONTENT_BYTES),
        );
        let page = match request.strict_budget {
            Some(strict_budget) => self.index.core_event_range_page_with_strict_budget(
                &request.selection,
                request.cursor.as_ref(),
                page_items,
                budget,
                strict_budget,
            ),
            None => self.index.core_event_range_page_with_budget(
                &request.selection,
                request.cursor.as_ref(),
                page_items,
                budget,
            ),
        }?;
        Ok(ListEventsResult {
            selection: request.selection.clone(),
            limit,
            page,
        })
    }
}

pub fn event_range_selection(
    since: Option<&str>,
    until: Option<&str>,
    filters: CoreEventRangeFilters,
) -> Result<CoreEventRangeSelection, ListEventsError> {
    match (since, until) {
        (None, None) => CoreEventRangeSelection::all(filters).map_err(Into::into),
        (Some(since), Some(until)) => CoreEventRangeSelection::with_filters(
            parse_rfc3339("since", since)?,
            parse_rfc3339("until", until)?,
            filters,
        )
        .map_err(Into::into),
        (Some(_), None) | (None, Some(_)) => Err(ListEventsError::IncompleteTimestampRange),
    }
}

pub fn validated_event_limit(limit: u64) -> Result<usize, ListEventsError> {
    if !(1..=MAX_EVENT_QUERY_LIMIT).contains(&limit) {
        return Err(ListEventsError::InvalidResourceLimit {
            field: "limit",
            requested: limit,
            minimum: 1,
            maximum: MAX_EVENT_QUERY_LIMIT,
        });
    }
    usize::try_from(limit).map_err(|_| ListEventsError::InvalidResourceLimit {
        field: "limit",
        requested: limit,
        minimum: 1,
        maximum: MAX_EVENT_QUERY_LIMIT,
    })
}

pub fn decode_event_range_cursor(encoded: &str) -> Result<CoreEventRangeCursor, ListEventsError> {
    if encoded.len() > MAX_EVENT_QUERY_CURSOR_CHARS {
        return Err(ListEventsError::CursorTooLarge {
            actual: encoded.len(),
            maximum: MAX_EVENT_QUERY_CURSOR_CHARS,
        });
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ListEventsError::InvalidCursorEncoding)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err(ListEventsError::InvalidCursorEncoding);
    }
    Ok(CoreEventRangeCursor::decode(&bytes)?)
}

pub fn encode_event_range_cursor(cursor: &CoreEventRangeCursor) -> String {
    URL_SAFE_NO_PAD.encode(cursor.encode())
}

pub fn parse_event_query_uuid(
    field: &'static str,
    value: Option<&str>,
) -> Result<Option<Uuid>, ListEventsError> {
    value
        .map(|value| {
            Uuid::parse_str(value).map_err(|_| ListEventsError::InvalidUuid {
                field,
                value: value.to_owned(),
            })
        })
        .transpose()
}

fn parse_rfc3339(field: &'static str, value: &str) -> Result<i64, ListEventsError> {
    let timestamp =
        DateTime::parse_from_rfc3339(value).map_err(|_| ListEventsError::InvalidTimestamp {
            field,
            value: value.to_owned(),
        })?;
    if timestamp.timestamp_subsec_nanos() % 1_000_000 != 0 {
        return Err(ListEventsError::InvalidTimestampPrecision {
            field,
            value: value.to_owned(),
        });
    }
    Ok(timestamp.timestamp_millis())
}

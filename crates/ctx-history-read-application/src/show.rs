use std::{collections::VecDeque, fmt};

use anyhow::{anyhow, Result};
use ctx_history_core::{
    CaptureProvider, EventType, MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES,
};
use ctx_history_index_query::{
    CopiedEventLineage, CoreEventPageBudget, CoreEventRecord, SessionEventCoordinate,
    SessionEventCursor, SessionRecord, MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
    SHOW_COPIED_EVENT_LINEAGE_POLICY,
};
use uuid::Uuid;

use crate::generation::PinnedGenerationRead;
use crate::{
    event_window_with_lineage_read_model, paginated_session_transcript_read_model,
    reference_needs_retained_peer, resolve_core_event_with_refs, resolve_show_session_with_refs,
    retain_structured_session_page, CompactPresentationProjection, GenerationReadError,
    GenerationReadPort, GenerationReadReceipt, GenerationReadRequest, GenerationReadTarget,
    PinnedHistoryQuery, RetainedPeerRead, StructuredOutputFormat, StructuredTranscriptMode,
};

const QUERY_FETCH_MAX_EVENTS: usize = 200;
pub const SHOW_SESSION_PAGE_ITEMS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEventMode {
    Full,
    Lite,
    Log,
}

#[derive(Debug, Clone)]
pub struct ShowEventRequest {
    pub selector: String,
    pub before: usize,
    pub after: usize,
    pub window: Option<usize>,
    pub budget: EventWindowBudget,
}

#[derive(Debug, Clone, Copy)]
pub struct EventWindowBudget {
    pub maximum_events: usize,
    pub maximum_encoded_core_bytes: usize,
    pub maximum_content_bytes: usize,
}

impl Default for EventWindowBudget {
    fn default() -> Self {
        Self {
            maximum_events: MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
            maximum_encoded_core_bytes: MAX_ENCODED_CORE_RECORD_BYTES,
            maximum_content_bytes: MAX_CORE_CONTENT_BYTES,
        }
    }
}

#[derive(Debug)]
pub struct ShowEventResult {
    pub selected: CoreEventRecord,
    pub events: Vec<CoreEventRecord>,
    pub copied_lineage: CopiedEventLineage,
}

#[derive(Debug, Clone)]
pub struct ShowSessionPageRequest {
    pub selector: Option<String>,
    pub provider_session_id: Option<String>,
    pub provider: Option<CaptureProvider>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub mode: SessionEventMode,
    pub cursor: Option<SessionEventCursor>,
    pub limit: usize,
    pub page_items: usize,
    pub page_budget: CoreEventPageBudget,
}

#[derive(Debug)]
pub struct ShowSessionEvent {
    pub event: CoreEventRecord,
    pub cursor_after: SessionEventCursor,
}

#[derive(Debug)]
pub struct ShowSessionPage {
    pub session: SessionRecord,
    pub events: Vec<ShowSessionEvent>,
    pub has_more: bool,
    pub next_cursor: Option<SessionEventCursor>,
}

pub struct ShowEventApplicationRequest {
    pub request: ShowEventRequest,
    pub generation_target: GenerationReadTarget,
    pub compact_projection: bool,
}

impl ShowEventApplicationRequest {
    fn retained_peer_read(&self) -> RetainedPeerRead {
        if self.compact_projection || reference_needs_retained_peer(&self.request.selector) {
            RetainedPeerRead::IfAvailable
        } else {
            RetainedPeerRead::Omit
        }
    }
}

pub struct ShowEventApplicationResult {
    generation: PinnedGenerationRead,
    result: ShowEventResult,
}

impl ShowEventApplicationResult {
    pub fn receipt(&self) -> GenerationReadReceipt<'_> {
        self.generation.receipt()
    }

    pub const fn result(&self) -> &ShowEventResult {
        &self.result
    }

    pub fn read_model(
        &self,
        format: StructuredOutputFormat,
        output_limit_bytes: usize,
    ) -> Result<serde_json::Value> {
        event_window_with_lineage_read_model(
            &self.result.selected,
            &self.result.events,
            &self.result.copied_lineage,
            format,
            output_limit_bytes,
        )
    }

    pub fn project_read_model(&self, value: &serde_json::Value) -> Result<serde_json::Value> {
        CompactPresentationProjection::new(self.generation.index(), self.generation.retained_peer())
            .project(value)
    }
}

#[derive(Debug)]
pub enum ShowReadApplicationError<GenerationError, StreamError = std::convert::Infallible> {
    Generation(GenerationReadError<GenerationError>),
    Query(anyhow::Error),
    Stream(StreamError),
}

pub fn execute_show_event<Generation: GenerationReadPort>(
    request: ShowEventApplicationRequest,
    generation_port: &mut Generation,
) -> std::result::Result<ShowEventApplicationResult, ShowReadApplicationError<Generation::Error>> {
    let retained_peer = request.retained_peer_read();
    let generation = PinnedGenerationRead::open(
        generation_port,
        GenerationReadRequest {
            target: request.generation_target,
            retained_peer,
        },
    )
    .map_err(ShowReadApplicationError::Generation)?;
    let result = PinnedHistoryQuery::new(generation.index(), generation.retained_peer())
        .show_event(&request.request)
        .map_err(ShowReadApplicationError::Query)?;
    Ok(ShowEventApplicationResult { generation, result })
}

#[derive(Debug, Clone)]
pub struct ShowSessionApplicationRequest {
    pub selector: Option<String>,
    pub provider_session_id: Option<String>,
    pub provider: Option<CaptureProvider>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub mode: SessionEventMode,
    pub cursor: Option<String>,
    pub limit: usize,
    pub page_items: usize,
    pub page_budget: CoreEventPageBudget,
    pub compact_projection: bool,
}

impl ShowSessionApplicationRequest {
    fn retained_peer_read(&self) -> RetainedPeerRead {
        if self.compact_projection
            || self
                .selector
                .as_deref()
                .is_some_and(reference_needs_retained_peer)
        {
            RetainedPeerRead::IfAvailable
        } else {
            RetainedPeerRead::Omit
        }
    }

    fn decode_cursor(&self) -> Result<Option<SessionEventCursor>> {
        self.cursor
            .as_deref()
            .map(crate::decode_session_event_cursor)
            .transpose()
    }
}

pub struct ShowSessionApplicationResult {
    generation: PinnedGenerationRead,
    page: ShowSessionPage,
    compact_projection: bool,
}

pub struct ShowSessionReadModels {
    pub value: serde_json::Value,
    pub compact_value: Option<serde_json::Value>,
}

impl ShowSessionApplicationResult {
    pub fn receipt(&self) -> GenerationReadReceipt<'_> {
        self.generation.receipt()
    }

    pub const fn page(&self) -> &ShowSessionPage {
        &self.page
    }

    pub fn into_read_models(
        self,
        mode: StructuredTranscriptMode,
        format: StructuredOutputFormat,
        limit: usize,
        output_limit_bytes: usize,
    ) -> Result<ShowSessionReadModels> {
        let retained = retain_structured_session_page(
            self.page.events,
            self.page.has_more,
            output_limit_bytes,
        )?;
        let value = paginated_session_transcript_read_model(
            &self.page.session,
            mode,
            format,
            retained.events,
            limit,
            retained.has_more,
            retained.next_cursor.as_ref(),
        )?;
        let compact_value = self
            .compact_projection
            .then(|| {
                CompactPresentationProjection::new(
                    self.generation.index(),
                    self.generation.retained_peer(),
                )
                .project(&value)
            })
            .transpose()?;
        Ok(ShowSessionReadModels {
            value,
            compact_value,
        })
    }
}

pub fn execute_show_session_page<Generation: GenerationReadPort>(
    request: ShowSessionApplicationRequest,
    generation_port: &mut Generation,
) -> std::result::Result<ShowSessionApplicationResult, ShowReadApplicationError<Generation::Error>>
{
    let cursor = request
        .decode_cursor()
        .map_err(ShowReadApplicationError::Query)?;
    let target = cursor
        .as_ref()
        .map(|cursor| GenerationReadTarget::Exact(cursor.generation_id().to_owned()))
        .unwrap_or(GenerationReadTarget::Active);
    let retained_peer = request.retained_peer_read();
    let generation = PinnedGenerationRead::open(
        generation_port,
        GenerationReadRequest {
            target,
            retained_peer,
        },
    )
    .map_err(ShowReadApplicationError::Generation)?;
    let page = PinnedHistoryQuery::new(generation.index(), generation.retained_peer())
        .show_session_page(&ShowSessionPageRequest {
            selector: request.selector,
            provider_session_id: request.provider_session_id,
            provider: request.provider,
            provider_key: request.provider_key,
            source_id: request.source_id,
            mode: request.mode,
            cursor,
            limit: request.limit,
            page_items: request.page_items,
            page_budget: request.page_budget,
        })
        .map_err(ShowReadApplicationError::Query)?;
    Ok(ShowSessionApplicationResult {
        generation,
        page,
        compact_projection: request.compact_projection,
    })
}

#[derive(Debug, Clone)]
pub struct ShowSessionStreamRequest {
    pub selector: Option<String>,
    pub provider_session_id: Option<String>,
    pub provider: Option<CaptureProvider>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub mode: SessionEventMode,
    pub cursor: Option<String>,
    pub max_events: Option<usize>,
    pub page_items: usize,
    pub page_budget: CoreEventPageBudget,
    pub compact_projection: bool,
}

#[derive(Clone, Copy)]
pub struct ShowReadModelProjection<'index> {
    current: &'index ctx_history_index_query::VerifiedIndex,
    retained_peer: Option<&'index ctx_history_index_query::VerifiedIndex>,
    enabled: bool,
}

impl ShowReadModelProjection<'_> {
    pub fn project(&self, value: &serde_json::Value) -> Result<Option<serde_json::Value>> {
        self.enabled
            .then(|| {
                CompactPresentationProjection::new(self.current, self.retained_peer).project(value)
            })
            .transpose()
    }
}

pub struct ShowSessionStreamStart<'read> {
    pub session: &'read SessionRecord,
    pub projection: ShowReadModelProjection<'read>,
}

pub struct ShowSessionStreamPage<'read> {
    pub events: Vec<ShowSessionEvent>,
    pub projection: ShowReadModelProjection<'read>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowSessionStreamControl {
    Continue,
    Stop,
}

pub trait ShowSessionStreamCallback {
    type Error;

    fn start(&mut self, start: ShowSessionStreamStart<'_>) -> Result<(), Self::Error>;

    fn page(
        &mut self,
        page: ShowSessionStreamPage<'_>,
    ) -> Result<ShowSessionStreamControl, Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowSessionStreamResult {
    pub events_returned: usize,
    pub truncated: bool,
}

pub fn execute_show_session_stream<Generation, Stream>(
    request: ShowSessionStreamRequest,
    generation_port: &mut Generation,
    stream: &mut Stream,
) -> std::result::Result<
    ShowSessionStreamResult,
    ShowReadApplicationError<Generation::Error, Stream::Error>,
>
where
    Generation: GenerationReadPort,
    Stream: ShowSessionStreamCallback,
{
    let cursor = request
        .cursor
        .as_deref()
        .map(crate::decode_session_event_cursor)
        .transpose()
        .map_err(ShowReadApplicationError::Query)?;
    let target = cursor
        .as_ref()
        .map(|cursor| GenerationReadTarget::Exact(cursor.generation_id().to_owned()))
        .unwrap_or(GenerationReadTarget::Active);
    let retained_peer = if request.compact_projection
        || request
            .selector
            .as_deref()
            .is_some_and(reference_needs_retained_peer)
    {
        RetainedPeerRead::IfAvailable
    } else {
        RetainedPeerRead::Omit
    };
    let generation = PinnedGenerationRead::open(
        generation_port,
        GenerationReadRequest {
            target,
            retained_peer,
        },
    )
    .map_err(ShowReadApplicationError::Generation)?;
    let query = PinnedHistoryQuery::new(generation.index(), generation.retained_peer());
    let session = query
        .show_session(
            request.selector.as_deref(),
            request.provider_session_id.as_deref(),
            request.provider,
            request.provider_key.as_deref(),
            request.source_id.as_deref(),
        )
        .map_err(ShowReadApplicationError::Query)?;
    let projection = ShowReadModelProjection {
        current: generation.index(),
        retained_peer: generation.retained_peer(),
        enabled: request.compact_projection,
    };
    stream
        .start(ShowSessionStreamStart {
            session: &session,
            projection,
        })
        .map_err(ShowReadApplicationError::Stream)?;

    let mut cursor = cursor;
    let mut events_returned = 0_usize;
    let mut truncated = false;
    loop {
        let remaining = request
            .max_events
            .map(|maximum| maximum.saturating_sub(events_returned))
            .unwrap_or(request.page_items);
        if remaining == 0 {
            truncated = true;
            break;
        }
        let page = query
            .show_session_page(&ShowSessionPageRequest {
                selector: Some(session.session_id.to_string()),
                provider_session_id: None,
                provider: None,
                provider_key: None,
                source_id: None,
                mode: request.mode,
                cursor: cursor.clone(),
                limit: remaining.min(request.page_items),
                page_items: request.page_items,
                page_budget: request.page_budget,
            })
            .map_err(ShowReadApplicationError::Query)?;
        let page_events = page.events.len();
        let control = stream
            .page(ShowSessionStreamPage {
                events: page.events,
                projection,
            })
            .map_err(ShowReadApplicationError::Stream)?;
        events_returned = events_returned.saturating_add(page_events);
        if control == ShowSessionStreamControl::Stop {
            truncated = page.has_more;
            break;
        }
        if !page.has_more {
            break;
        }
        if request
            .max_events
            .is_some_and(|maximum| events_returned >= maximum)
        {
            truncated = true;
            break;
        }
        cursor = Some(page.next_cursor.ok_or_else(|| {
            ShowReadApplicationError::Query(anyhow!(
                "nonterminal Core session event page omitted its continuation cursor"
            ))
        })?);
    }
    Ok(ShowSessionStreamResult {
        events_returned,
        truncated,
    })
}

impl PinnedHistoryQuery<'_> {
    pub fn show_event(&self, request: &ShowEventRequest) -> Result<ShowEventResult> {
        let selected = resolve_core_event_with_refs(&self.references, &request.selector)?;
        let (before, after) = request
            .window
            .map(|window| (window, window))
            .unwrap_or((request.before, request.after));
        let coordinates = self
            .index
            .session_event_coordinate_window(
                selected.session_id.as_uuid(),
                selected.event_id.as_uuid(),
                before,
                after,
            )?
            .ok_or_else(|| anyhow!("selected event is absent from its pinned Core session"))?;
        let event_ids = coordinates
            .iter()
            .map(|coordinate| coordinate.event_id)
            .collect::<Vec<_>>();
        let events = core_events_by_ids_with_budget(self.index, &event_ids, request.budget)?;
        let copied_lineage = self.index.copied_event_lineage(
            selected.event_id.as_uuid(),
            SHOW_COPIED_EVENT_LINEAGE_POLICY,
        )?;
        Ok(ShowEventResult {
            selected,
            events,
            copied_lineage,
        })
    }

    pub fn show_session_page(&self, request: &ShowSessionPageRequest) -> Result<ShowSessionPage> {
        if request.page_items == 0 {
            return Err(anyhow!("session query page_items must be positive"));
        }
        let session = self.show_session(
            request.selector.as_deref(),
            request.provider_session_id.as_deref(),
            request.provider,
            request.provider_key.as_deref(),
            request.source_id.as_deref(),
        )?;
        let mut selector = SessionEventSelector::new(request.mode);
        let mut selected = Vec::with_capacity(request.limit);
        let mut cursor = request.cursor.clone();
        let mut has_more = false;

        'pages: loop {
            let page = self.index.core_session_event_page_with_budget(
                session.session_id.as_uuid(),
                cursor.as_ref(),
                request.page_items,
                request.page_budget,
            )?;
            let terminal = page.terminal;
            let next_page_cursor = page.next_cursor;
            for event in page.items {
                for event in selector.push(event) {
                    if selected.len() == request.limit {
                        has_more = true;
                        break 'pages;
                    }
                    let cursor_after = cursor_after_event(self.index, &session, &event);
                    selected.push(ShowSessionEvent {
                        event,
                        cursor_after,
                    });
                }
            }
            if terminal {
                if let Some(event) = selector.finish() {
                    if selected.len() == request.limit {
                        has_more = true;
                    } else {
                        let cursor_after = cursor_after_event(self.index, &session, &event);
                        selected.push(ShowSessionEvent {
                            event,
                            cursor_after,
                        });
                    }
                }
                break;
            }
            cursor = Some(next_page_cursor.ok_or_else(|| {
                anyhow!("nonterminal Core session event page omitted its continuation cursor")
            })?);
        }

        let next_cursor = has_more
            .then(|| selected.last().map(|event| event.cursor_after.clone()))
            .flatten();
        Ok(ShowSessionPage {
            session,
            events: selected,
            has_more,
            next_cursor,
        })
    }

    pub fn show_session(
        &self,
        selector: Option<&str>,
        provider_session_id: Option<&str>,
        provider: Option<CaptureProvider>,
        provider_key: Option<&str>,
        source_id: Option<&str>,
    ) -> Result<SessionRecord> {
        resolve_show_session_with_refs(
            &self.references,
            selector,
            provider_session_id,
            provider,
            provider_key,
            source_id,
        )
    }
}

fn cursor_after_event(
    query: &ctx_history_index_query::VerifiedIndex,
    session: &SessionRecord,
    event: &CoreEventRecord,
) -> SessionEventCursor {
    SessionEventCursor::new(
        query.generation_id(),
        session.session_id,
        SessionEventCoordinate {
            event_id: event.event_id.as_uuid(),
            event_sequence: event.event_sequence,
            occurred_at_unix_ms: event.occurred_at_unix_ms,
        },
    )
}

struct SessionEventSelector {
    mode: SessionEventMode,
    pending_assistant: Option<CoreEventRecord>,
}

impl SessionEventSelector {
    const fn new(mode: SessionEventMode) -> Self {
        Self {
            mode,
            pending_assistant: None,
        }
    }

    fn push(&mut self, event: CoreEventRecord) -> Vec<CoreEventRecord> {
        match self.mode {
            SessionEventMode::Log => vec![event],
            SessionEventMode::Full => {
                if event.event_type == EventType::Message.as_str()
                    && matches!(event.role.as_deref(), Some("user" | "assistant" | "system"))
                {
                    vec![event]
                } else {
                    Vec::new()
                }
            }
            SessionEventMode::Lite => {
                if event.event_type != EventType::Message.as_str() {
                    return Vec::new();
                }
                match event.role.as_deref() {
                    Some("user") => {
                        let mut selected = Vec::with_capacity(2);
                        if let Some(assistant) = self.pending_assistant.take() {
                            selected.push(assistant);
                        }
                        selected.push(event);
                        selected
                    }
                    Some("assistant") => {
                        self.pending_assistant = Some(event);
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
        }
    }

    fn finish(&mut self) -> Option<CoreEventRecord> {
        self.pending_assistant.take()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventWindowLimitError {
    pub actual_events: usize,
    pub maximum_events: usize,
}

impl fmt::Display for EventWindowLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Core event window selected at least {} events; the query limit is {} events",
            self.actual_events, self.maximum_events
        )
    }
}

impl std::error::Error for EventWindowLimitError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedCoreQueryLimitError {
    pub event_id: Uuid,
    pub actual_bytes: usize,
    pub maximum_bytes: usize,
}

impl fmt::Display for EncodedCoreQueryLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "stored Core encoding through event {} requires {} bytes; the query limit is {} bytes",
            self.event_id, self.actual_bytes, self.maximum_bytes
        )
    }
}

impl std::error::Error for EncodedCoreQueryLimitError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentQueryLimitError {
    pub event_id: Uuid,
    pub actual_bytes: usize,
    pub maximum_bytes: usize,
}

impl fmt::Display for ContentQueryLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Core content through event {} requires {} bytes; the query limit is {} bytes",
            self.event_id, self.actual_bytes, self.maximum_bytes
        )
    }
}

impl std::error::Error for ContentQueryLimitError {}

fn core_events_by_ids_with_budget(
    index: &ctx_history_index_query::VerifiedIndex,
    event_ids: &[Uuid],
    budget: EventWindowBudget,
) -> Result<Vec<CoreEventRecord>> {
    if event_ids.len() > budget.maximum_events {
        return Err(anyhow::Error::new(EventWindowLimitError {
            actual_events: event_ids.len(),
            maximum_events: budget.maximum_events,
        }));
    }

    let mut pending = VecDeque::new();
    for chunk in event_ids.chunks(QUERY_FETCH_MAX_EVENTS) {
        pending.push_back(chunk);
    }
    let mut events = Vec::with_capacity(event_ids.len());
    let mut retained_content_bytes = 0_usize;
    let mut retained_encoded_core_bytes = 0_usize;
    while let Some(ids) = pending.pop_front() {
        let remaining_content_bytes = budget
            .maximum_content_bytes
            .saturating_sub(retained_content_bytes)
            .clamp(1, MAX_CORE_CONTENT_BYTES);
        let remaining_encoded_core_bytes = budget
            .maximum_encoded_core_bytes
            .saturating_sub(retained_encoded_core_bytes)
            .clamp(1, MAX_ENCODED_CORE_RECORD_BYTES);
        let core_budget =
            CoreEventPageBudget::new(remaining_encoded_core_bytes, remaining_content_bytes);
        match index.core_events_by_ids_with_budget(ids, QUERY_FETCH_MAX_EVENTS, core_budget)? {
            Some(batch) => {
                let event_id = ids.last().copied().unwrap_or_else(Uuid::nil);
                let actual_encoded_core_bytes =
                    retained_encoded_core_bytes.saturating_add(batch.encoded_core_bytes);
                if actual_encoded_core_bytes > budget.maximum_encoded_core_bytes {
                    return Err(anyhow::Error::new(EncodedCoreQueryLimitError {
                        event_id,
                        actual_bytes: actual_encoded_core_bytes,
                        maximum_bytes: budget.maximum_encoded_core_bytes,
                    }));
                }
                retained_encoded_core_bytes = actual_encoded_core_bytes;
                let actual_content_bytes =
                    retained_content_bytes.saturating_add(batch.content_bytes);
                if actual_content_bytes > budget.maximum_content_bytes {
                    return Err(anyhow::Error::new(ContentQueryLimitError {
                        event_id,
                        actual_bytes: actual_content_bytes,
                        maximum_bytes: budget.maximum_content_bytes,
                    }));
                }
                retained_content_bytes = actual_content_bytes;
                events.extend(batch.items);
            }
            None if ids.len() > 1 => {
                let middle = ids.len() / 2;
                let (left, right) = ids.split_at(middle);
                pending.push_front(right);
                pending.push_front(left);
            }
            None => {
                return Err(anyhow!(
                    "pinned Core generation could not resolve event {} within the remaining query budget",
                    ids[0]
                ));
            }
        }
    }
    if events.len() != event_ids.len()
        || events
            .iter()
            .zip(event_ids)
            .any(|(event, expected)| event.event_id.as_uuid() != *expected)
    {
        return Err(anyhow!(
            "pinned Core generation did not return the requested event order"
        ));
    }
    Ok(events)
}

use std::{io::Write, path::Path, path::PathBuf};

use anyhow::Result;
use ctx_history_index::{
    CoreEventPageBudget, CoreEventRangeCursor, CoreEventRangeDirection, CoreEventRangeError,
    CoreEventRangeFilters, CoreEventRangePage, CoreEventRangeScope, CoreEventRangeSelection,
    IndexError, VerifiedIndex,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    analytics::count_bucket,
    analytics::ShowTelemetry,
    local_usage::{CliUsage, ResultObservationAction},
    ui::Ui,
};

mod render;
mod request;

pub use ctx_history_read_application::{
    mcp_event_query_core_record_bytes, DEFAULT_EVENT_QUERY_LIMIT, EVENT_QUERY_PAGE_BYTES,
    EVENT_QUERY_PAGE_ITEMS, EVENT_QUERY_SCHEMA_VERSION, MAX_EVENT_QUERY_WIRE_RECORD_BYTES,
};
pub use render::render_event;
pub use request::{
    EventContentProjection, EventContentProjectionArg, EventQueryDirection, EventQueryFormat,
    EventQueryScope, EventQueryWireRequest, ListEventsArgs,
};

#[derive(Debug, thiserror::Error)]
pub enum EventQueryError {
    #[error(transparent)]
    GenerationReadAuthority(#[from] ctx_history_read_application::GenerationReadAuthorityError),
    #[error(transparent)]
    Range(#[from] CoreEventRangeError),
    #[error(transparent)]
    Application(#[from] ctx_history_read_application::ListEventsError),
    #[error("nonterminal Core event page omitted its continuation cursor")]
    MissingContinuationCursor,
    #[error("Core event page omitted admitted usage for a retained prefix")]
    MissingPrefixUsage,
    #[error(
        "serialized event requires {actual} bytes, exceeding the conservative wire cap of {maximum}; retry with --content text or --content none"
    )]
    WireRecordTooLarge { actual: usize, maximum: usize },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Core(#[from] ctx_history_core::CoreRecordError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn run(
    args: ListEventsArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    match execute(args, &data_root, ui.stdout_writer()) {
        Ok(events) => {
            telemetry.events_returned = Some(count_bucket(events as u64));
            local_usage.set_result_observation(ResultObservationAction::OpenEvent, events, 0);
            Ok(())
        }
        Err(error) => {
            if is_broken_pipe(&error) {
                return Ok(());
            }
            writeln!(
                ui.stderr_writer(),
                "{}",
                serde_json::to_string(&event_query_error_value(&error))?
            )?;
            Err(crate::dispatch::rendered_cli_error())
        }
    }
}

fn is_broken_pipe(error: &EventQueryError) -> bool {
    matches!(error, EventQueryError::Io(error) if error.kind() == std::io::ErrorKind::BrokenPipe)
        || matches!(error, EventQueryError::Json(error) if error.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe))
}

fn execute(
    args: ListEventsArgs,
    data_root: &Path,
    writer: &mut dyn Write,
) -> std::result::Result<usize, EventQueryError> {
    let mut args = crate::ListEventsRequest::from(args);
    let cursor = args.cursor.take();
    let limit = args.limit;
    let content = match args.content {
        crate::ListEventsContentProjection::Full => EventContentProjection::Full,
        crate::ListEventsContentProjection::Text => EventContentProjection::Text,
        crate::ListEventsContentProjection::None => EventContentProjection::None,
    };
    let format = args.format;
    let selection = selection_from_request(args)?;
    let cursor = cursor.as_deref().map(decode_cursor).transpose()?;
    let limit = validated_limit(limit)?;
    let request = EventQueryWireRequest::from_selection(&selection, content, limit);
    match format {
        crate::OutputFormat::Json => {
            let mut generation = |read: &ctx_history_read_application::GenerationReadRequest| {
                open_event_range_generation(data_root, read)
            };
            let application = ctx_history_read_application::execute_list_events_page(
                list_page_request(selection, cursor, &request, None),
                &mut generation,
            )
            .map_err(list_page_application_error)?;
            let page =
                encode_bounded_page(application.index(), &application.result().page, &request)?;
            writer.write_all(&page.encoded)?;
            writer.flush()?;
            Ok(page.items)
        }
        crate::OutputFormat::Jsonl => {
            write_jsonl_pages(data_root, selection, cursor, &request, writer, || {})
        }
        _ => unreachable!("list-events execution accepts only JSON and JSONL formats"),
    }
}

pub fn validated_limit(limit: u64) -> std::result::Result<usize, EventQueryError> {
    ctx_history_read_application::validated_event_limit(limit).map_err(Into::into)
}

pub fn selection(
    since: Option<&str>,
    until: Option<&str>,
    filters: CoreEventRangeFilters,
) -> std::result::Result<CoreEventRangeSelection, EventQueryError> {
    ctx_history_read_application::event_range_selection(since, until, filters).map_err(Into::into)
}

pub fn selection_from_request(
    args: crate::ListEventsRequest,
) -> std::result::Result<CoreEventRangeSelection, EventQueryError> {
    let crate::ListEventsRequest {
        since,
        until,
        providers,
        source,
        history_source,
        provider_key,
        source_id,
        source_format,
        provider_session,
        session,
        parent_session,
        root_session,
        branch,
        workspace,
        event_type,
        role,
        file,
        scope,
        direction,
        ..
    } = args;
    selection(
        since.as_deref(),
        until.as_deref(),
        CoreEventRangeFilters {
            providers,
            source_identity: parse_uuid("source", source.as_deref())?,
            history_source,
            provider_key,
            source_id,
            source_format,
            provider_session_id: provider_session,
            session_id: parse_uuid("session", session.as_deref())?,
            parent_session_id: parse_uuid("parent_session", parent_session.as_deref())?,
            root_session_id: parse_uuid("root", root_session.as_deref())?,
            branch,
            workspace,
            event_type,
            role,
            scope: match scope {
                crate::ListEventsScope::All => CoreEventRangeScope::All,
                crate::ListEventsScope::Primary => CoreEventRangeScope::Primary,
                crate::ListEventsScope::Subagent => CoreEventRangeScope::Subagent,
            },
            file,
            direction: match direction {
                crate::ListEventsDirection::Ascending => CoreEventRangeDirection::Ascending,
                crate::ListEventsDirection::Descending => CoreEventRangeDirection::Descending,
            },
        },
    )
}

#[cfg(test)]
pub(crate) fn open_event_range_index(
    data_root: &Path,
    cursor: Option<&CoreEventRangeCursor>,
) -> std::result::Result<VerifiedIndex, EventQueryError> {
    let root = data_root.join("search/lexical");
    let index = match cursor {
        Some(cursor) => VerifiedIndex::open_pinned_generation(&root, cursor.generation_id()),
        None => VerifiedIndex::open_pinned(&root),
    }
    .map_err(CoreEventRangeError::from)
    .map_err(EventQueryError::from)?;
    Ok(index)
}

fn open_event_range_generation(
    data_root: &Path,
    request: &ctx_history_read_application::GenerationReadRequest,
) -> std::result::Result<ctx_history_read_application::GenerationRead, EventQueryError> {
    let root = data_root.join("search/lexical");
    let index = match &request.target {
        ctx_history_read_application::GenerationReadTarget::Active => {
            VerifiedIndex::open_pinned(&root)
        }
        ctx_history_read_application::GenerationReadTarget::Exact(generation_id) => {
            VerifiedIndex::open_pinned_generation(&root, generation_id)
        }
    }
    .map_err(CoreEventRangeError::from)
    .map_err(EventQueryError::from)?;
    Ok(ctx_history_read_application::GenerationRead::new(
        index, None,
    ))
}

fn list_page_request(
    selection: CoreEventRangeSelection,
    cursor: Option<CoreEventRangeCursor>,
    request: &EventQueryWireRequest,
    strict_budget: Option<CoreEventPageBudget>,
) -> ctx_history_read_application::ListEventsPageRequest {
    ctx_history_read_application::ListEventsPageRequest {
        selection,
        cursor,
        limit: u64::try_from(request.limit).unwrap_or(u64::MAX),
        page_items: request.page_items(),
        byte_budget: EVENT_QUERY_PAGE_BYTES,
        strict_budget,
    }
}

fn list_page_application_error(
    error: ctx_history_read_application::ListEventsApplicationError<EventQueryError>,
) -> EventQueryError {
    match error {
        ctx_history_read_application::ListEventsApplicationError::Generation(
            ctx_history_read_application::GenerationReadError::Port(error),
        ) => error,
        ctx_history_read_application::ListEventsApplicationError::Query(
            ctx_history_read_application::ListEventsError::Range(error),
        ) => error.into(),
        ctx_history_read_application::ListEventsApplicationError::Query(error) => error.into(),
        ctx_history_read_application::ListEventsApplicationError::Generation(
            ctx_history_read_application::GenerationReadError::Authority(error),
        ) => error.into(),
        ctx_history_read_application::ListEventsApplicationError::Stream(error) => match error {},
    }
}

struct EncodedPage {
    encoded: Vec<u8>,
    items: usize,
}

pub fn event_range_page_value(
    data_root: &Path,
    selection: &CoreEventRangeSelection,
    cursor: Option<&CoreEventRangeCursor>,
    request: &EventQueryWireRequest,
    strict_budget: Option<CoreEventPageBudget>,
) -> std::result::Result<Value, EventQueryError> {
    let mut generation = |read: &ctx_history_read_application::GenerationReadRequest| {
        open_event_range_generation(data_root, read)
    };
    let application = ctx_history_read_application::execute_list_events_page(
        list_page_request(selection.clone(), cursor.cloned(), request, strict_budget),
        &mut generation,
    )
    .map_err(list_page_application_error)?;
    let page = encode_bounded_page(application.index(), &application.result().page, request)?;
    Ok(serde_json::from_slice(&page.encoded)?)
}

fn encode_bounded_page(
    index: &VerifiedIndex,
    page: &CoreEventRangePage,
    request: &EventQueryWireRequest,
) -> std::result::Result<EncodedPage, EventQueryError> {
    let rendered = page
        .items
        .iter()
        .map(|event| render_event(event, request.content))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let next_cursor = page.next_cursor.as_ref().map(encode_cursor);
    let full = encode_page(
        index,
        request,
        &page.generation_id,
        &rendered,
        next_cursor.as_deref(),
        page.terminal,
        global_limit_truncated(request, rendered.len(), page.terminal),
        page.encoded_core_bytes,
        page.content_bytes,
        page.oversized_singleton,
    )?;
    if full.len() <= EVENT_QUERY_PAGE_BYTES {
        return Ok(EncodedPage {
            encoded: full,
            items: rendered.len(),
        });
    }
    if rendered.is_empty() {
        return Err(EventQueryError::WireRecordTooLarge {
            actual: full.len(),
            maximum: EVENT_QUERY_PAGE_BYTES,
        });
    }
    if rendered.len() == 1 {
        let encoded = encode_page(
            index,
            request,
            &page.generation_id,
            &rendered,
            next_cursor.as_deref(),
            page.terminal,
            global_limit_truncated(request, 1, page.terminal),
            page.encoded_core_bytes,
            page.content_bytes,
            true,
        )?;
        enforce_wire_record_cap(encoded.len())?;
        return Ok(EncodedPage { encoded, items: 1 });
    }

    let mut low = 1_usize;
    let mut high = rendered.len().saturating_sub(1);
    let mut accepted: Option<EncodedPage> = None;
    while low <= high {
        let middle = low + (high - low) / 2;
        let cursor = page
            .cursor_after(middle - 1)?
            .ok_or(EventQueryError::MissingContinuationCursor)?;
        let cursor = encode_cursor(&cursor);
        let (encoded_core_bytes, content_bytes) = page
            .usage_for_prefix(middle)?
            .ok_or(EventQueryError::MissingPrefixUsage)?;
        let encoded = encode_page(
            index,
            request,
            &page.generation_id,
            &rendered[..middle],
            Some(&cursor),
            false,
            global_limit_truncated(request, middle, false),
            encoded_core_bytes,
            content_bytes,
            false,
        )?;
        if encoded.len() <= EVENT_QUERY_PAGE_BYTES {
            accepted = Some(EncodedPage {
                encoded,
                items: middle,
            });
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }
    if let Some(accepted) = accepted {
        return Ok(accepted);
    }

    let cursor = page
        .cursor_after(0)?
        .ok_or(EventQueryError::MissingContinuationCursor)?;
    let cursor = encode_cursor(&cursor);
    let (encoded_core_bytes, content_bytes) = page
        .usage_for_prefix(1)?
        .ok_or(EventQueryError::MissingPrefixUsage)?;
    let encoded = encode_page(
        index,
        request,
        &page.generation_id,
        &rendered[..1],
        Some(&cursor),
        false,
        global_limit_truncated(request, 1, false),
        encoded_core_bytes,
        content_bytes,
        true,
    )?;
    enforce_wire_record_cap(encoded.len())?;
    Ok(EncodedPage { encoded, items: 1 })
}

#[allow(clippy::too_many_arguments)]
fn encode_page(
    index: &VerifiedIndex,
    request: &EventQueryWireRequest,
    generation_id: &str,
    events: &[Value],
    next_cursor: Option<&str>,
    terminal: bool,
    truncated: bool,
    encoded_core_bytes: usize,
    content_bytes: usize,
    oversized_singleton: bool,
) -> std::result::Result<Vec<u8>, EventQueryError> {
    let mut bytes = 0_usize;
    loop {
        let model = ctx_history_read_application::event_query_page_read_model(
            index,
            request,
            generation_id,
            events,
            next_cursor,
            terminal,
            truncated,
            ctx_history_read_application::EventQueryPageUsage {
                items: events.len(),
                pages: 1,
                bytes,
                encoded_core_bytes,
                content_bytes,
                oversized_singleton,
            },
        );
        let mut encoded = serde_json::to_vec(&model)?;
        let observed = encoded.len().saturating_add(1);
        if observed == bytes {
            encoded.push(b'\n');
            return Ok(encoded);
        }
        bytes = observed;
    }
}

fn global_limit_truncated(request: &EventQueryWireRequest, items: usize, terminal: bool) -> bool {
    !terminal && items == request.limit
}

fn write_jsonl_pages<F>(
    data_root: &Path,
    selection: CoreEventRangeSelection,
    cursor: Option<CoreEventRangeCursor>,
    request: &EventQueryWireRequest,
    writer: &mut dyn Write,
    after_page: F,
) -> std::result::Result<usize, EventQueryError>
where
    F: FnMut(),
{
    let mut stream = JsonlEventStream {
        writer: CountingWriter::new(writer),
        request,
        after_page,
    };
    let mut generation = |read: &ctx_history_read_application::GenerationReadRequest| {
        open_event_range_generation(data_root, read)
    };
    let result = ctx_history_read_application::execute_list_events_stream(
        ctx_history_read_application::ListEventsPageRequest {
            selection,
            cursor,
            limit: u64::try_from(request.limit).unwrap_or(u64::MAX),
            page_items: EVENT_QUERY_PAGE_ITEMS,
            byte_budget: EVENT_QUERY_PAGE_BYTES,
            strict_budget: None,
        },
        &mut generation,
        &mut stream,
    )
    .map_err(list_stream_application_error)?;
    Ok(result.items)
}

struct JsonlEventStream<'writer, 'request, F> {
    writer: CountingWriter<'writer>,
    request: &'request EventQueryWireRequest,
    after_page: F,
}

impl<F> ctx_history_read_application::ListEventsStreamCallback for JsonlEventStream<'_, '_, F>
where
    F: FnMut(),
{
    type Error = EventQueryError;

    fn page(
        &mut self,
        page: ctx_history_read_application::ListEventsStreamPage<'_>,
    ) -> std::result::Result<ctx_history_read_application::ListEventsStreamControl, Self::Error>
    {
        for (offset, event) in page.page.items.iter().enumerate() {
            let rendered = render_event(event, self.request.content)?;
            let record = ctx_history_read_application::event_query_event_read_model(
                &page.page.generation_id,
                page.ordinal.saturating_add(offset),
                rendered,
            );
            let wire_bytes =
                crate::presentation_limit::serialized_json_bytes(&record)?.saturating_add(1);
            enforce_wire_record_cap(wire_bytes)?;
            serde_json::to_writer(&mut self.writer, &record)?;
            self.writer.write_all(b"\n")?;
            self.writer.flush()?;
        }
        (self.after_page)();
        Ok(ctx_history_read_application::ListEventsStreamControl::Continue)
    }

    fn complete(
        &mut self,
        completion: ctx_history_read_application::ListEventsStreamCompletion<'_>,
    ) -> std::result::Result<(), Self::Error> {
        let encoded = encode_completion(
            completion.index,
            completion.generation_id,
            completion.terminal,
            completion.truncated,
            completion.next_cursor,
            completion.items,
            completion.pages,
            self.writer.bytes(),
            completion.encoded_core_bytes,
            completion.content_bytes,
            completion.oversized_singleton_pages,
            self.request,
        )?;
        self.writer.write_all(&encoded)?;
        self.writer.flush()?;
        Ok(())
    }
}

fn list_stream_application_error(
    error: ctx_history_read_application::ListEventsApplicationError<
        EventQueryError,
        EventQueryError,
    >,
) -> EventQueryError {
    match error {
        ctx_history_read_application::ListEventsApplicationError::Generation(
            ctx_history_read_application::GenerationReadError::Port(error),
        )
        | ctx_history_read_application::ListEventsApplicationError::Stream(error) => error,
        ctx_history_read_application::ListEventsApplicationError::Generation(
            ctx_history_read_application::GenerationReadError::Authority(error),
        ) => error.into(),
        ctx_history_read_application::ListEventsApplicationError::Query(
            ctx_history_read_application::ListEventsError::Range(error),
        ) => error.into(),
        ctx_history_read_application::ListEventsApplicationError::Query(error) => error.into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn encode_completion(
    index: &VerifiedIndex,
    generation_id: &str,
    terminal: bool,
    truncated: bool,
    next_cursor: Option<&str>,
    events: usize,
    pages: usize,
    prior_output_bytes: usize,
    encoded_core_bytes: usize,
    content_bytes: usize,
    oversized_singleton_pages: usize,
    request: &EventQueryWireRequest,
) -> std::result::Result<Vec<u8>, EventQueryError> {
    let mut bytes = prior_output_bytes;
    loop {
        let model = ctx_history_read_application::event_query_completion_read_model(
            index,
            request,
            generation_id,
            next_cursor,
            terminal,
            truncated,
            ctx_history_read_application::EventQueryCompletionUsage {
                items: events,
                pages,
                bytes,
                encoded_core_bytes,
                content_bytes,
                oversized_singleton_pages,
            },
        );
        let mut encoded = serde_json::to_vec(&model)?;
        let observed = prior_output_bytes
            .saturating_add(encoded.len())
            .saturating_add(1);
        if observed == bytes {
            encoded.push(b'\n');
            return Ok(encoded);
        }
        bytes = observed;
    }
}

struct CountingWriter<'a> {
    inner: &'a mut dyn Write,
    bytes: usize,
}

impl<'a> CountingWriter<'a> {
    fn new(inner: &'a mut dyn Write) -> Self {
        Self { inner, bytes: 0 }
    }

    fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Write for CountingWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.bytes = self.bytes.saturating_add(written);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn enforce_wire_record_cap(actual: usize) -> std::result::Result<(), EventQueryError> {
    if actual > MAX_EVENT_QUERY_WIRE_RECORD_BYTES {
        return Err(EventQueryError::WireRecordTooLarge {
            actual,
            maximum: MAX_EVENT_QUERY_WIRE_RECORD_BYTES,
        });
    }
    Ok(())
}

pub fn decode_cursor(encoded: &str) -> std::result::Result<CoreEventRangeCursor, EventQueryError> {
    ctx_history_read_application::decode_event_range_cursor(encoded).map_err(Into::into)
}

pub(crate) fn encode_cursor(cursor: &CoreEventRangeCursor) -> String {
    ctx_history_read_application::encode_event_range_cursor(cursor)
}

pub(crate) fn parse_uuid(
    field: &'static str,
    value: Option<&str>,
) -> std::result::Result<Option<Uuid>, EventQueryError> {
    ctx_history_read_application::parse_event_query_uuid(field, value).map_err(Into::into)
}

pub fn event_query_error_value(error: &EventQueryError) -> Value {
    let output_limit_exceeded = matches!(
        error,
        EventQueryError::Range(CoreEventRangeError::RecordExceedsStrictBudget { .. })
    ) || matches!(
        error,
        EventQueryError::Application(ctx_history_read_application::ListEventsError::Range(
            CoreEventRangeError::RecordExceedsStrictBudget { .. }
        ))
    );
    let error_code = match error {
        EventQueryError::GenerationReadAuthority(_) => "event_query_failed",
        EventQueryError::Range(error) => event_range_error_code(error),
        EventQueryError::Application(error) => list_events_error_code(error),
        EventQueryError::WireRecordTooLarge { .. } => "resource_limit",
        EventQueryError::MissingContinuationCursor | EventQueryError::MissingPrefixUsage => {
            "invalid_page"
        }
        EventQueryError::Io(_) => "output_failed",
        _ => "event_query_failed",
    };
    let retryable = output_limit_exceeded;
    json!({
        "schema_version": EVENT_QUERY_SCHEMA_VERSION,
        "error_code": error_code,
        "detail": error.to_string(),
        "retryable": retryable,
        "restart_required": error_code == "generation_not_retained",
        "recommendation": if output_limit_exceeded {
            Some("use CLI JSONL with ctx list events")
        } else if matches!(error, EventQueryError::WireRecordTooLarge { .. }) {
            Some("retry with --content text or --content none")
        } else {
            None
        },
    })
}

fn event_range_error_code(error: &CoreEventRangeError) -> &'static str {
    match error {
        CoreEventRangeError::Index(IndexError::PinnedGenerationNotRetained { .. }) => {
            "generation_not_retained"
        }
        CoreEventRangeError::CursorSelectionMismatch => "cursor_request_mismatch",
        CoreEventRangeError::CursorGenerationMismatch { .. } => "cursor_generation_mismatch",
        CoreEventRangeError::InvalidCursor => "invalid_cursor",
        CoreEventRangeError::InvalidCursorCoordinate => "invalid_cursor_coordinate",
        CoreEventRangeError::InvalidRange { .. } | CoreEventRangeError::InvalidFilter { .. } => {
            "invalid_range"
        }
        CoreEventRangeError::RecordExceedsStrictBudget { .. } => "output_limit_exceeded",
        CoreEventRangeError::InvalidPageSize { .. }
        | CoreEventRangeError::Index(IndexError::InvalidCoreEventPageByteLimit { .. }) => {
            "resource_limit"
        }
        _ => "event_query_failed",
    }
}

fn list_events_error_code(error: &ctx_history_read_application::ListEventsError) -> &'static str {
    use ctx_history_read_application::ListEventsError;
    match error {
        ListEventsError::Range(error) => event_range_error_code(error),
        ListEventsError::InvalidTimestamp { .. }
        | ListEventsError::InvalidTimestampPrecision { .. }
        | ListEventsError::IncompleteTimestampRange
        | ListEventsError::InvalidUuid { .. } => "invalid_range",
        ListEventsError::InvalidResourceLimit { .. } => "resource_limit",
        ListEventsError::CursorTooLarge { .. } | ListEventsError::InvalidCursorEncoding => {
            "invalid_cursor"
        }
        ListEventsError::MissingContinuationCursor | ListEventsError::NonAdvancingPage => {
            "invalid_page"
        }
    }
}

impl From<IndexError> for EventQueryError {
    fn from(value: IndexError) -> Self {
        Self::Range(CoreEventRangeError::Index(value))
    }
}

#[cfg(test)]
#[path = "list_events/tests.rs"]
mod tests;

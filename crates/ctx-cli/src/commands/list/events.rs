use std::{io::Write, path::Path, path::PathBuf};

use anyhow::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, SecondsFormat, Utc};
use ctx_history_core::{
    CoreContentPolicyStatus, MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES,
};
use ctx_history_index::{
    CoreEventPageBudget, CoreEventRangeCursor, CoreEventRangeError, CoreEventRangeFilters,
    CoreEventRangePage, CoreEventRangeSelection, CoreEventRecord, IndexError, VerifiedIndex,
};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    analytics::count_bucket,
    analytics::ShowTelemetry,
    local_usage::{CliUsage, ResultObservationAction},
    ui::Ui,
};

mod request;

#[cfg(test)]
use ctx_history_index::{CoreEventRangeDirection, CoreEventRangeScope};
pub(crate) use request::{
    EventContentProjection, EventQueryFormat, EventQueryWireRequest, ListEventsArgs,
};

pub(crate) const EVENT_QUERY_SCHEMA_VERSION: u8 = 1;
pub(crate) const DEFAULT_EVENT_QUERY_LIMIT: u64 = 10_000;
const EVENT_QUERY_PAGE_ITEMS: usize = 100;
const EVENT_QUERY_PAGE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_EVENT_QUERY_LIMIT: u64 = 10_000_000;
pub(crate) const MAX_EVENT_QUERY_CURSOR_CHARS: usize = 512;
/// JSON string escaping can expand each admitted Core byte to six wire bytes.
/// Keep a fixed envelope allowance while retaining a deterministic upper bound.
pub(crate) const MAX_EVENT_QUERY_WIRE_RECORD_BYTES: usize =
    MAX_ENCODED_CORE_RECORD_BYTES * 6 + 1024 * 1024;

/// Stored Core is already JSON escaped. Reserving seven eighths of the MCP
/// envelope covers event projection, receipt fields, and JSON-RPC framing
/// before a record is materialized.
pub(crate) const fn mcp_event_query_core_record_bytes(response_cap: usize) -> usize {
    response_cap / 8
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum EventQueryError {
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
    #[error("Core event page omitted admitted usage for a retained prefix")]
    MissingPrefixUsage,
    #[error("nonterminal Core event page made no progress")]
    NonAdvancingPage,
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

pub(crate) fn run(
    args: ListEventsArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    match execute(args, &data_root, ui.stdout_writer()) {
        Ok(events) => {
            telemetry.events_returned = Some(count_bucket(events as u64));
            local_usage.set_result_observation(ResultObservationAction::OpenEvent, events, 0, 0);
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
    let selection = selection_from_args(&args)?;
    let cursor = args.cursor.as_deref().map(decode_cursor).transpose()?;
    if let Some(cursor) = &cursor {
        cursor.validate_selection(&selection)?;
    }
    let index = open_event_range_index(data_root, cursor.as_ref())?;
    let limit = validated_limit(args.limit)?;
    let request = EventQueryWireRequest::from_selection(&selection, args.content, limit);
    match args.format {
        EventQueryFormat::Json => {
            let page = bounded_page(&index, &selection, cursor.as_ref(), &request, None)?;
            writer.write_all(&page.encoded)?;
            writer.flush()?;
            Ok(page.items)
        }
        EventQueryFormat::Jsonl => {
            write_jsonl_pages(&index, &selection, cursor, &request, writer, || {})
        }
    }
}

pub(crate) fn validated_limit(limit: u64) -> std::result::Result<usize, EventQueryError> {
    validate_resource_limit("limit", limit, 1, MAX_EVENT_QUERY_LIMIT)?;
    usize::try_from(limit).map_err(|_| EventQueryError::InvalidResourceLimit {
        field: "limit",
        requested: limit,
        minimum: 1,
        maximum: MAX_EVENT_QUERY_LIMIT,
    })
}

fn validate_resource_limit(
    field: &'static str,
    requested: u64,
    minimum: u64,
    maximum: u64,
) -> std::result::Result<(), EventQueryError> {
    if !(minimum..=maximum).contains(&requested) {
        return Err(EventQueryError::InvalidResourceLimit {
            field,
            requested,
            minimum,
            maximum,
        });
    }
    Ok(())
}

pub(crate) fn selection(
    since: Option<&str>,
    until: Option<&str>,
    filters: CoreEventRangeFilters,
) -> std::result::Result<CoreEventRangeSelection, EventQueryError> {
    match (since, until) {
        (None, None) => CoreEventRangeSelection::all(filters).map_err(Into::into),
        (Some(since), Some(until)) => CoreEventRangeSelection::with_filters(
            parse_rfc3339("since", since)?,
            parse_rfc3339("until", until)?,
            filters,
        )
        .map_err(Into::into),
        (Some(_), None) | (None, Some(_)) => Err(EventQueryError::IncompleteTimestampRange),
    }
}

fn selection_from_args(
    args: &ListEventsArgs,
) -> std::result::Result<CoreEventRangeSelection, EventQueryError> {
    selection(
        args.since.as_deref(),
        args.until.as_deref(),
        CoreEventRangeFilters {
            providers: args.provider.clone(),
            source_identity: parse_uuid("source", args.source.as_deref())?,
            history_source: args.history_source.clone(),
            provider_key: args.provider_key.clone(),
            source_id: args.source_id.clone(),
            source_format: args.source_format.clone(),
            provider_session_id: args.provider_session.clone(),
            session_id: parse_uuid("session", args.session.as_deref())?,
            parent_session_id: parse_uuid("parent_session", args.parent_session.as_deref())?,
            root_session_id: parse_uuid("root", args.root_session.as_deref())?,
            branch: args.branch.clone(),
            workspace: args.workspace.clone(),
            event_type: args.event_type.clone(),
            role: args.role.clone(),
            agent_type: args.agent_type.clone(),
            scope: args.scope.into(),
            file: args.file.clone(),
            direction: args.direction.into(),
        },
    )
}

pub(crate) fn open_event_range_index(
    data_root: &Path,
    cursor: Option<&CoreEventRangeCursor>,
) -> std::result::Result<VerifiedIndex, EventQueryError> {
    let root = data_root.join("search/lexical");
    match cursor {
        Some(cursor) => VerifiedIndex::open_pinned_generation(&root, cursor.generation_id()),
        None => VerifiedIndex::open_pinned(&root),
    }
    .map_err(CoreEventRangeError::from)
    .map_err(Into::into)
}

fn read_page(
    index: &VerifiedIndex,
    selection: &CoreEventRangeSelection,
    cursor: Option<&CoreEventRangeCursor>,
    page_items: usize,
    byte_budget: usize,
    strict_budget: Option<CoreEventPageBudget>,
) -> std::result::Result<CoreEventRangePage, EventQueryError> {
    let budget = CoreEventPageBudget::new(byte_budget, byte_budget.min(MAX_CORE_CONTENT_BYTES));
    match strict_budget {
        Some(strict_budget) => index.core_event_range_page_with_strict_budget(
            selection,
            cursor,
            page_items,
            budget,
            strict_budget,
        ),
        None => index.core_event_range_page_with_budget(selection, cursor, page_items, budget),
    }
    .map_err(Into::into)
}

struct EncodedPage {
    encoded: Vec<u8>,
    items: usize,
}

fn bounded_page(
    index: &VerifiedIndex,
    selection: &CoreEventRangeSelection,
    cursor: Option<&CoreEventRangeCursor>,
    request: &EventQueryWireRequest,
    strict_budget: Option<CoreEventPageBudget>,
) -> std::result::Result<EncodedPage, EventQueryError> {
    let page = read_page(
        index,
        selection,
        cursor,
        request.page_items(),
        EVENT_QUERY_PAGE_BYTES,
        strict_budget,
    )?;
    encode_bounded_page(index, &page, request)
}

pub(crate) fn event_range_page_value(
    data_root: &Path,
    selection: &CoreEventRangeSelection,
    cursor: Option<&CoreEventRangeCursor>,
    request: &EventQueryWireRequest,
    strict_budget: Option<CoreEventPageBudget>,
) -> std::result::Result<Value, EventQueryError> {
    if let Some(cursor) = cursor {
        cursor.validate_selection(selection)?;
    }
    let index = open_event_range_index(data_root, cursor)?;
    let page = bounded_page(&index, selection, cursor, request, strict_budget)?;
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

#[derive(Serialize)]
struct EventQueryReceipt<'a> {
    schema_version: u8,
    generation_id: &'a str,
    domain: &'a Value,
    filters: &'a Value,
    direction: &'static str,
    content: &'static str,
    limit: usize,
    terminal: bool,
    truncated: bool,
    next_cursor: Option<&'a str>,
    freshness: EventQueryFreshness,
    frontier: EventQueryFrontier<'a>,
}

#[derive(Serialize)]
struct EventQueryFreshness {
    mode: &'static str,
    status: &'static str,
    source_count: usize,
    read_only: bool,
}

#[derive(Serialize)]
struct EventQueryFrontier<'a> {
    generation_id: &'a str,
    cursor: Option<&'a str>,
    status: &'static str,
    certified_sources: usize,
    sources_with_frontier: usize,
    certified_bytes: u64,
}

#[derive(Serialize)]
struct EventQueryPageUsage {
    items: usize,
    pages: usize,
    bytes: usize,
    encoded_core_bytes: usize,
    content_bytes: usize,
    oversized_singleton: bool,
}

#[derive(Serialize)]
struct EventQueryPage<'a> {
    #[serde(flatten)]
    receipt: EventQueryReceipt<'a>,
    payload_type: &'static str,
    events: &'a [Value],
    usage: EventQueryPageUsage,
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
        let mut encoded = serde_json::to_vec(&EventQueryPage {
            receipt: event_query_receipt(
                index,
                request,
                generation_id,
                next_cursor,
                terminal,
                truncated,
            ),
            payload_type: "event_range_page",
            events,
            usage: EventQueryPageUsage {
                items: events.len(),
                pages: 1,
                bytes,
                encoded_core_bytes,
                content_bytes,
                oversized_singleton,
            },
        })?;
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
    index: &VerifiedIndex,
    selection: &CoreEventRangeSelection,
    mut cursor: Option<CoreEventRangeCursor>,
    request: &EventQueryWireRequest,
    writer: &mut dyn Write,
    mut after_page: F,
) -> std::result::Result<usize, EventQueryError>
where
    F: FnMut(),
{
    let mut writer = CountingWriter::new(writer);
    let mut events = 0_usize;
    let mut pages = 0_usize;
    let mut oversized_singleton_pages = 0_usize;
    let mut encoded_core_bytes = 0_usize;
    let mut content_bytes = 0_usize;
    loop {
        let remaining = request.limit.saturating_sub(events);
        let page = read_page(
            index,
            selection,
            cursor.as_ref(),
            EVENT_QUERY_PAGE_ITEMS.min(remaining),
            EVENT_QUERY_PAGE_BYTES,
            None,
        )?;
        pages = pages.checked_add(1).ok_or(IndexError::CountOverflow)?;
        oversized_singleton_pages = oversized_singleton_pages
            .checked_add(usize::from(page.oversized_singleton))
            .ok_or(IndexError::CountOverflow)?;
        if page.items.is_empty() && !page.terminal {
            return Err(EventQueryError::NonAdvancingPage);
        }
        for event in &page.items {
            let record = json!({
                "schema_version": EVENT_QUERY_SCHEMA_VERSION,
                "record_type": "event_range_event",
                "generation_id": page.generation_id,
                "ordinal": events,
                "event": render_event(event, request.content)?,
            });
            let wire_bytes =
                crate::presentation_limit::serialized_json_bytes(&record)?.saturating_add(1);
            enforce_wire_record_cap(wire_bytes)?;
            serde_json::to_writer(&mut writer, &record)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            events = events.checked_add(1).ok_or(IndexError::CountOverflow)?;
        }
        encoded_core_bytes = encoded_core_bytes
            .checked_add(page.encoded_core_bytes)
            .ok_or(IndexError::CountOverflow)?;
        content_bytes = content_bytes
            .checked_add(page.content_bytes)
            .ok_or(IndexError::CountOverflow)?;
        after_page();

        let terminal = page.terminal;
        let next_cursor = page.next_cursor;
        let limit_reached = events >= request.limit;
        if terminal || limit_reached {
            let next_cursor = next_cursor.as_ref().map(encode_cursor);
            let completion = encode_completion(
                index,
                &page.generation_id,
                terminal,
                limit_reached && !terminal,
                next_cursor.as_deref(),
                events,
                pages,
                writer.bytes(),
                encoded_core_bytes,
                content_bytes,
                oversized_singleton_pages,
                request,
            )?;
            writer.write_all(&completion)?;
            writer.flush()?;
            return Ok(events);
        }
        cursor = Some(next_cursor.ok_or(EventQueryError::MissingContinuationCursor)?);
    }
}

#[derive(Serialize)]
struct EventQueryCompletionUsage {
    items: usize,
    pages: usize,
    bytes: usize,
    encoded_core_bytes: usize,
    content_bytes: usize,
    oversized_singleton_pages: usize,
}

#[derive(Serialize)]
struct EventQueryCompletion<'a> {
    #[serde(flatten)]
    receipt: EventQueryReceipt<'a>,
    record_type: &'static str,
    usage: EventQueryCompletionUsage,
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
        let mut encoded = serde_json::to_vec(&EventQueryCompletion {
            receipt: event_query_receipt(
                index,
                request,
                generation_id,
                next_cursor,
                terminal,
                truncated,
            ),
            record_type: "event_range_completion",
            usage: EventQueryCompletionUsage {
                items: events,
                pages,
                bytes,
                encoded_core_bytes,
                content_bytes,
                oversized_singleton_pages,
            },
        })?;
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

pub(crate) fn render_event(
    event: &CoreEventRecord,
    projection: EventContentProjection,
) -> std::result::Result<Value, EventQueryError> {
    let record = &event.core_record;
    let content = &record.content;
    let (policy_status, policy_reason, complete) = match &content.policy_status {
        CoreContentPolicyStatus::Selected => ("selected", None, true),
        CoreContentPolicyStatus::Redacted { reason } => ("redacted", Some(reason.as_str()), false),
        CoreContentPolicyStatus::Omitted { reason } => ("omitted", Some(reason.as_str()), false),
    };
    let text = (projection != EventContentProjection::None)
        .then_some(content.normalized_body.as_ref())
        .flatten();
    let structured_content = (projection == EventContentProjection::Full)
        .then_some(content.structured_content.as_ref())
        .flatten();
    let occurred_at = format_timestamp(event.occurred_at_unix_ms);
    Ok(json!({
        "schema_version": EVENT_QUERY_SCHEMA_VERSION,
        "record_version": record.record_version,
        "ctx_event_id": event.event_id.as_uuid(),
        "ctx_source_id": event.source.identity().as_uuid(),
        "ctx_session_id": event.session_id.as_uuid(),
        "parent_ctx_session_id": event.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": event.root_session_id.as_uuid(),
        "occurred_at": occurred_at,
        "occurred_at_ms": event.occurred_at_unix_ms,
        "sequence": event.event_sequence,
        "provider": event.provider,
        "source_format": event.source_format,
        "source": event.source,
        "provider_session_id": event.provider_session_id,
        "native_event_id": event.native_event_id,
        "branch": event.branch,
        "agent_type": event.agent_type,
        "agent_scope": if event.is_primary { "primary" } else { "subagent" },
        "is_primary": event.is_primary,
        "event_type": event.event_type,
        "role": event.role,
        "workspace": event.workspace,
        "cwd": event.cwd,
        "touched_files": event.touched_files,
        "parser_revision": record.parser_revision,
        "normalization_revision": record.normalization_revision,
        "text": text,
        "structured_content": structured_content,
        "content": {
            "complete": complete,
            "policy_revision": content.policy_revision,
            "policy_status": policy_status,
            "policy_reason": policy_reason,
        },
        "citations": [{
            "target_type": "event",
            "ctx_event_id": event.event_id.as_uuid(),
            "ctx_session_id": event.session_id.as_uuid(),
            "label": event.event_type,
            "time": occurred_at,
            "provider": event.provider,
            "session_id": event.provider_session_id,
            "event_seq": event.event_sequence,
        }],
        "metadata": record.metadata,
        "repository_candidate_evidence": record.repository_candidate_evidence,
        "repository_bindings": record.repository_bindings,
        "repository_abstentions": record.repository_abstentions,
        "repository_file_invocation_evidence": record.repository_file_invocation_evidence,
        "repository_file_observations": record.repository_file_observations,
        "repository_vcs_observations": record.repository_vcs_observations,
        "content_projection": projection.as_str(),
    }))
}

fn event_query_receipt<'a>(
    index: &VerifiedIndex,
    request: &'a EventQueryWireRequest,
    generation_id: &'a str,
    next_cursor: Option<&'a str>,
    terminal: bool,
    truncated: bool,
) -> EventQueryReceipt<'a> {
    let sources = &index.manifest().sources;
    let sources_with_frontier = sources
        .iter()
        .filter(|source| source.frontier().is_some())
        .count();
    let frontier_status = if sources_with_frontier == 0 {
        "unavailable"
    } else if sources_with_frontier == sources.len() {
        "available"
    } else {
        "partial"
    };
    EventQueryReceipt {
        schema_version: EVENT_QUERY_SCHEMA_VERSION,
        generation_id,
        domain: &request.domain,
        filters: &request.filters,
        direction: request.direction,
        content: request.content.as_str(),
        limit: request.limit,
        terminal,
        truncated,
        next_cursor,
        freshness: EventQueryFreshness {
            mode: "pinned",
            status: "not_checked",
            source_count: sources.len(),
            read_only: true,
        },
        frontier: EventQueryFrontier {
            generation_id,
            cursor: next_cursor,
            status: frontier_status,
            certified_sources: sources.len(),
            sources_with_frontier,
            certified_bytes: index.manifest().certified_source_bytes,
        },
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

pub(crate) fn decode_cursor(
    encoded: &str,
) -> std::result::Result<CoreEventRangeCursor, EventQueryError> {
    if encoded.len() > MAX_EVENT_QUERY_CURSOR_CHARS {
        return Err(EventQueryError::CursorTooLarge {
            actual: encoded.len(),
            maximum: MAX_EVENT_QUERY_CURSOR_CHARS,
        });
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| EventQueryError::InvalidCursorEncoding)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err(EventQueryError::InvalidCursorEncoding);
    }
    Ok(CoreEventRangeCursor::decode(&bytes)?)
}

pub(crate) fn encode_cursor(cursor: &CoreEventRangeCursor) -> String {
    URL_SAFE_NO_PAD.encode(cursor.encode())
}

pub(crate) fn parse_rfc3339(
    field: &'static str,
    value: &str,
) -> std::result::Result<i64, EventQueryError> {
    let timestamp =
        DateTime::parse_from_rfc3339(value).map_err(|_| EventQueryError::InvalidTimestamp {
            field,
            value: value.to_owned(),
        })?;
    if timestamp.timestamp_subsec_nanos() % 1_000_000 != 0 {
        return Err(EventQueryError::InvalidTimestampPrecision {
            field,
            value: value.to_owned(),
        });
    }
    Ok(timestamp.timestamp_millis())
}

pub(crate) fn parse_uuid(
    field: &'static str,
    value: Option<&str>,
) -> std::result::Result<Option<Uuid>, EventQueryError> {
    value
        .map(|value| {
            Uuid::parse_str(value).map_err(|_| EventQueryError::InvalidUuid {
                field,
                value: value.to_owned(),
            })
        })
        .transpose()
}

fn format_timestamp(value: Option<i64>) -> Option<String> {
    value
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub(crate) fn event_query_error_value(error: &EventQueryError) -> Value {
    let output_limit_exceeded = matches!(
        error,
        EventQueryError::Range(CoreEventRangeError::RecordExceedsStrictBudget { .. })
    );
    let error_code = match error {
        EventQueryError::Range(CoreEventRangeError::Index(
            IndexError::PinnedGenerationNotRetained { .. },
        )) => "generation_not_retained",
        EventQueryError::Range(CoreEventRangeError::CursorSelectionMismatch) => {
            "cursor_request_mismatch"
        }
        EventQueryError::Range(CoreEventRangeError::CursorGenerationMismatch { .. }) => {
            "cursor_generation_mismatch"
        }
        EventQueryError::Range(CoreEventRangeError::InvalidCursor)
        | EventQueryError::InvalidCursorEncoding
        | EventQueryError::CursorTooLarge { .. } => "invalid_cursor",
        EventQueryError::Range(CoreEventRangeError::InvalidCursorCoordinate) => {
            "invalid_cursor_coordinate"
        }
        EventQueryError::Range(CoreEventRangeError::InvalidRange { .. })
        | EventQueryError::Range(CoreEventRangeError::InvalidFilter { .. })
        | EventQueryError::InvalidTimestamp { .. }
        | EventQueryError::InvalidTimestampPrecision { .. }
        | EventQueryError::IncompleteTimestampRange
        | EventQueryError::InvalidUuid { .. } => "invalid_range",
        EventQueryError::Range(CoreEventRangeError::RecordExceedsStrictBudget { .. }) => {
            "output_limit_exceeded"
        }
        EventQueryError::InvalidResourceLimit { .. }
        | EventQueryError::WireRecordTooLarge { .. }
        | EventQueryError::Range(CoreEventRangeError::InvalidPageSize { .. })
        | EventQueryError::Range(CoreEventRangeError::Index(
            IndexError::InvalidCoreEventPageByteLimit { .. },
        )) => "resource_limit",
        EventQueryError::MissingContinuationCursor
        | EventQueryError::MissingPrefixUsage
        | EventQueryError::NonAdvancingPage => "invalid_page",
        EventQueryError::Io(_) => "output_failed",
        _ => "event_query_failed",
    };
    json!({
        "schema_version": EVENT_QUERY_SCHEMA_VERSION,
        "error_code": error_code,
        "detail": error.to_string(),
        "retryable": output_limit_exceeded,
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

impl From<IndexError> for EventQueryError {
    fn from(value: IndexError) -> Self {
        Self::Range(CoreEventRangeError::Index(value))
    }
}

#[cfg(test)]
#[path = "events/tests.rs"]
mod tests;

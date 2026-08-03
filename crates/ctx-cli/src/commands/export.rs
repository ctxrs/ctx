use std::{io::Write, path::PathBuf};

use anyhow::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::DateTime;
use clap::{Args, Subcommand, ValueEnum};
use ctx_history_core::CoreContentPolicyStatus;
use ctx_history_index::{
    CoreEventRangeCursor, CoreEventRangeError, CoreEventRangeSelection, IndexError, VerifiedIndex,
    MAX_CORE_EVENT_RANGE_PAGE_ITEMS,
};
use serde_json::{json, Value};

use crate::{output::compact_json, provider_args::ProviderArg, ui::Ui};

const EXPORT_SCHEMA_VERSION: u8 = 1;
const DEFAULT_EXPORT_ITEMS: u64 = 100;
const DEFAULT_EXPORT_BYTES: u64 = 1024 * 1024;
const MIN_EXPORT_BYTES: u64 = 512;
const MAX_EXPORT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CURSOR_CHARS: usize = 512;

#[derive(Debug, Args)]
pub(crate) struct ExportArgs {
    #[command(subcommand)]
    target: ExportTarget,
}

#[derive(Debug, Subcommand)]
enum ExportTarget {
    #[command(about = "Export complete timestamped Core events in deterministic order")]
    Events(ExportEventsArgs),
}

#[derive(Debug, Args)]
struct ExportEventsArgs {
    #[arg(long, help = "Inclusive absolute RFC3339 lower bound")]
    since: String,
    #[arg(long, help = "Exclusive absolute RFC3339 upper bound")]
    until: String,
    #[arg(
        long,
        value_enum,
        help = "Filter by provider; repeat to select more than one"
    )]
    provider: Vec<ProviderArg>,
    #[arg(long, help = "Resume from an opaque cursor returned by JSON output")]
    cursor: Option<String>,
    #[arg(
        long,
        default_value_t = DEFAULT_EXPORT_ITEMS,
        value_parser = clap::value_parser!(u64).range(1..=MAX_CORE_EVENT_RANGE_PAGE_ITEMS as u64),
        help = "Maximum events per internal page"
    )]
    max_items: u64,
    #[arg(
        long,
        default_value_t = DEFAULT_EXPORT_BYTES,
        value_parser = clap::value_parser!(u64).range(MIN_EXPORT_BYTES..=MAX_EXPORT_BYTES),
        help = "Maximum serialized bytes per page"
    )]
    max_bytes: u64,
    #[arg(long, value_enum, default_value_t = EventExportFormat::Json)]
    format: EventExportFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EventExportFormat {
    Json,
    Jsonl,
}

#[derive(Debug, thiserror::Error)]
enum ExportEventsError {
    #[error(transparent)]
    Range(#[from] CoreEventRangeError),
    #[error("{field} must be an absolute RFC3339 timestamp: {value:?}")]
    InvalidTimestamp { field: &'static str, value: String },
    #[error("event export cursor is {actual} characters, maximum {maximum}")]
    CursorTooLarge { actual: usize, maximum: usize },
    #[error("event export cursor is not canonical base64url")]
    InvalidCursorEncoding,
    #[error(
        "event {event_id} requires {required_bytes} serialized bytes, maximum {maximum_bytes}"
    )]
    WireRecordTooLarge {
        event_id: uuid::Uuid,
        required_bytes: usize,
        maximum_bytes: usize,
        cursor: Option<String>,
    },
    #[error(
        "the empty event export page requires {required_bytes} bytes, maximum {maximum_bytes}"
    )]
    WireEnvelopeTooLarge {
        required_bytes: usize,
        maximum_bytes: usize,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub(crate) fn run_export(args: ExportArgs, data_root: PathBuf, ui: &mut Ui) -> Result<()> {
    let result = match args.target {
        ExportTarget::Events(args) => execute_events(args, data_root, ui.stdout_writer()),
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            writeln!(
                ui.stderr_writer(),
                "{}",
                serde_json::to_string(&export_error_value(&error))?
            )?;
            Err(crate::dispatch::rendered_cli_error())
        }
    }
}

fn execute_events(
    args: ExportEventsArgs,
    data_root: PathBuf,
    writer: &mut dyn Write,
) -> std::result::Result<(), ExportEventsError> {
    let since = parse_rfc3339("since", &args.since)?;
    let until = parse_rfc3339("until", &args.until)?;
    let providers = args
        .provider
        .into_iter()
        .map(|provider| provider.capture_provider().as_str().to_owned());
    let selection = CoreEventRangeSelection::new(since, until, providers)?;
    let cursor = args.cursor.as_deref().map(decode_cursor).transpose()?;
    if let Some(cursor) = &cursor {
        cursor.validate_selection(&selection)?;
    }
    let root = data_root.join("search/lexical");
    let index = match &cursor {
        Some(cursor) => VerifiedIndex::open_pinned_generation(&root, cursor.generation_id()),
        None => VerifiedIndex::open_pinned(&root),
    }
    .map_err(CoreEventRangeError::from)?;
    let max_items = args.max_items as usize;
    let max_bytes = args.max_bytes as usize;

    match args.format {
        EventExportFormat::Json => write_json_page(
            &index,
            &selection,
            cursor.as_ref(),
            args.cursor.as_deref(),
            max_items,
            max_bytes,
            writer,
        ),
        EventExportFormat::Jsonl => write_jsonl_pages(
            &index,
            &selection,
            cursor,
            args.cursor,
            max_items,
            max_bytes,
            writer,
            || {},
        ),
    }
}

fn write_json_page(
    index: &VerifiedIndex,
    selection: &CoreEventRangeSelection,
    cursor: Option<&CoreEventRangeCursor>,
    cursor_text: Option<&str>,
    max_items: usize,
    max_bytes: usize,
    writer: &mut dyn Write,
) -> std::result::Result<(), ExportEventsError> {
    let page = index.core_event_range_page(selection, cursor, max_items)?;
    let rendered = page
        .items
        .iter()
        .map(render_export_event)
        .collect::<Vec<_>>();
    let full_cursor = page.next_cursor.as_ref().map(encode_cursor);
    let full = encode_page(
        &page.generation_id,
        &rendered,
        full_cursor.as_deref(),
        page.terminal,
    )?;
    if full.len() <= max_bytes {
        writer.write_all(&full)?;
        return Ok(());
    }
    if rendered.is_empty() {
        return Err(ExportEventsError::WireEnvelopeTooLarge {
            required_bytes: full.len(),
            maximum_bytes: max_bytes,
        });
    }

    let mut low = 1_usize;
    let mut high = rendered.len().saturating_sub(1);
    let mut accepted = None;
    while low <= high {
        let middle = low + (high - low) / 2;
        let cursor = selection.cursor_for(&page.generation_id, &page.items[middle - 1])?;
        let cursor = encode_cursor(&cursor);
        let encoded = encode_page(
            &page.generation_id,
            &rendered[..middle],
            Some(&cursor),
            false,
        )?;
        if encoded.len() <= max_bytes {
            accepted = Some(encoded);
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }
    if let Some(encoded) = accepted {
        writer.write_all(&encoded)?;
        return Ok(());
    }
    let terminal = page.terminal && rendered.len() == 1;
    let singleton_cursor = (!terminal)
        .then(|| selection.cursor_for(&page.generation_id, &page.items[0]))
        .transpose()?
        .as_ref()
        .map(encode_cursor);
    let singleton = encode_page(
        &page.generation_id,
        &rendered[..1],
        singleton_cursor.as_deref(),
        terminal,
    )?;
    Err(ExportEventsError::WireRecordTooLarge {
        event_id: page.items[0].event_id.as_uuid(),
        required_bytes: singleton.len(),
        maximum_bytes: max_bytes,
        cursor: cursor_text.map(ToOwned::to_owned),
    })
}

#[allow(clippy::too_many_arguments)]
fn write_jsonl_pages<F>(
    index: &VerifiedIndex,
    selection: &CoreEventRangeSelection,
    mut cursor: Option<CoreEventRangeCursor>,
    mut cursor_text: Option<String>,
    max_items: usize,
    max_bytes: usize,
    writer: &mut dyn Write,
    mut after_page: F,
) -> std::result::Result<(), ExportEventsError>
where
    F: FnMut(),
{
    loop {
        let page = index.core_event_range_page(selection, cursor.as_ref(), max_items)?;
        if page.items.is_empty() {
            return Ok(());
        }
        let mut buffer = Vec::new();
        let mut accepted = 0_usize;
        for event in &page.items {
            let mut line = serde_json::to_vec(&render_export_event(event))?;
            line.push(b'\n');
            if buffer.len().saturating_add(line.len()) > max_bytes {
                if accepted == 0 {
                    return Err(ExportEventsError::WireRecordTooLarge {
                        event_id: event.event_id.as_uuid(),
                        required_bytes: line.len(),
                        maximum_bytes: max_bytes,
                        cursor: cursor_text,
                    });
                }
                break;
            }
            buffer.extend_from_slice(&line);
            accepted += 1;
        }
        writer.write_all(&buffer)?;
        after_page();
        if accepted == page.items.len() && page.terminal {
            return Ok(());
        }
        let next = selection.cursor_for(&page.generation_id, &page.items[accepted - 1])?;
        cursor_text = Some(encode_cursor(&next));
        cursor = Some(next);
    }
}

fn render_export_event(event: &ctx_history_index::CoreEventRecord) -> Value {
    let content = &event.core_record.content;
    let (policy_status, policy_reason, complete) = match &content.policy_status {
        CoreContentPolicyStatus::Selected => ("selected", None, true),
        CoreContentPolicyStatus::Redacted { reason } => ("redacted", Some(reason.as_str()), false),
        CoreContentPolicyStatus::Omitted { reason } => ("omitted", Some(reason.as_str()), false),
    };
    compact_json(json!({
        "schema_version": EXPORT_SCHEMA_VERSION,
        "ctx_event_id": event.event_id.as_uuid(),
        "ctx_source_id": event.source.identity().as_uuid(),
        "ctx_session_id": event.session_id.as_uuid(),
        "parent_ctx_session_id": event.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": event.root_session_id.as_uuid(),
        "provider": event.provider,
        "source_format": event.source_format,
        "provider_session_id": event.provider_session_id,
        "native_event_id": event.native_event_id,
        "branch": event.branch,
        "agent_type": event.agent_type,
        "is_primary": event.is_primary,
        "event_sequence": event.event_sequence,
        "occurred_at_unix_ms": event.occurred_at_unix_ms,
        "event_type": event.event_type,
        "role": event.role,
        "workspace": event.workspace,
        "cwd": event.cwd,
        "touched_files": event.touched_files,
        "text": content.normalized_body.as_deref(),
        "structured_content": content.structured_content.as_ref(),
        "content": {
            "complete": complete,
            "policy_status": policy_status,
            "policy_reason": policy_reason,
        },
    }))
}

fn encode_page(
    generation_id: &str,
    events: &[Value],
    next_cursor: Option<&str>,
    terminal: bool,
) -> std::result::Result<Vec<u8>, ExportEventsError> {
    let mut exact_bytes = 0_usize;
    loop {
        let mut encoded = serde_json::to_vec(&json!({
            "schema_version": EXPORT_SCHEMA_VERSION,
            "generation_id": generation_id,
            "events": events,
            "next_cursor": next_cursor,
            "terminal": terminal,
            "usage": {"items": events.len(), "bytes": exact_bytes},
        }))?;
        let observed = encoded.len().saturating_add(1);
        if observed == exact_bytes {
            encoded.push(b'\n');
            return Ok(encoded);
        }
        exact_bytes = observed;
    }
}

fn decode_cursor(encoded: &str) -> std::result::Result<CoreEventRangeCursor, ExportEventsError> {
    if encoded.len() > MAX_CURSOR_CHARS {
        return Err(ExportEventsError::CursorTooLarge {
            actual: encoded.len(),
            maximum: MAX_CURSOR_CHARS,
        });
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ExportEventsError::InvalidCursorEncoding)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err(ExportEventsError::InvalidCursorEncoding);
    }
    Ok(CoreEventRangeCursor::decode(&bytes)?)
}

fn encode_cursor(cursor: &CoreEventRangeCursor) -> String {
    URL_SAFE_NO_PAD.encode(cursor.encode())
}

fn parse_rfc3339(field: &'static str, value: &str) -> std::result::Result<i64, ExportEventsError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.timestamp_millis())
        .map_err(|_| ExportEventsError::InvalidTimestamp {
            field,
            value: value.to_owned(),
        })
}

fn export_error_value(error: &ExportEventsError) -> Value {
    let code = match error {
        ExportEventsError::Range(CoreEventRangeError::Index(
            IndexError::PinnedGenerationNotRetained { .. },
        )) => "generation_not_retained",
        ExportEventsError::Range(CoreEventRangeError::CursorSelectionMismatch) => {
            "cursor_request_mismatch"
        }
        ExportEventsError::Range(CoreEventRangeError::CursorGenerationMismatch { .. }) => {
            "cursor_generation_mismatch"
        }
        ExportEventsError::Range(CoreEventRangeError::InvalidCursor)
        | ExportEventsError::InvalidCursorEncoding
        | ExportEventsError::CursorTooLarge { .. } => "invalid_cursor",
        ExportEventsError::Range(CoreEventRangeError::InvalidCursorCoordinate) => {
            "invalid_cursor_coordinate"
        }
        ExportEventsError::Range(CoreEventRangeError::InvalidRange { .. })
        | ExportEventsError::InvalidTimestamp { .. } => "invalid_range",
        ExportEventsError::WireRecordTooLarge { .. } => "event_too_large",
        ExportEventsError::WireEnvelopeTooLarge { .. } => "page_too_large",
        _ => "event_export_failed",
    };
    let cursor = match error {
        ExportEventsError::WireRecordTooLarge { cursor, .. } => cursor.as_deref(),
        _ => None,
    };
    json!({
        "schema_version": EXPORT_SCHEMA_VERSION,
        "error_code": code,
        "detail": error.to_string(),
        "cursor": cursor,
    })
}

#[cfg(test)]
#[path = "export/tests.rs"]
mod tests;

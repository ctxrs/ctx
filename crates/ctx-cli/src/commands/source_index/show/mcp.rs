use std::path::Path;

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ctx_history_core::{MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES};
use ctx_history_index::{
    CoreEventPageBudget, CoreEventRecord, SessionEventCoordinate, SessionEventCursor,
    SessionRecord, VerifiedIndex,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    output::{compact_json, OutputFormat},
    presentation_limit::{enforce_presentation_output_limit, serialized_json_bytes},
    transcript::TranscriptMode,
};

use super::{
    event_window, event_window_json, render_event_value, session_transcript_value,
    SessionEventSelector, CLI_SESSION_EVENT_PAGE_ITEMS,
};
use crate::commands::source_index::{
    render::enforce_json_output_limit,
    shared::{open_index, resolve_core_event, resolve_session},
};

pub(crate) fn mcp_show_session(
    data_root: &Path,
    id: &str,
    mode: TranscriptMode,
    limit: usize,
    cursor: Option<&str>,
    output_limit_bytes: usize,
) -> Result<Value> {
    let index = open_index(data_root)?;
    let session = resolve_session(&index, id)?;
    let cursor = cursor.map(decode_session_event_cursor).transpose()?;
    let (rendered, has_more, next_cursor) =
        collect_selected_session_page(&index, &session, mode, limit, cursor, output_limit_bytes)?;
    let returned = rendered.len();
    let mut value =
        session_transcript_value(&session, mode, OutputFormat::Json, rendered, false, None);
    value["pagination"] = compact_json(json!({
        "limit": limit,
        "returned": returned,
        "has_more": has_more,
        "next_cursor": next_cursor.as_ref().map(encode_session_event_cursor).transpose()?,
    }));
    let event_id = value["events"]
        .as_array()
        .and_then(|events| events.last())
        .and_then(|event| event["ctx_event_id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .unwrap_or_else(|| session.session_id.as_uuid());
    enforce_json_output_limit(&value, output_limit_bytes, event_id)?;
    Ok(value)
}

fn collect_selected_session_page(
    index: &VerifiedIndex,
    session: &SessionRecord,
    mode: TranscriptMode,
    limit: usize,
    mut cursor: Option<SessionEventCursor>,
    output_limit_bytes: usize,
) -> Result<(Vec<Value>, bool, Option<SessionEventCursor>)> {
    let mut selector = SessionEventSelector::new(mode);
    let mut selected = Vec::with_capacity(limit);
    let mut serialized_event_bytes = 2_usize;
    let mut continuation = None;
    let mut has_more = false;

    'pages: loop {
        let page = index.core_session_event_page_with_budget(
            session.session_id.as_uuid(),
            cursor.as_ref(),
            CLI_SESSION_EVENT_PAGE_ITEMS,
            CoreEventPageBudget::new(
                output_limit_bytes.clamp(1, MAX_ENCODED_CORE_RECORD_BYTES),
                output_limit_bytes.clamp(1, MAX_CORE_CONTENT_BYTES),
            ),
        )?;
        let terminal = page.terminal;
        let next_page_cursor = page.next_cursor;
        for event in page.items {
            for event in selector.push(event) {
                if !retain_mcp_selected_event(
                    index,
                    session,
                    event,
                    limit,
                    output_limit_bytes,
                    &mut selected,
                    &mut serialized_event_bytes,
                    &mut continuation,
                )? {
                    has_more = true;
                    break 'pages;
                }
            }
        }
        if terminal {
            if let Some(event) = selector.finish() {
                if !retain_mcp_selected_event(
                    index,
                    session,
                    event,
                    limit,
                    output_limit_bytes,
                    &mut selected,
                    &mut serialized_event_bytes,
                    &mut continuation,
                )? {
                    has_more = true;
                }
            }
            break;
        }
        cursor = Some(next_page_cursor.ok_or_else(|| {
            anyhow!("nonterminal Core session event page omitted its continuation cursor")
        })?);
    }

    if !has_more {
        continuation = None;
    }
    Ok((selected, has_more, continuation))
}

#[allow(clippy::too_many_arguments)]
fn retain_mcp_selected_event(
    index: &VerifiedIndex,
    session: &SessionRecord,
    event: CoreEventRecord,
    limit: usize,
    output_limit_bytes: usize,
    selected: &mut Vec<Value>,
    serialized_event_bytes: &mut usize,
    continuation: &mut Option<SessionEventCursor>,
) -> Result<bool> {
    if selected.len() == limit {
        return Ok(false);
    }
    let event_id = event.event_id.as_uuid();
    let value = render_event_value(&event);
    let candidate_bytes = serialized_event_bytes
        .saturating_add(usize::from(!selected.is_empty()))
        .saturating_add(serialized_json_bytes(&value)?);
    if candidate_bytes > output_limit_bytes {
        if selected.is_empty() {
            enforce_presentation_output_limit(candidate_bytes, output_limit_bytes, event_id)?;
        }
        return Ok(false);
    }
    *serialized_event_bytes = candidate_bytes;
    *continuation = Some(cursor_after_event(index, session, &event));
    selected.push(value);
    Ok(true)
}

fn cursor_after_event(
    index: &VerifiedIndex,
    session: &SessionRecord,
    event: &CoreEventRecord,
) -> SessionEventCursor {
    SessionEventCursor::new(
        index.generation_id(),
        session.session_id,
        SessionEventCoordinate {
            event_id: event.event_id.as_uuid(),
            event_sequence: event.event_sequence,
            occurred_at_unix_ms: event.occurred_at_unix_ms,
        },
    )
}

fn encode_session_event_cursor(cursor: &SessionEventCursor) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor)?))
}

fn decode_session_event_cursor(encoded: &str) -> Result<SessionEventCursor> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        anyhow::Error::new(ctx_history_index::IndexError::InvalidSessionEventCursorCoordinate)
    })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::Error::new(ctx_history_index::IndexError::InvalidSessionEventCursorCoordinate)
    })
}

pub(crate) fn mcp_show_event(
    data_root: &Path,
    id: &str,
    before: usize,
    after: usize,
    window: Option<usize>,
    output_limit_bytes: usize,
) -> Result<Value> {
    let index = open_index(data_root)?;
    let selected = resolve_core_event(&index, id)?;
    let events = event_window(&index, &selected, before, after, window, output_limit_bytes)?;
    let value = event_window_json(&selected, &events, OutputFormat::Json, output_limit_bytes)?;
    enforce_json_output_limit(&value, output_limit_bytes, selected.event_id.as_uuid())?;
    Ok(value)
}

//! Source-backed Warp parsing and exact task-message hydration.

use rusqlite::Connection;

use super::schema::WarpSqliteSchema;
use crate::{CaptureError, Result};

mod decode;
mod model;
mod query;

use model::WarpNativeEventIdentity;
pub(super) use model::{
    WarpNativeCounters, WarpNativeEvent, WarpNativeMessageIdentity, WarpNativePage,
    WarpNativeSession, WarpNativeSink,
};
use query::scan_warp_native_pinned_snapshot;

pub(in super::super) struct WarpNativeSourceBackedScan {
    pub(in super::super) source_integrity_digest: String,
    pub(in super::super) counters: WarpNativeCounters,
}

/// Runs the Warp parser against a caller-owned, certified SQLite read
/// connection. Publication and source lifecycle are owned by the shared
/// source-backed coordinator.
pub(in super::super) fn scan_warp_source_backed_connection(
    connection: &Connection,
    sink: &mut dyn WarpNativeSink,
) -> Result<WarpNativeSourceBackedScan> {
    let schema = WarpSqliteSchema::detect(connection)?;
    validate_source_paging_compatibility(connection)?;
    let result = scan_warp_native_pinned_snapshot(connection, &schema, sink)?;
    Ok(WarpNativeSourceBackedScan {
        source_integrity_digest: result.source_integrity_digest,
        counters: result.counters,
    })
}

pub(super) fn resolve_warp_task_message(
    task_bytes: &[u8],
    conversation_id: &str,
    fallback_task_id: &str,
    message_index: usize,
) -> Result<Option<super::WarpTaskContent>> {
    let decoded = decode::decode_warp_native_task(task_bytes)?;
    let task_id = fallback_task_id;
    let message_ordinal = u32::try_from(message_index)
        .map_err(|_| CaptureError::InvalidPayload("Warp message locator exceeds u32".to_owned()))?;
    let Some(message) = decoded
        .messages
        .into_iter()
        .find(|message| message.message_ordinal == message_ordinal)
    else {
        return Ok(None);
    };
    let decode::WarpDecodedMessagePayload::Retained(retained) = message.payload else {
        return Ok(None);
    };
    let native_record_id = message
        .message_id
        .clone()
        .unwrap_or_else(|| format!("{task_id}:{}", message.message_ordinal));
    let identity = WarpNativeEventIdentity {
        conversation_id: conversation_id.to_owned(),
        task_id: task_id.to_owned(),
        message: message
            .message_id
            .map(model::WarpNativeMessageIdentity::ProviderId)
            .unwrap_or(model::WarpNativeMessageIdentity::MessageOrdinal(
                message.message_ordinal,
            )),
    };
    let normalized_payload_hash =
        model::normalized_retained_event_hash(&identity, &retained.body, None, None)?;
    Ok(Some(super::WarpTaskContent {
        event_type: retained.event_type,
        native_record_id,
        text: retained.body,
        normalized_payload_hash: Some(normalized_payload_hash),
    }))
}

fn validate_source_paging_compatibility(connection: &Connection) -> Result<()> {
    let invalid_rowid: bool = connection.query_row(
        "select exists(select 1 from agent_conversations where rowid <= 0)
             or exists(select 1 from agent_tasks where rowid <= 0)",
        [],
        |row| row.get(0),
    )?;
    if invalid_rowid {
        return Err(CaptureError::InvalidPayload(
            "Warp source-backed paging requires positive 64-bit source rowids".to_owned(),
        ));
    }
    Ok(())
}

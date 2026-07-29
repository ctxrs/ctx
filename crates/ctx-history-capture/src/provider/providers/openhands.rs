#[cfg(test)]
use std::path::Path;

#[cfg(test)]
use ctx_history_store::Store;

use crate::Result;

mod event;
pub(crate) mod nativepath;
mod source;

pub(crate) use event::openhands_result_content;
#[allow(unused_imports)]
pub(crate) use event::{decode_openhands_event, decode_openhands_event_value};
pub(crate) use nativepath::import_openhands_nativepath;
#[allow(unused_imports)]
pub(crate) use nativepath::{
    project_openhands_source_backed_v1, OpenHandsHydratedRecordV1, OpenHandsLocatorResolverV1,
    OpenHandsRejectedEventV1, OpenHandsSourceBackedAdapterV1, OpenHandsSourceBackedErrorV1,
    OpenHandsSourceBackedProjectionV1, OpenHandsSourceBackedResultV1,
};

fn openhands_bounded_derived_text(value: String, field: &str) -> Result<String> {
    const MAX_DERIVED_TEXT_BYTES: usize = 16 * 1024;
    if value.len() > MAX_DERIVED_TEXT_BYTES {
        return Err(crate::CaptureError::InvalidPayload(format!(
            "OpenHands {field} exceeds {MAX_DERIVED_TEXT_BYTES} bytes"
        )));
    }
    Ok(value)
}

#[cfg(test)]
pub(crate) use source::count_openhands_source_file_opens;

#[cfg(test)]
pub(crate) fn seed_c213_openhands_terminal_cursor(
    store: &Store,
    path: &Path,
    machine_id: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    use ctx_history_core::{new_id, EntityTimestamps, SyncCursor};

    use crate::{
        native_source::NativePosition,
        provider::importer::{BoundedParserCheckpoint, CertifiedProviderCursor},
    };

    let source = source::OpenHandsObservedFile::open(path)?;
    let position = NativePosition::new("openhands-event-file-v1", 1_u64.to_be_bytes().to_vec())
        .map_err(|error| crate::CaptureError::InvalidPayload(error.to_string()))?;
    let cursor = CertifiedProviderCursor::new(
        source.cursor_revision(None),
        2,
        1,
        position,
        BoundedParserCheckpoint::from_serializable(&serde_json::json!({
            "next_position": 1,
            "accepted_events": 1,
            "accepted_file_touches": 1,
            "rejection": null,
        }))?,
    )?;
    store.upsert_sync_cursor(&SyncCursor {
        id: new_id(),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream: source.cursor_stream,
        cursor: cursor.encode()?,
        last_synced_at: Some(observed_at),
        timestamps: EntityTimestamps {
            created_at: observed_at,
            updated_at: observed_at,
        },
    })?;
    Ok(())
}

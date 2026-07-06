use ctx_history_core::{
    CaptureProvider, ProviderCaptureEnvelope, ProviderCursorCheckpoint, RedactionState, SyncCursor,
};
use ctx_history_store::Store;

use crate::{stable_capture_uuid, Result};

use super::ids::timestamps;

pub(crate) fn provider_cursor_stream(provider: CaptureProvider, source_format: &str) -> String {
    format!("provider:{}:{}", provider.as_str(), source_format)
}

pub(crate) fn effective_event_redaction_state(
    requested: RedactionState,
    sanitizer_redacted: bool,
) -> RedactionState {
    match requested {
        RedactionState::Withheld => RedactionState::Withheld,
        RedactionState::Redacted => RedactionState::Redacted,
        RedactionState::Raw if !sanitizer_redacted => RedactionState::Raw,
        _ if sanitizer_redacted => RedactionState::Redacted,
        _ => RedactionState::LocalPreview,
    }
}

pub(crate) fn persist_provider_cursor(
    store: &mut Store,
    capture: &ProviderCaptureEnvelope,
) -> Result<()> {
    let checkpoint = capture
        .source
        .cursor
        .as_ref()
        .and_then(|cursor| cursor.after.as_ref())
        .cloned()
        .or_else(|| {
            capture.event.as_ref().and_then(|event| {
                event
                    .cursor
                    .as_ref()
                    .map(|cursor| ProviderCursorCheckpoint {
                        stream: provider_cursor_stream(
                            capture.provider,
                            &capture.source.source_format,
                        ),
                        cursor: cursor.clone(),
                        observed_at: event.occurred_at,
                    })
            })
        });
    let Some(checkpoint) = checkpoint else {
        return Ok(());
    };

    store.upsert_sync_cursor(&SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                capture.provider.as_str(),
                capture.source.machine_id,
                checkpoint.stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: capture.source.machine_id.clone(),
        stream: checkpoint.stream,
        cursor: checkpoint.cursor,
        last_synced_at: Some(checkpoint.observed_at),
        timestamps: timestamps(checkpoint.observed_at),
    })?;
    Ok(())
}

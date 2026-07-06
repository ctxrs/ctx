use ctx_history_core::{
    CaptureProvider, ProviderCaptureEnvelope, ProviderCursorCheckpoint, ProviderCursorRange,
    ProviderSourceEnvelope, RedactionState, SyncCursor,
};
use ctx_history_store::Store;

use crate::{stable_capture_uuid, Result};

use super::ids::{provider_source_identity_component, timestamps};

pub(crate) fn provider_cursor_stream(provider: CaptureProvider, source_format: &str) -> String {
    format!("provider:{}:{}", provider.as_str(), source_format)
}

#[cfg(test)]
pub(crate) fn provider_source_cursor_stream(
    provider: CaptureProvider,
    source_format: &str,
    source_root: Option<&str>,
) -> String {
    provider_source_cursor_stream_for_component(
        provider,
        source_format,
        provider_source_identity_component(source_root, None, None, &serde_json::Value::Null)
            .unwrap_or(("default", "default".to_owned())),
    )
}

pub(crate) fn provider_source_cursor_range(
    capture: &ProviderCaptureEnvelope,
) -> Option<ProviderCursorRange> {
    if capture.provider == CaptureProvider::Custom {
        return capture.source.cursor.clone();
    }
    capture
        .source
        .cursor
        .as_ref()
        .map(|cursor| ProviderCursorRange {
            before: cursor
                .before
                .as_ref()
                .map(|checkpoint| source_scoped_checkpoint(capture, checkpoint)),
            after: cursor
                .after
                .as_ref()
                .map(|checkpoint| source_scoped_checkpoint(capture, checkpoint)),
        })
}

fn source_scoped_checkpoint(
    capture: &ProviderCaptureEnvelope,
    checkpoint: &ProviderCursorCheckpoint,
) -> ProviderCursorCheckpoint {
    if capture.provider == CaptureProvider::Custom {
        return checkpoint.clone();
    }
    ProviderCursorCheckpoint {
        stream: provider_source_cursor_stream_for_source(capture.provider, &capture.source),
        cursor: checkpoint.cursor.clone(),
        observed_at: checkpoint.observed_at,
    }
}

pub(crate) fn provider_source_cursor_stream_for_source(
    provider: CaptureProvider,
    source: &ProviderSourceEnvelope,
) -> String {
    provider_source_cursor_stream_for_component(
        provider,
        &source.source_format,
        provider_source_identity_component(
            source.source_root.as_deref(),
            source.raw_source_path.as_deref(),
            source.idempotency_key.as_deref(),
            &source.metadata,
        )
        .unwrap_or(("default", "default".to_owned())),
    )
}

fn provider_source_cursor_stream_for_component(
    provider: CaptureProvider,
    source_format: &str,
    component: (&'static str, String),
) -> String {
    let (component_kind, component_value) = component;
    let key = serde_json::to_string(&(
        "provider-source-cursor-v1",
        provider.as_str(),
        source_format,
        component_kind,
        component_value,
    ))
    .expect("provider cursor source identity key should serialize");
    let source_id = stable_capture_uuid(&key, "provider-cursor-source");
    format!(
        "{}:source:{}",
        provider_cursor_stream(provider, source_format),
        source_id.simple()
    )
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
        .and_then(|cursor| {
            cursor
                .after
                .as_ref()
                .map(|after| source_scoped_checkpoint(capture, after))
        })
        .or_else(|| {
            capture.event.as_ref().and_then(|event| {
                event
                    .cursor
                    .as_ref()
                    .map(|cursor| ProviderCursorCheckpoint {
                        stream: provider_source_cursor_stream_for_source(
                            capture.provider,
                            &capture.source,
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

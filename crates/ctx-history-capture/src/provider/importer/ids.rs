use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, EntityTimestamps, Fidelity, SyncMetadata, SyncState, Visibility,
};
use serde_json::Value;
use uuid::Uuid;

use crate::{fnv1a64, stable_capture_uuid};

#[cfg(test)]
pub(crate) fn provider_source_uuid(provider: CaptureProvider, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &format!("provider:{}:{provider_session_id}", provider.as_str()),
        "source",
    )
}

pub(crate) fn provider_scoped_source_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
) -> Uuid {
    stable_capture_uuid(
        &provider_scoped_source_identity_key(
            provider,
            provider_session_id,
            source_format,
            raw_source_path,
        ),
        "source",
    )
}

pub(crate) fn provider_scoped_source_identity_key(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
) -> String {
    serde_json::to_string(&(
        "provider-source-v2",
        provider.as_str(),
        provider_session_id,
        source_format,
        raw_source_path,
    ))
    .expect("provider source identity key should serialize")
}

pub(crate) fn provider_session_uuid(provider: CaptureProvider, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &format!("provider:{}:{provider_session_id}", provider.as_str()),
        "session",
    )
}

pub(crate) fn provider_run_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    run_key: &str,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:run:{run_key}",
            provider.as_str()
        ),
        "run",
    )
}

pub(crate) fn provider_source_run_uuid(source_id: Uuid, run_key: &str) -> Uuid {
    stable_capture_uuid(&format!("provider-source:{source_id}:run:{run_key}"), "run")
}

pub(crate) fn provider_event_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_event_index: u64,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:{provider_event_index}",
            provider.as_str()
        ),
        "event",
    )
}

pub(crate) fn provider_event_seq(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_event_index: u64,
) -> u64 {
    let session_key = format!("provider:{}:{provider_session_id}", provider.as_str());
    ((fnv1a64(session_key.as_bytes()) & 0x0000_07ff_ffff_ffff) << 20)
        | (provider_event_index & 0x000f_ffff)
}

pub(crate) fn provider_source_event_uuid(source_id: Uuid, provider_event_index: u64) -> Uuid {
    stable_capture_uuid(
        &format!("provider-source:{source_id}:event:{provider_event_index}"),
        "event",
    )
}

pub(crate) fn provider_file_touch_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_touch_index: u64,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:file-touch:{provider_touch_index}",
            provider.as_str()
        ),
        "file-touch",
    )
}

pub(crate) fn provider_source_file_touch_uuid(source_id: Uuid, provider_touch_index: u64) -> Uuid {
    stable_capture_uuid(
        &format!("provider-source:{source_id}:file-touch:{provider_touch_index}"),
        "file-touch",
    )
}

pub(crate) fn provider_source_event_seq(source_id: Uuid, provider_event_index: u64) -> u64 {
    let source_key = source_id.to_string();
    ((fnv1a64(source_key.as_bytes()) & 0x0000_0000_7fff_ffff) << 32)
        | (provider_event_index & 0xffff_ffff)
}

pub(crate) fn provider_edge_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    edge_kind: &str,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:{edge_kind}",
            provider.as_str()
        ),
        "session-edge",
    )
}

pub(crate) fn timestamps(at: DateTime<Utc>) -> EntityTimestamps {
    EntityTimestamps {
        created_at: at,
        updated_at: at,
    }
}

pub(crate) fn provider_sync_metadata(fidelity: Fidelity, metadata: Value) -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::default(),
        fidelity,
        sync_state: SyncState::default(),
        sync_version: 0,
        deleted_at: None,
        metadata,
    }
}

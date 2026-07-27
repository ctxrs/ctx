use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, EntityTimestamps, Fidelity, SyncMetadata, SyncState, Visibility,
};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    fnv1a64, provider::file_touches::MAX_PACKED_PROVIDER_EVENT_INDEX, stable_capture_uuid,
};

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

pub(crate) fn provider_source_root(
    source_root: Option<&str>,
    raw_source_path: Option<&str>,
) -> Option<String> {
    source_root
        .map(str::trim)
        .filter(|root| !root.is_empty())
        .or_else(|| {
            raw_source_path
                .map(str::trim)
                .filter(|path| !path.is_empty())
        })
        .map(str::to_owned)
}

pub(crate) fn provider_source_identity(
    provider: CaptureProvider,
    source_format: &str,
    source_root: Option<&str>,
    raw_source_path: Option<&str>,
    source_idempotency_key: Option<&str>,
    source_metadata: &Value,
) -> Option<String> {
    provider_source_identity_component(
        source_root,
        raw_source_path,
        source_idempotency_key,
        source_metadata,
    )
    .map(|(component_kind, component_value)| {
        provider_source_identity_from_component(
            provider,
            source_format,
            component_kind,
            &component_value,
        )
    })
}

pub(crate) fn provider_source_identity_component(
    source_root: Option<&str>,
    raw_source_path: Option<&str>,
    source_idempotency_key: Option<&str>,
    source_metadata: &Value,
) -> Option<(&'static str, String)> {
    if let Some(root) = source_root.and_then(normalized_source_identity_value) {
        return Some(("root", root));
    }
    if let Some(path) = raw_source_path.and_then(normalized_source_identity_value) {
        return Some(("path", path));
    }
    for key in [
        "source_id",
        "native_source_id",
        "workspace_id",
        "native_workspace_id",
        "root_id",
    ] {
        if let Some(value) = source_metadata
            .get(key)
            .and_then(Value::as_str)
            .and_then(normalized_source_identity_value)
        {
            return Some((key, value));
        }
    }
    source_idempotency_key
        .and_then(normalized_source_identity_value)
        .map(|idempotency_key| ("idempotency_key", idempotency_key))
}

pub(crate) fn provider_source_identity_key(
    provider: CaptureProvider,
    source_format: &str,
    component_kind: &str,
    component_value: &str,
) -> String {
    serde_json::to_string(&(
        "provider-source-identity-v1",
        provider.as_str(),
        source_format,
        component_kind,
        component_value,
    ))
    .expect("provider source identity key should serialize")
}

pub(crate) fn provider_source_identity_from_component(
    provider: CaptureProvider,
    source_format: &str,
    component_kind: &str,
    component_value: &str,
) -> String {
    stable_capture_uuid(
        &provider_source_identity_key(provider, source_format, component_kind, component_value),
        "provider-source-root",
    )
    .to_string()
}

#[cfg(test)]
pub(crate) fn provider_source_root_identity_key(
    provider: CaptureProvider,
    source_format: &str,
    source_root: &str,
) -> String {
    provider_source_identity_key(
        provider,
        source_format,
        "root",
        &normalized_source_identity_value(source_root).unwrap_or_else(|| source_root.to_owned()),
    )
}

#[cfg(test)]
pub(crate) fn provider_source_root_identity(
    provider: CaptureProvider,
    source_format: &str,
    source_root: &str,
) -> String {
    stable_capture_uuid(
        &provider_source_root_identity_key(provider, source_format, source_root),
        "provider-source-root",
    )
    .to_string()
}

pub(crate) fn normalized_source_identity_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut normalized = value.replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    Some(normalized)
}

pub(crate) fn provider_session_uuid(provider: CaptureProvider, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &format!("provider:{}:{provider_session_id}", provider.as_str()),
        "session",
    )
}

pub(crate) fn provider_source_session_uuid(
    source_identity: &str,
    provider_session_id: &str,
) -> Uuid {
    stable_capture_uuid(
        &format!("provider-source-root:{source_identity}:session:{provider_session_id}"),
        "session",
    )
}

pub(crate) fn provider_source_edge_uuid(
    source_identity: &str,
    provider_session_id: &str,
    edge_kind: &str,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider-source-root:{source_identity}:session:{provider_session_id}:{edge_kind}"
        ),
        "session-edge",
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

// Low event indices keep the historical packed touch UUID exactly. Above that boundary the
// producer exposes the per-event ordinal as `provider_touch_index`, so the full event index must
// participate in the UUID key.
pub(crate) fn provider_event_file_touch_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_event_index: Option<u64>,
    provider_touch_index: u64,
) -> Uuid {
    let Some(provider_event_index) =
        provider_event_index.filter(|index| *index > MAX_PACKED_PROVIDER_EVENT_INDEX)
    else {
        return provider_file_touch_uuid(provider, provider_session_id, provider_touch_index);
    };
    let identity_key = serde_json::to_string(&(
        "provider-event-file-touch-v2",
        provider.as_str(),
        provider_session_id,
        provider_event_index,
        provider_touch_index,
    ))
    .expect("provider event file-touch identity key should serialize");
    stable_capture_uuid(&identity_key, "file-touch")
}

pub(crate) fn provider_source_file_touch_uuid(source_id: Uuid, provider_touch_index: u64) -> Uuid {
    stable_capture_uuid(
        &format!("provider-source:{source_id}:file-touch:{provider_touch_index}"),
        "file-touch",
    )
}

pub(crate) fn provider_source_event_file_touch_uuid(
    source_id: Uuid,
    provider_event_index: Option<u64>,
    provider_touch_index: u64,
) -> Uuid {
    let Some(provider_event_index) =
        provider_event_index.filter(|index| *index > MAX_PACKED_PROVIDER_EVENT_INDEX)
    else {
        return provider_source_file_touch_uuid(source_id, provider_touch_index);
    };
    let identity_key = serde_json::to_string(&(
        "provider-source-event-file-touch-v2",
        source_id,
        provider_event_index,
        provider_touch_index,
    ))
    .expect("provider source event file-touch identity key should serialize");
    stable_capture_uuid(&identity_key, "file-touch")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_event_file_touch_uuids_retain_packed_identity_compatibility() {
        let provider = CaptureProvider::OpenHands;
        let provider_session_id = "legacy-packed-session";
        let source_id = Uuid::parse_str("4ea89d63-c113-4fe8-93e5-12859eb2aac7").unwrap();
        for provider_event_index in [17, MAX_PACKED_PROVIDER_EVENT_INDEX] {
            let provider_touch_index = (provider_event_index << 16) | 3;

            assert_eq!(
                provider_event_file_touch_uuid(
                    provider,
                    provider_session_id,
                    Some(provider_event_index),
                    provider_touch_index,
                ),
                provider_file_touch_uuid(provider, provider_session_id, provider_touch_index)
            );
            assert_eq!(
                provider_source_event_file_touch_uuid(
                    source_id,
                    Some(provider_event_index),
                    provider_touch_index,
                ),
                provider_source_file_touch_uuid(source_id, provider_touch_index)
            );
        }
    }

    #[test]
    fn full_width_event_file_touch_uuids_do_not_alias_packed_event_zero() {
        let provider = CaptureProvider::OpenHands;
        let provider_session_id = "hash-indexed-session";
        let source_id = Uuid::parse_str("4ea89d63-c113-4fe8-93e5-12859eb2aac7").unwrap();
        let event_zero_provider_id =
            provider_event_file_touch_uuid(provider, provider_session_id, Some(0), 0);
        let event_zero_source_id = provider_source_event_file_touch_uuid(source_id, Some(0), 0);

        assert_ne!(
            provider_event_file_touch_uuid(provider, provider_session_id, Some(1_u64 << 48), 0),
            event_zero_provider_id
        );
        assert_ne!(
            provider_source_event_file_touch_uuid(source_id, Some(1_u64 << 48), 0),
            event_zero_source_id
        );
        assert_ne!(
            provider_event_file_touch_uuid(provider, provider_session_id, Some(1_u64 << 48), 0,),
            provider_event_file_touch_uuid(provider, provider_session_id, Some(1_u64 << 48), 1,)
        );
        assert_ne!(
            provider_source_event_file_touch_uuid(source_id, Some(1_u64 << 48), 0),
            provider_source_event_file_touch_uuid(source_id, Some(1_u64 << 48), 1)
        );

        let full_hash = 0xfedc_ba98_7654_3210;
        assert_ne!(
            provider_event_file_touch_uuid(provider, provider_session_id, Some(full_hash), 0),
            event_zero_provider_id
        );
        assert_ne!(
            provider_source_event_file_touch_uuid(source_id, Some(full_hash), 0),
            event_zero_source_id
        );
    }
}

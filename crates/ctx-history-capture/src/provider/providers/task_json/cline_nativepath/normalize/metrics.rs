use super::*;

pub(in super::super) fn estimated_event_bytes(row: &ClineEventRow) -> usize {
    // This is the exact size of the provider-owned length-prefixed page
    // encoding. Strings and byte arrays carry an eight-byte length. Optional
    // values carry a one-byte presence tag.
    encoded_str(row.identity.task.as_str())
        .saturating_add(1)
        .saturating_add(estimated_native_key_bytes(&row.identity.item))
        .saturating_add(4)
        .saturating_add(1 + 8 + 4)
        .saturating_add(1 + 1)
        .saturating_add(encoded_option_i64(row.occurred_at_millis))
        .saturating_add(encoded_option_str(row.body.as_deref()))
        .saturating_add(32)
        .saturating_add(1 + usize::from(row.source_record.is_some()) * (8 + 8 + 8 + 32))
        .saturating_add(row.tool_call.as_ref().map_or(1, |call| {
            1_usize
                .saturating_add(encoded_option_str(call.call_id.as_deref()))
                .saturating_add(encoded_option_str(call.name.as_deref()))
        }))
        .saturating_add(row.sparse_output.as_ref().map_or(1, |output| {
            1_usize
                .saturating_add(1)
                .saturating_add(encoded_option_i32(output.exit_code))
                .saturating_add(encoded_option_u64(output.duration_ms))
                .saturating_add(8)
                .saturating_add(encoded_option_str(output.call_id.as_deref()))
        }))
        .saturating_add(8)
        .saturating_add(row.file_touches.iter().fold(0_usize, |bytes, touch| {
            bytes
                .saturating_add(encoded_str(&touch.path))
                .saturating_add(encoded_option_str(touch.old_path.as_deref()))
                .saturating_add(1)
                .saturating_add(1)
                .saturating_add(encoded_str(
                    &serde_json::to_string(&touch.metadata)
                        .expect("file-touch metadata should serialize"),
                ))
        }))
}

pub(in super::super) fn estimated_session_bytes(session: &ClineSessionRow) -> usize {
    encoded_str(session.identity.as_str())
        .saturating_add(1)
        .saturating_add(8)
        .saturating_add(
            session
                .identity_aliases
                .iter()
                .map(|alias| encoded_str(alias.as_str()))
                .sum::<usize>(),
        )
        .saturating_add(encoded_option_str(session.title.as_deref()))
        .saturating_add(encoded_option_str(session.workspace_directory.as_deref()))
        .saturating_add(encoded_option_str(session.created_at.as_deref()))
        .saturating_add(encoded_option_str(session.last_modified.as_deref()))
        .saturating_add(encoded_option_str(session.model_id.as_deref()))
        .saturating_add(encoded_option_str(session.model_provider.as_deref()))
        .saturating_add(encoded_option_u64(session.tokens_input))
        .saturating_add(encoded_option_u64(session.tokens_output))
        .saturating_add(32)
}

pub(in super::super) fn estimated_rejection_bytes(rejection: &ClineItemRejection) -> usize {
    1_usize
        .saturating_add(8)
        .saturating_add(encoded_option_str(rejection.native_id.as_deref()))
        .saturating_add(1)
        .saturating_add(8)
        .saturating_add(encoded_str(&rejection.detail))
}

pub(in super::super) fn estimated_source_bytes(source: &ClineFileSourceIdentity) -> usize {
    1_usize.saturating_add(encoded_bytes(
        source.canonical_path.as_os_str().as_encoded_bytes(),
    ))
}

fn estimated_native_key_bytes(key: &ClineNativeItemKey) -> usize {
    match key {
        ClineNativeItemKey::NativeId {
            native_id,
            occurrence: _,
        } => 1_usize
            .saturating_add(encoded_str(native_id))
            .saturating_add(8),
        ClineNativeItemKey::ComponentOrdinal(_) => 1 + 8,
    }
}

fn encoded_str(value: &str) -> usize {
    encoded_bytes(value.as_bytes())
}

fn encoded_bytes(value: &[u8]) -> usize {
    8_usize.saturating_add(value.len())
}

fn encoded_option_str(value: Option<&str>) -> usize {
    1_usize.saturating_add(value.map_or(0, encoded_str))
}

fn encoded_option_i32(value: Option<i32>) -> usize {
    1 + usize::from(value.is_some()) * 4
}

fn encoded_option_i64(value: Option<i64>) -> usize {
    1 + usize::from(value.is_some()) * 8
}

fn encoded_option_u64(value: Option<u64>) -> usize {
    1 + usize::from(value.is_some()) * 8
}

pub(super) fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

pub(super) fn hash_native_key(hasher: &mut Sha256, key: &ClineNativeItemKey) {
    match key {
        ClineNativeItemKey::NativeId {
            native_id,
            occurrence,
        } => {
            hasher.update(b"id\0");
            hasher.update(native_id.as_bytes());
            hasher.update(occurrence.to_le_bytes());
        }
        ClineNativeItemKey::ComponentOrdinal(ordinal) => {
            hasher.update(b"ordinal\0");
            hasher.update(ordinal.to_le_bytes());
        }
    }
    hasher.update(b"\0");
}

use super::*;

pub(super) fn page_publication_id(
    dialect: TaskJsonNativeDialect,
    source: &NativeSourceIdentity,
    page: &NativeIngestionPage<ClineCertifiedPage>,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(dialect.page_publication_domain);
    digest.update(source.provider().as_bytes());
    digest.update(source.source_identity().as_bytes());
    digest.update(page.core.identity.as_bytes());
    digest.update(page.expected_frontier.version.to_le_bytes());
    digest.update(&page.expected_frontier.bytes);
    digest.update(page.next_safe_frontier.version.to_le_bytes());
    digest.update(&page.next_safe_frontier.bytes);
    digest.update([u8::from(page.terminal)]);
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!(
        "{}{}",
        dialect.page_publication_prefix,
        hex(&digest.finalize())
    )
}

pub(super) fn packed_event_index(
    event: &ClineEventRow,
) -> std::result::Result<u64, ClineNativeVerticalError> {
    let component = u64::from(event.native_order.component as u8);
    let sub_index = u64::from(event.native_order.sub_index);
    let item_index = event.native_order.item_index;
    if item_index >= (TASK_JSON_PACKED_EVENT_INDEX_LIMIT >> 18) || sub_index > 0xffff {
        return Err(ClineNativeVerticalError::EventIndexOverflow);
    }
    Ok((component << 16) | (item_index << 18) | sub_index)
}

pub(super) fn provider_local_event_identity_index(
    event: &ClineEventRow,
) -> std::result::Result<u64, ClineNativeVerticalError> {
    let ClineNativeItemKey::NativeId {
        native_id,
        occurrence,
    } = &event.identity.item
    else {
        return packed_event_index(event);
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx-task-json-provider-local-event-v1\0");
    digest.update([event.identity.component as u8]);
    digest.update((native_id.len() as u64).to_le_bytes());
    digest.update(native_id.as_bytes());
    digest.update(occurrence.to_le_bytes());
    digest.update(event.identity.sub_index.to_le_bytes());
    let bytes = digest.finalize();
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&bytes[..8]);
    let hash = u64::from_le_bytes(prefix);
    Ok((hash & ((1_u64 << 63) - 1)) | (1_u64 << 63))
}

pub(super) fn provider_local_touch_identity_index(
    event_identity_index: u64,
    touch_ordinal: u64,
) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"ctx-task-json-provider-local-touch-v1\0");
    digest.update(event_identity_index.to_le_bytes());
    digest.update(touch_ordinal.to_le_bytes());
    let bytes = digest.finalize();
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(prefix)
}

pub(in super::super) fn released_v025_event_identity(
    source: &ClineFileSourceIdentity,
    event: &ClineEventRow,
) -> std::result::Result<Option<(u64, String)>, ClineNativeVerticalError> {
    if event.native_order.sub_index != 0 {
        return Ok(None);
    }
    let source_name = match event.native_order.component {
        ClineEventComponent::ApiHistory => "api_conversation_history",
        ClineEventComponent::UiMessages => "ui_messages",
        ClineEventComponent::FallbackHistory => "claude_messages",
    };
    let native_id = match &event.identity.item {
        ClineNativeItemKey::NativeId {
            native_id,
            occurrence: _,
        } => native_id.to_string(),
        ClineNativeItemKey::ComponentOrdinal(index) => {
            format!("{source_name}-{index}")
        }
    };
    let ordinal = source
        .released_ordinal_offset
        .checked_add(event.native_order.item_index)
        .ok_or(ClineNativeVerticalError::EventIndexOverflow)?;
    Ok(Some((
        ordinal,
        format!("{}:{source_name}:{native_id}", event.identity.task.as_str()),
    )))
}

pub(super) fn event_type(kind: ClineEventKind) -> EventType {
    match kind {
        ClineEventKind::Message => EventType::Message,
        ClineEventKind::Summary => EventType::Summary,
        ClineEventKind::ToolCall => EventType::ToolCall,
        ClineEventKind::ToolOutput => EventType::ToolOutput,
        ClineEventKind::CommandOutput => EventType::CommandOutput,
        ClineEventKind::Notice => EventType::Notice,
    }
}

pub(super) fn event_role(role: ClineEventRole) -> EventRole {
    match role {
        ClineEventRole::User => EventRole::User,
        ClineEventRole::Assistant => EventRole::Assistant,
        ClineEventRole::System => EventRole::System,
        ClineEventRole::Unknown => EventRole::Unknown,
    }
}

pub(super) fn component_name(component: ClineEventComponent) -> &'static str {
    match component {
        ClineEventComponent::ApiHistory => "api_history",
        ClineEventComponent::UiMessages => "ui_messages",
        ClineEventComponent::FallbackHistory => "fallback_history",
    }
}

pub(super) fn parse_timestamp(value: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    value
        .and_then(crate::common::time::parse_rfc3339_utc)
        .unwrap_or(fallback)
}

pub(super) fn revision(hash: &[u8; 32]) -> String {
    format!("sha256:{}", hex(hash))
}

pub(super) fn task_route_revision(dialect: TaskJsonNativeDialect, task_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-task-json-route-revision-v1\0");
    digest.update(dialect.provider.as_str().as_bytes());
    digest.update(dialect.source_format.as_bytes());
    digest.update((task_id.len() as u64).to_le_bytes());
    digest.update(task_id.as_bytes());
    revision(&digest.finalize().into())
}

pub(super) fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

pub(super) fn map_vertical_error(error: ClineNativeVerticalError) -> CaptureError {
    match error {
        ClineNativeVerticalError::Capture(error) => error,
        ClineNativeVerticalError::Store(error) => CaptureError::Store(error),
        ClineNativeVerticalError::Adapter(error) => CaptureError::InvalidPayload(error.to_string()),
        ClineNativeVerticalError::Source(error) => map_source_error(error),
        ClineNativeVerticalError::SourceChanged => CaptureError::SourceChangedDuringCapture,
        other => CaptureError::InvalidPayload(other.to_string()),
    }
}

pub(super) fn map_source_error(error: ClineNativePathError) -> CaptureError {
    match error {
        ClineNativePathError::SourceChanged { .. } => CaptureError::SourceChangedDuringCapture,
        ClineNativePathError::SourceIo {
            path,
            operation,
            kind,
            raw_os_error,
            message,
        } => CaptureError::SystemIo {
            operation,
            source: std::io::Error::new(
                kind,
                format!("{} (os={raw_os_error:?}): {message}", path.display()),
            ),
        },
        ClineNativePathError::SystemicSource { path, message } => CaptureError::SystemIo {
            operation: "read task JSON NativePath source",
            source: std::io::Error::other(format!("{}: {message}", path.display())),
        },
        other => CaptureError::InvalidPayload(other.to_string()),
    }
}

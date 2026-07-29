use super::*;

pub(super) fn page_identity(page: &WarpNativePage) -> Result<WarpNativePageIdentity> {
    let mut hasher = Sha256::new();
    hasher.update(WARP_PAGE_IDENTITY_DOMAIN);
    hash_usize(
        &mut hasher,
        page.logical_units,
        "Warp NativePath page logical-unit count exceeds u64",
    )?;
    for session in &page.sessions {
        hash_session(&mut hasher, session)?;
    }
    for edge in &page.hierarchy_edges {
        hash_edge(&mut hasher, edge)?;
    }
    for event in &page.events {
        hash_event(&mut hasher, event)?;
    }
    for rejection in &page.rejections {
        hash_rejection(&mut hasher, rejection)?;
    }
    Ok(WarpNativePageIdentity(hasher.finalize().into()))
}

pub(super) fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    value.chars().take(limit).collect()
}

pub(in crate::provider::providers::warp::nativepath) fn normalized_retained_event_hash(
    identity: &WarpNativeEventIdentity,
    complete_body: &str,
    result_outcome: Option<OutputOutcome>,
    call_id: Option<&str>,
) -> Result<String> {
    retained_event_hash(
        identity,
        &truncate_chars(complete_body, WARP_NATIVE_BODY_MAX_CHARS),
        result_outcome,
        call_id,
    )
}

pub(super) fn retained_event_hash(
    identity: &WarpNativeEventIdentity,
    body: &str,
    result_outcome: Option<OutputOutcome>,
    call_id: Option<&str>,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(WARP_EVENT_HASH_DOMAIN);
    hash_text(&mut hasher, &identity.conversation_id)?;
    hash_text(&mut hasher, &identity.task_id)?;
    match &identity.message {
        WarpNativeMessageIdentity::ProviderId(value) => {
            hasher.update([1]);
            hash_text(&mut hasher, value)?;
        }
        WarpNativeMessageIdentity::MessageOrdinal(value) => {
            hasher.update([2]);
            hasher.update(value.to_le_bytes());
        }
    }
    hash_text(&mut hasher, body)?;
    hash_optional_outcome(&mut hasher, result_outcome);
    hash_optional_text(&mut hasher, call_id)?;
    Ok(hex_digest(hasher.finalize().into()))
}

pub(super) fn session_estimated_bytes(session: &WarpNativeSession) -> Result<usize> {
    let metadata_bytes = serde_json::to_vec(&session.metadata)?.len();
    let text = session
        .conversation_id
        .len()
        .saturating_add(
            session
                .parent_conversation_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(session.root_conversation_id.len())
        .saturating_add(session.title.len())
        .saturating_add(metadata_bytes);
    Ok(conservative_text_bytes(text, 384))
}

pub(super) fn event_estimated_bytes(event: &WarpNativeEvent) -> usize {
    let text = event
        .identity
        .conversation_id
        .len()
        .saturating_add(event.identity.task_id.len())
        .saturating_add(match &event.identity.message {
            WarpNativeMessageIdentity::ProviderId(value) => value.len(),
            WarpNativeMessageIdentity::MessageOrdinal(_) => 4,
        })
        .saturating_add(event.native_order.task_key.len())
        .saturating_add(event.request_id.as_ref().map_or(0, String::len))
        .saturating_add(event.call_id.as_ref().map_or(0, String::len))
        .saturating_add(event.lexical_body.len())
        .saturating_add(event.kind.len());
    conservative_text_bytes(text, 512)
}

pub(super) fn conservative_text_bytes(text_bytes: usize, overhead: usize) -> usize {
    text_bytes.saturating_mul(6).saturating_add(overhead)
}

pub(super) fn checked_add(total: usize, value: usize, message: &'static str) -> Result<usize> {
    total
        .checked_add(value)
        .ok_or(CaptureError::SystemInvariant(message))
}

fn hash_session(hasher: &mut Sha256, session: &WarpNativeSession) -> Result<()> {
    hasher.update(b"session\0");
    hash_text(hasher, &session.conversation_id)?;
    hash_optional_text(hasher, session.parent_conversation_id.as_deref())?;
    hash_text(hasher, &session.root_conversation_id)?;
    hasher.update([u8::from(session.parent_present)]);
    hash_text(hasher, &session.title)?;
    hash_optional_i64(
        hasher,
        session.modified_at.map(|value| value.timestamp_millis()),
    );
    hash_bytes(hasher, &serde_json::to_vec(&session.metadata)?)?;
    Ok(())
}

fn hash_edge(hasher: &mut Sha256, edge: &WarpNativeHierarchyEdge) -> Result<()> {
    hasher.update(b"edge\0");
    hash_text(hasher, &edge.child_conversation_id)?;
    hash_text(hasher, &edge.parent_conversation_id)?;
    hasher.update([u8::from(edge.parent_present)]);
    Ok(())
}

fn hash_event(hasher: &mut Sha256, event: &WarpNativeEvent) -> Result<()> {
    hasher.update(b"event\0");
    hash_text(hasher, &event.identity.conversation_id)?;
    hash_text(hasher, &event.identity.task_id)?;
    match &event.identity.message {
        WarpNativeMessageIdentity::ProviderId(value) => {
            hasher.update([1]);
            hash_text(hasher, value)?;
        }
        WarpNativeMessageIdentity::MessageOrdinal(value) => {
            hasher.update([2]);
            hasher.update(value.to_le_bytes());
        }
    }
    hasher.update(event.native_order.provider_event_index.to_le_bytes());
    hash_optional_u64(hasher, event.native_order.legacy_provider_event_index);
    hasher.update(event.native_order.task_rowid.to_le_bytes());
    hash_text(hasher, &event.native_order.task_key)?;
    hasher.update(event.native_order.message_ordinal.to_le_bytes());
    hash_text(hasher, event.event_type.as_str())?;
    hash_optional_text(hasher, event.role.map(EventRole::as_str))?;
    hash_text(hasher, event.kind)?;
    hash_optional_text(hasher, event.request_id.as_deref())?;
    hash_optional_outcome(hasher, event.result_outcome);
    hash_optional_text(hasher, event.call_id.as_deref())?;
    hash_optional_i64(
        hasher,
        event.occurred_at.map(|value| value.timestamp_millis()),
    );
    hash_text(hasher, &event.lexical_body)?;
    hash_text(hasher, event.source_record_digest.as_str())?;
    Ok(())
}

fn hash_rejection(hasher: &mut Sha256, rejection: &WarpNativeRejection) -> Result<()> {
    hasher.update(b"rejection\0");
    hasher.update([match rejection.kind {
        WarpNativeRejectionKind::ConversationRecord => 1,
        WarpNativeRejectionKind::TaskRecord => 2,
        WarpNativeRejectionKind::MalformedProtobuf => 3,
        WarpNativeRejectionKind::MissingConversation => 4,
        WarpNativeRejectionKind::OversizedTask => 5,
        WarpNativeRejectionKind::OversizedNormalizedUnit => 6,
        WarpNativeRejectionKind::DuplicateMessageIdentity => 7,
    }]);
    hash_text(hasher, &rejection.native_key)?;
    hash_text(hasher, &rejection.reason)?;
    Ok(())
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn hash_optional_i32(hasher: &mut Sha256, value: Option<i32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn hash_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn hash_optional_text(hasher: &mut Sha256, value: Option<&str>) -> Result<()> {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hash_text(hasher, value)?;
    }
    Ok(())
}

fn hash_text(hasher: &mut Sha256, value: &str) -> Result<()> {
    hash_bytes(hasher, value.as_bytes())
}

pub(super) fn hash_bytes(hasher: &mut Sha256, value: &[u8]) -> Result<()> {
    hash_usize(
        hasher,
        value.len(),
        "Warp NativePath digest field length exceeds u64",
    )?;
    hasher.update(value);
    Ok(())
}

fn hash_usize(hasher: &mut Sha256, value: usize, message: &'static str) -> Result<()> {
    let value = u64::try_from(value).map_err(|_| CaptureError::SystemInvariant(message))?;
    hasher.update(value.to_le_bytes());
    Ok(())
}

fn hash_optional_outcome(hasher: &mut Sha256, outcome: Option<OutputOutcome>) {
    hasher.update([match outcome {
        None => 0,
        Some(OutputOutcome::Success) => 1,
        Some(OutputOutcome::Failure) => 2,
        Some(OutputOutcome::Timeout) => 3,
        Some(OutputOutcome::Unknown) => 4,
    }]);
}

pub(super) fn bound_rejection_text(native_key: &mut String, reason: &mut String) {
    *native_key = truncate_chars(native_key, WARP_NATIVE_REJECTION_KEY_MAX_CHARS);
    *reason = truncate_chars(reason, WARP_NATIVE_REJECTION_REASON_MAX_CHARS);
}

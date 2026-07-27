use super::*;

pub(super) fn core_unit_digest(unit: &WarpNativeCoreUnit) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(WARP_CORE_UNIT_DIGEST_DOMAIN);
    for session in &unit.sessions {
        hash_session(&mut hasher, session)?;
    }
    for edge in &unit.hierarchy_edges {
        hash_edge(&mut hasher, edge)?;
    }
    for event in &unit.events {
        hash_event(&mut hasher, event)?;
    }
    for rejection in &unit.rejections {
        hash_rejection(&mut hasher, rejection)?;
    }
    Ok(hasher.finalize().into())
}

pub(super) fn page_identity(page: &WarpNativePage) -> Result<WarpNativePageIdentity> {
    let mut hasher = Sha256::new();
    hasher.update(WARP_PAGE_IDENTITY_DOMAIN);
    hash_frontier(&mut hasher, &page.expected_frontier)?;
    hash_frontier(&mut hasher, &page.next_safe_frontier)?;
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

pub(super) fn pro_page_identity(
    page: &WarpNativeProOutputPage,
) -> Result<WarpNativeProOutputPageIdentity> {
    let mut hasher = Sha256::new();
    hasher.update(WARP_PRO_PAGE_IDENTITY_DOMAIN);
    hash_frontier(&mut hasher, &page.expected_frontier)?;
    hash_frontier(&mut hasher, &page.next_safe_frontier)?;
    hash_usize(
        &mut hasher,
        page.logical_units,
        "Warp NativePath Pro page logical-unit count exceeds u64",
    )?;
    for output in &page.outputs {
        hash_pro_output(&mut hasher, output)?;
    }
    for rejection in &page.rejections {
        hash_output_rejection(&mut hasher, rejection)?;
    }
    Ok(WarpNativeProOutputPageIdentity(hasher.finalize().into()))
}

fn hash_pro_output(hasher: &mut Sha256, output: &ProOutputObservation) -> Result<()> {
    hasher.update(b"output\0");
    hasher.update([match output.kind {
        crate::OutputObservationKind::Command => 1,
        crate::OutputObservationKind::Tool => 2,
    }]);
    hash_text(hasher, &output.coordinate.unit_key)?;
    hasher.update(output.coordinate.native_sequence.to_le_bytes());
    hash_optional_text(hasher, output.coordinate.native_record_id.as_deref())?;
    hash_optional_u64(hasher, output.coordinate.source_record_ordinal);
    hash_optional_u32(hasher, output.coordinate.source_record_subrecord_index);
    hash_optional_u64(hasher, output.coordinate.byte_start);
    hash_optional_u64(hasher, output.coordinate.byte_end_exclusive);
    hash_optional_i64(hasher, output.occurred_at_unix_ms);

    let associations = &output.associations;
    hash_text(hasher, &associations.direct_session_id)?;
    hash_text(hasher, &associations.root_session_id)?;
    hash_optional_text(hasher, associations.parent_session_id.as_deref())?;
    hash_optional_text(hasher, associations.provider_session_id.as_deref())?;
    hash_optional_text(hasher, associations.agent_id.as_deref())?;
    hasher.update([u8::from(associations.repository.is_some())]);
    if let Some(repository) = &associations.repository {
        hash_text(hasher, &repository.repository_id)?;
        hash_optional_text(hasher, repository.checkout_id.as_deref())?;
        hash_optional_text(hasher, repository.worktree_id.as_deref())?;
        hash_optional_text(hasher, repository.object_format.as_deref())?;
    }

    hash_optional_text(hasher, output.call_id.as_deref())?;
    hasher.update([u8::from(output.command.is_some())]);
    if let Some(command) = &output.command {
        hash_text(hasher, &command.tool_name)?;
        hash_text(hasher, &command.command)?;
        hash_optional_text(hasher, command.working_directory.as_deref())?;
    }
    hash_optional_outcome(hasher, Some(output.outcome.outcome));
    hash_optional_i32(hasher, output.outcome.exit_code);
    hash_optional_u64(hasher, output.outcome.duration_ms);
    hasher.update(output.locator.version.to_le_bytes());
    hash_text(hasher, &output.locator.kind)?;
    hash_bytes(hasher, &output.locator.payload)?;
    hash_bytes(hasher, &output.content)
}

fn hash_output_rejection(hasher: &mut Sha256, rejection: &WarpNativeOutputRejection) -> Result<()> {
    hasher.update(b"output-rejection\0");
    hasher.update([match rejection.kind {
        WarpNativeOutputRejectionKind::Malformed => 1,
        WarpNativeOutputRejectionKind::Oversized => 2,
    }]);
    hash_text(hasher, &rejection.native_key)?;
    hash_text(hasher, &rejection.reason)
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
        .saturating_add(event.body.len())
        .saturating_add(event.content_hash.len())
        .saturating_add(event.preview.len())
        .saturating_add(event.kind.len());
    conservative_text_bytes(text, 512)
}

pub(super) fn output_estimated_bytes(output: &ProOutputObservation) -> usize {
    let associations = &output.associations;
    let repository_bytes = associations.repository.as_ref().map_or(0, |repository| {
        repository
            .repository_id
            .len()
            .saturating_add(repository.checkout_id.as_ref().map_or(0, String::len))
            .saturating_add(repository.worktree_id.as_ref().map_or(0, String::len))
            .saturating_add(repository.object_format.as_ref().map_or(0, String::len))
    });
    let command_bytes = output.command.as_ref().map_or(0, |command| {
        command
            .tool_name
            .len()
            .saturating_add(command.command.len())
            .saturating_add(command.working_directory.as_ref().map_or(0, String::len))
    });
    let text = output
        .coordinate
        .unit_key
        .len()
        .saturating_add(
            output
                .coordinate
                .native_record_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(associations.direct_session_id.len())
        .saturating_add(associations.root_session_id.len())
        .saturating_add(
            associations
                .parent_session_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(
            associations
                .provider_session_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(associations.agent_id.as_ref().map_or(0, String::len))
        .saturating_add(repository_bytes)
        .saturating_add(output.call_id.as_ref().map_or(0, String::len))
        .saturating_add(command_bytes)
        .saturating_add(output.locator.kind.len())
        .saturating_add(output.locator.payload.len());
    conservative_text_bytes(text, 1_024).saturating_add(output.content.len())
}

pub(super) fn conservative_text_bytes(text_bytes: usize, overhead: usize) -> usize {
    text_bytes.saturating_mul(6).saturating_add(overhead)
}

pub(super) fn checked_add(total: usize, value: usize, message: &'static str) -> Result<usize> {
    total
        .checked_add(value)
        .ok_or(CaptureError::SystemInvariant(message))
}

fn hash_frontier(hasher: &mut Sha256, frontier: &WarpNativeFrontier) -> Result<()> {
    hasher.update([match frontier.phase {
        WarpNativeFrontierPhase::Start => 0,
        WarpNativeFrontierPhase::Conversations => 1,
        WarpNativeFrontierPhase::Tasks => 2,
    }]);
    hasher.update(frontier.completed_conversation_rows.to_le_bytes());
    hasher.update(frontier.completed_hierarchy_edges.to_le_bytes());
    hash_optional_i64(hasher, frontier.last_conversation_rowid);
    hasher.update(frontier.completed_task_rows.to_le_bytes());
    hash_optional_i64(hasher, frontier.last_task_rowid);
    hasher.update(frontier.next_message_ordinal.to_le_bytes());
    hasher.update(frontier.retained_events.to_le_bytes());
    hasher.update(frontier.legacy_indexed_events.to_le_bytes());
    hasher.update(frontier.source_digest);
    hasher.update(frontier.core_digest);
    Ok(())
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
    hash_text(hasher, &event.body)?;
    hash_text(hasher, &event.content_hash)?;
    if let Some(content_ref) = &event.complete_content_ref {
        hasher.update([1]);
        hash_text(hasher, event.source_record_digest.as_str())?;
        hash_text(hasher, content_ref.sha256())?;
        hasher.update(content_ref.byte_len().to_le_bytes());
    } else {
        hasher.update([0]);
    }
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

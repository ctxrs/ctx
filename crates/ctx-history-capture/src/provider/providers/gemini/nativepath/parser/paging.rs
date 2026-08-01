use super::*;

#[cfg(test)]
pub(super) fn core_page_conservative_bytes(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    retained_event_bytes: usize,
    rejection_bytes: usize,
) -> Option<usize> {
    PAGE_ENVELOPE_FIXED_BYTES
        .checked_add(frontier_wire_bytes(expected)?)?
        .checked_add(frontier_wire_bytes(next)?)?
        .checked_add(retained_event_bytes)?
        .checked_add(rejection_bytes)
}

#[cfg(test)]
pub(super) fn frontier_wire_bytes(frontier: &GeminiPageFrontier) -> Option<usize> {
    let mut total = 1024_usize;
    if let Some(session) = &frontier.session {
        for value in [
            Some(session.native_session_id.as_str()),
            session.parent_native_session_id.as_deref(),
            session.cwd.as_deref(),
            session.native_kind.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            total = total.checked_add(estimated_json_string_wire_bytes(value)?)?;
        }
    }
    Some(total)
}

pub(super) fn estimated_json_string_wire_bytes(value: &str) -> Option<usize> {
    value.chars().try_fold(2_usize, |total, character| {
        let escaped_bytes = match character {
            '"' | '\\' | '\u{0008}' | '\u{0009}' | '\n' | '\u{000c}' | '\r' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => character.len_utf8(),
        };
        total.checked_add(escaped_bytes)
    })
}

#[cfg(test)]
pub(super) fn rejection_wire_bytes(rejection: &GeminiRejection) -> Option<usize> {
    REJECTION_ENVELOPE_FIXED_BYTES.checked_add(estimated_json_string_wire_bytes(&rejection.reason)?)
}

#[cfg(test)]
pub(super) fn derive_page_identity(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    events: &[GeminiRetainedEvent],
    rejections: &[GeminiRejection],
    terminal: bool,
) -> GeminiPageIdentity {
    let mut hasher = Sha256::new();
    hasher.update(CORE_PAGE_IDENTITY_DOMAIN);
    hash_page_frontier(&mut hasher, expected);
    hash_page_frontier(&mut hasher, next);
    hasher.update([u8::from(terminal)]);
    hasher.update(
        u64::try_from(events.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for event in events {
        hasher.update(b"event\0");
        match &event.identity {
            GeminiEventIdentity::NativeRecordId(value) => hash_page_text(&mut hasher, value),
        }
        hasher.update(event.native_order.raw_ordinal.to_le_bytes());
        hasher.update(event.native_order.sub_ordinal.to_le_bytes());
        hash_page_text(&mut hasher, event.event_type.as_str());
        hash_page_text(&mut hasher, event.role.as_str());
        hash_page_optional_i64(
            &mut hasher,
            event.occurred_at.map(|value| value.timestamp_millis()),
        );
        hasher.update(event.body_sha256);
        hash_page_text(&mut hasher, &event.preview);
        hash_page_text(&mut hasher, &event.searchable_text);
        hasher.update(
            u64::try_from(event.safe_file_touches.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        for touch in &event.safe_file_touches {
            hash_page_text(&mut hasher, touch);
        }
    }
    hasher.update(
        u64::try_from(rejections.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for rejection in rejections {
        hasher.update(b"rejection\0");
        hasher.update(rejection.raw_ordinal.to_le_bytes());
        hasher.update(rejection.byte_start.to_le_bytes());
        hasher.update(rejection.byte_end_exclusive.to_le_bytes());
        match &rejection.kind {
            GeminiRejectionKind::InvalidRecord => hasher.update([0]),
        }
        hash_page_text(&mut hasher, &rejection.reason);
    }
    GeminiPageIdentity(hasher.finalize().into())
}

#[cfg(test)]
pub(super) fn hash_page_frontier(hasher: &mut Sha256, frontier: &GeminiPageFrontier) {
    hasher.update(frontier.parser_revision.to_le_bytes());
    hasher.update(frontier.policy_revision.to_le_bytes());
    hasher.update(frontier.complete_prefix_end.to_le_bytes());
    hasher.update(frontier.complete_prefix_sha256);
    hash_page_optional_u64(hasher, frontier.source_device);
    hash_page_optional_u64(hasher, frontier.source_inode);
    hasher.update(frontier.next_raw_ordinal.to_le_bytes());
    hasher.update(frontier.retained_event_count.to_le_bytes());
    hasher.update(frontier.rejected_records.to_le_bytes());
    hasher.update([u8::from(frontier.append_boundary_safe)]);
    if let Some(session) = &frontier.session {
        hasher.update([1]);
        hash_page_text(hasher, &session.native_session_id);
        hash_page_optional_text(hasher, session.parent_native_session_id.as_deref());
        hash_page_text(hasher, session.agent_type.as_str());
        hash_page_optional_i64(
            hasher,
            session.started_at.map(|value| value.timestamp_millis()),
        );
        hash_page_optional_text(hasher, session.cwd.as_deref());
        hash_page_optional_text(hasher, session.native_kind.as_deref());
    } else {
        hasher.update([0]);
    }
}

pub(super) fn hash_page_text(hasher: &mut Sha256, value: &str) {
    hash_page_bytes(hasher, value.as_bytes());
}

pub(super) fn hash_page_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

pub(super) fn hash_page_optional_text(hasher: &mut Sha256, value: Option<&str>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hash_page_text(hasher, value);
    }
}

pub(super) fn hash_page_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

pub(super) fn hash_page_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

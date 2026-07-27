use super::*;

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

pub(super) fn output_page_conservative_bytes(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    transient_output_bytes: usize,
) -> Option<usize> {
    PAGE_ENVELOPE_FIXED_BYTES
        .checked_add(frontier_wire_bytes(expected)?)?
        .checked_add(frontier_wire_bytes(next)?)?
        .checked_add(transient_output_bytes)
}

pub(super) fn build_output_pages(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    outputs: Vec<(ProOutputObservation, usize)>,
    terminal: bool,
) -> GeminiScanResult<Vec<GeminiNativeOutputPage>> {
    let mut pages = Vec::new();
    let mut page_outputs = Vec::new();
    let mut page_output_bytes = 0_usize;

    for (output, output_bytes) in outputs {
        let next_units = page_outputs
            .len()
            .checked_add(1)
            .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                "Gemini output page unit accounting overflowed",
            )))?;
        let next_output_bytes =
            page_output_bytes
                .checked_add(output_bytes)
                .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini output page byte accounting overflowed",
                )))?;
        let next_page_bytes = output_page_conservative_bytes(expected, next, next_output_bytes)
            .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                "Gemini output page accounting overflowed",
            )))?;
        if next_units > MAX_GEMINI_NATIVE_PAGE_RECORDS
            || next_page_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES
        {
            if page_outputs.is_empty() {
                return Err(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini output passed admission but cannot fit an empty output page",
                )));
            }
            pages.push(finish_output_page(
                expected,
                next,
                pages.len(),
                page_outputs,
                page_output_bytes,
                terminal,
            )?);
            page_outputs = Vec::new();
            page_output_bytes = 0;
        }

        page_output_bytes =
            page_output_bytes
                .checked_add(output_bytes)
                .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini output page byte accounting overflowed",
                )))?;
        let single_page_bytes = output_page_conservative_bytes(expected, next, page_output_bytes)
            .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
            "Gemini output page accounting overflowed",
        )))?;
        if single_page_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES {
            return Err(GeminiScanError::Capture(CaptureError::SystemInvariant(
                "Gemini output passed admission but exceeds an empty output page",
            )));
        }
        page_outputs.push(output);
    }

    if !page_outputs.is_empty() || pages.is_empty() {
        pages.push(finish_output_page(
            expected,
            next,
            pages.len(),
            page_outputs,
            page_output_bytes,
            terminal,
        )?);
    }
    Ok(pages)
}

pub(super) fn finish_output_page(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    _page_ordinal: usize,
    outputs: Vec<ProOutputObservation>,
    transient_output_bytes: usize,
    _terminal: bool,
) -> GeminiScanResult<GeminiNativeOutputPage> {
    #[cfg(test)]
    let page_ordinal = u32::try_from(_page_ordinal).map_err(|_| {
        GeminiScanError::Capture(CaptureError::SystemInvariant(
            "Gemini output page ordinal overflowed",
        ))
    })?;
    let conservative_serialized_bytes =
        output_page_conservative_bytes(expected, next, transient_output_bytes).ok_or(
            GeminiScanError::Capture(CaptureError::SystemInvariant(
                "Gemini output page accounting overflowed",
            )),
        )?;
    if outputs.len() > MAX_GEMINI_NATIVE_PAGE_RECORDS
        || conservative_serialized_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES
    {
        return Err(GeminiScanError::Capture(CaptureError::SystemInvariant(
            "Gemini output page exceeded its admitted bounds",
        )));
    }
    #[cfg(test)]
    let identity = derive_output_page_identity(expected, next, page_ordinal, &outputs, _terminal);
    Ok(GeminiNativeOutputPage {
        #[cfg(test)]
        identity,
        #[cfg(test)]
        page_ordinal,
        logical_units: outputs.len(),
        outputs,
        conservative_serialized_bytes,
    })
}

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

pub(super) fn estimated_base64_wire_bytes(decoded_bytes: usize) -> Option<usize> {
    decoded_bytes
        .checked_add(2)?
        .checked_div(3)?
        .checked_mul(4)?
        .checked_add(2)
}

pub(super) fn rejection_wire_bytes(rejection: &GeminiRejection) -> Option<usize> {
    REJECTION_ENVELOPE_FIXED_BYTES.checked_add(estimated_json_string_wire_bytes(&rejection.reason)?)
}

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
pub(super) fn derive_output_page_identity(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    page_ordinal: u32,
    outputs: &[ProOutputObservation],
    terminal: bool,
) -> GeminiOutputPageIdentity {
    let mut hasher = Sha256::new();
    hasher.update(OUTPUT_PAGE_IDENTITY_DOMAIN);
    hash_page_frontier(&mut hasher, expected);
    hash_page_frontier(&mut hasher, next);
    hasher.update(page_ordinal.to_le_bytes());
    hasher.update([u8::from(terminal)]);
    hasher.update(
        u64::try_from(outputs.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for output in outputs {
        hasher.update(b"output\0");
        hasher.update([match output.kind {
            OutputObservationKind::Command => 0,
            OutputObservationKind::Tool => 1,
        }]);
        hash_page_text(&mut hasher, &output.coordinate.unit_key);
        hasher.update(output.coordinate.native_sequence.to_le_bytes());
        hash_page_optional_text(&mut hasher, output.coordinate.native_record_id.as_deref());
        hash_page_optional_u64(&mut hasher, output.coordinate.source_record_ordinal);
        hash_page_optional_u32(&mut hasher, output.coordinate.source_record_subrecord_index);
        hash_page_optional_u64(&mut hasher, output.coordinate.byte_start);
        hash_page_optional_u64(&mut hasher, output.coordinate.byte_end_exclusive);
        hash_page_optional_i64(&mut hasher, output.occurred_at_unix_ms);
        hash_page_text(&mut hasher, &output.associations.direct_session_id);
        hash_page_text(&mut hasher, &output.associations.root_session_id);
        hash_page_optional_text(
            &mut hasher,
            output.associations.parent_session_id.as_deref(),
        );
        hash_page_optional_text(
            &mut hasher,
            output.associations.provider_session_id.as_deref(),
        );
        hash_page_optional_text(&mut hasher, output.associations.agent_id.as_deref());
        if let Some(repository) = &output.associations.repository {
            hasher.update([1]);
            hash_page_text(&mut hasher, &repository.repository_id);
            hash_page_optional_text(&mut hasher, repository.checkout_id.as_deref());
            hash_page_optional_text(&mut hasher, repository.worktree_id.as_deref());
            hash_page_optional_text(&mut hasher, repository.object_format.as_deref());
        } else {
            hasher.update([0]);
        }
        hash_page_optional_text(&mut hasher, output.call_id.as_deref());
        if let Some(command) = &output.command {
            hasher.update([1]);
            hash_page_text(&mut hasher, &command.tool_name);
            hash_page_text(&mut hasher, &command.command);
            hash_page_optional_text(&mut hasher, command.working_directory.as_deref());
        } else {
            hasher.update([0]);
        }
        hasher.update([match output.outcome.outcome {
            OutputOutcome::Success => 0,
            OutputOutcome::Failure => 1,
            OutputOutcome::Timeout => 2,
            OutputOutcome::Unknown => 3,
        }]);
        hash_page_optional_i32(&mut hasher, output.outcome.exit_code);
        hash_page_optional_u64(&mut hasher, output.outcome.duration_ms);
        hasher.update(output.locator.version.to_le_bytes());
        hash_page_text(&mut hasher, &output.locator.kind);
        hash_page_bytes(&mut hasher, &output.locator.payload);
        hash_page_bytes(&mut hasher, &output.content);
    }
    GeminiOutputPageIdentity(hasher.finalize().into())
}

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

#[cfg(test)]
pub(super) fn hash_page_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
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

#[cfg(test)]
pub(super) fn hash_page_optional_i32(hasher: &mut Sha256, value: Option<i32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

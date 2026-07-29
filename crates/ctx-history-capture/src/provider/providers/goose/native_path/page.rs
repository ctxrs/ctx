use super::*;

pub(super) fn finalize_core_page(
    page: &mut GooseNativePage,
    generation_digest: &[u8],
    limits: GooseNativePageLimits,
) -> Result<()> {
    if !page.excluded_outputs.is_empty() {
        return Err(CaptureError::SystemInvariant(
            "Goose Core page contains an output marker",
        ));
    }
    page.terminal = page.next_frontier.phase == GooseNativeScanPhase::Complete;
    let logical_units = usize::try_from(
        page.next_frontier
            .native_rows_seen
            .saturating_sub(page.expected_frontier.native_rows_seen),
    )
    .unwrap_or(usize::MAX);
    let conservative_serialized_bytes = goose_core_page_encoded_bytes(page);
    validate_goose_page_bounds(logical_units, conservative_serialized_bytes, limits, "Core")?;
    page.accounting = GooseNativePageAccounting {
        logical_units,
        conservative_serialized_bytes,
    };
    let mut hasher = Sha256::new();
    hasher.update(GOOSE_CORE_PAGE_IDENTITY_DOMAIN);
    goose_hash_bytes(&mut hasher, generation_digest);
    goose_hash_position(&mut hasher, page.expected_frontier);
    goose_hash_position(&mut hasher, page.next_frontier);
    hasher.update([u8::from(page.terminal)]);
    for session in &page.sessions {
        hasher.update(b"session");
        goose_hash_str(&mut hasher, &session.native_identity);
        goose_hash_str(&mut hasher, &goose_session_content_digest(session));
    }
    for event in &page.events {
        hasher.update(b"event");
        goose_hash_str(&mut hasher, &event.native_identity);
        goose_hash_str(&mut hasher, &goose_event_content_digest(event));
    }
    for rejection in &page.rejections {
        hasher.update(b"rejection");
        goose_hash_i64(&mut hasher, rejection.sqlite_rowid);
        goose_hash_str(&mut hasher, &rejection.native_identity);
        goose_hash_str(&mut hasher, rejection.kind.as_str());
        goose_hash_str(&mut hasher, &rejection.reason);
    }
    page.identity = GooseNativePageIdentity(hasher.finalize().into());
    Ok(())
}

pub(super) fn validate_goose_page_bounds(
    logical_units: usize,
    bytes: usize,
    limits: GooseNativePageLimits,
    lane: &str,
) -> Result<()> {
    if logical_units == 0 || logical_units > limits.rows {
        return Err(CaptureError::InvalidPayload(format!(
            "Goose NativePath {lane} page has {logical_units} logical units"
        )));
    }
    let byte_limit = usize::try_from(limits.retained_bytes).unwrap_or(usize::MAX);
    if bytes > byte_limit {
        return Err(CaptureError::InvalidPayload(format!(
            "Goose NativePath {lane} page has {bytes} conservatively encoded bytes"
        )));
    }
    Ok(())
}

#[derive(Default)]
struct GooseEncodedByteCounter {
    bytes: usize,
}

impl GooseEncodedByteCounter {
    fn fixed(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
    }

    fn bytes(&mut self, value: &[u8]) {
        self.fixed(8);
        self.fixed(value.len());
    }

    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn optional_string(&mut self, value: Option<&str>) {
        self.fixed(1);
        if let Some(value) = value {
            self.string(value);
        }
    }
}

pub(super) fn goose_core_page_encoded_bytes(page: &GooseNativePage) -> usize {
    let mut counter = GooseEncodedByteCounter {
        bytes: GOOSE_PAGE_FIXED_BYTES,
    };
    for session in &page.sessions {
        counter.fixed(8);
        counter.string(&session.native_identity);
        counter.string(&format!("{:?}", session.row));
    }
    for event in &page.events {
        counter.fixed(8 * 3 + 4);
        counter.string(&event.native_identity);
        counter.string(&event.session_identity);
        counter.string(&event.role);
        counter.string(&event.content.to_string());
        counter.string(&event.searchable_text);
        counter.optional_string(event.timestamp.as_deref());
        counter.optional_string(event.tokens_json.as_deref());
        counter.optional_string(event.metadata_json.as_deref());
        counter.fixed(1);
        if let Some(logical_row_digest) = event.logical_row_digest {
            counter.bytes(&logical_row_digest);
        }
        for touch in &event.file_touches {
            counter.string(&touch.path);
            counter.optional_string(touch.old_path.as_deref());
            counter.string(touch.evidence);
        }
    }
    for rejection in &page.rejections {
        counter.fixed(8 * 2 + 4);
        counter.string(&rejection.native_identity);
        counter.optional_string(rejection.session_identity.as_deref());
        counter.string(&rejection.reason);
    }
    counter.bytes
}

pub(super) fn goose_hash_position(hasher: &mut Sha256, position: GooseNativeScanPosition) {
    hasher.update([match position.phase {
        GooseNativeScanPhase::Sessions => 1,
        GooseNativeScanPhase::Messages => 2,
        GooseNativeScanPhase::Complete => 3,
    }]);
    match position.keyset {
        super::super::position::GooseNativeRowKeyset::Unstarted => hasher.update([0]),
        super::super::position::GooseNativeRowKeyset::After(rowid) => {
            hasher.update([1]);
            goose_hash_i64(hasher, rowid);
        }
    }
    hasher.update(position.native_rows_seen.to_le_bytes());
}

pub(super) fn goose_hash_session_row(hasher: &mut Sha256, row: &GooseSessionRow) {
    goose_hash_str(hasher, &row.id);
    goose_hash_optional_str(hasher, row.name.as_deref());
    goose_hash_optional_str(hasher, row.description.as_deref());
    hasher.update([u8::from(row.user_set_name)]);
    goose_hash_optional_str(hasher, row.session_type.as_deref());
    goose_hash_optional_str(hasher, row.working_dir.as_deref());
    goose_hash_optional_str(hasher, row.created_at.as_deref());
    goose_hash_optional_str(hasher, row.updated_at.as_deref());
    goose_hash_optional_str(hasher, row.extension_data.as_deref());
    for value in [
        row.total_tokens,
        row.input_tokens,
        row.output_tokens,
        row.accumulated_total_tokens,
        row.accumulated_input_tokens,
        row.accumulated_output_tokens,
    ] {
        goose_hash_optional_i64(hasher, value);
    }
    match row.accumulated_cost {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_bits().to_le_bytes());
        }
        None => hasher.update([0]),
    }
    goose_hash_optional_str(hasher, row.provider_name.as_deref());
    goose_hash_optional_str(hasher, row.model_config_json.as_deref());
    goose_hash_optional_str(hasher, row.goose_mode.as_deref());
    goose_hash_optional_str(hasher, row.archived_at.as_deref());
    goose_hash_optional_str(hasher, row.project_id.as_deref());
}

pub(super) fn goose_hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            goose_hash_i64(hasher, value);
        }
        None => hasher.update([0]),
    }
}

pub(super) fn goose_hash_optional_str(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            goose_hash_str(hasher, value);
        }
        None => hasher.update([0]),
    }
}

pub(super) fn goose_hash_i64(hasher: &mut Sha256, value: i64) {
    hasher.update(value.to_le_bytes());
}

pub(super) fn goose_hash_str(hasher: &mut Sha256, value: &str) {
    goose_hash_bytes(hasher, value.as_bytes());
}

pub(super) fn goose_hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

pub(super) fn goose_hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

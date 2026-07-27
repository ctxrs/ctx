use super::*;

#[derive(Debug)]
pub(crate) struct BoundEncoding {
    bytes: usize,
}

impl BoundEncoding {
    pub(crate) fn mutation() -> Self {
        Self {
            bytes: BOUND_MUTATION_HEADER_BYTES,
        }
    }

    pub(crate) fn null(&mut self) {
        self.bytes = self.bytes.saturating_add(BOUND_TAG_BYTES);
    }

    pub(crate) fn integer(&mut self) {
        self.bytes = self.bytes.saturating_add(BOUND_TAG_BYTES + 8);
    }

    pub(crate) fn text(&mut self, value: &str) {
        self.bytes = self
            .bytes
            .saturating_add(BOUND_TAG_BYTES)
            .saturating_add(BOUND_LENGTH_BYTES)
            .saturating_add(value.len());
    }

    pub(crate) fn optional_text(&mut self, value: Option<&str>) {
        match value {
            Some(value) => self.text(value),
            None => self.null(),
        }
    }

    pub(crate) fn optional_integer(&mut self, present: bool) {
        if present {
            self.integer();
        } else {
            self.null();
        }
    }

    pub(crate) fn finish(self) -> usize {
        self.bytes
    }
}

pub(super) fn capture_source_bind_bytes(source: &CaptureSource) -> Result<usize> {
    let metadata = serde_json::to_string(&source.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&source.id.to_string());
    values.text(source.descriptor.kind.as_str());
    values.text(source.descriptor.provider.as_str());
    values.text(&source.descriptor.machine_id);
    values.optional_integer(source.descriptor.process_id.is_some());
    values.optional_text(source.descriptor.cwd.as_deref());
    values.optional_text(source.descriptor.raw_source_path.as_deref());
    values.optional_text(source.descriptor.source_format.as_deref());
    values.optional_text(source.descriptor.source_root.as_deref());
    values.optional_text(source.descriptor.source_identity.as_deref());
    values.optional_text(source.descriptor.external_session_id.as_deref());
    values.integer();
    values.optional_integer(source.ended_at.is_some());
    values.text(source.sync.fidelity.as_str());
    values.text(source.sync.visibility.as_str());
    values.text(source.sync.sync_state.as_str());
    values.integer();
    values.text(&metadata);
    Ok(values.finish())
}

pub(super) fn session_bind_bytes(session: &Session) -> Result<usize> {
    let metadata = serde_json::to_string(&session.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&session.id.to_string());
    add_optional_uuid(&mut values, session.history_record_id);
    add_optional_uuid(&mut values, session.parent_session_id);
    add_optional_uuid(&mut values, session.root_session_id);
    add_optional_uuid(&mut values, session.capture_source_id);
    values.text(session.provider.as_str());
    values.optional_text(session.external_session_id.as_deref());
    values.optional_text(session.external_agent_id.as_deref());
    values.text(session.agent_type.as_str());
    values.optional_text(session.role_hint.as_deref());
    values.integer();
    values.text(session.status.as_str());
    values.text(session.sync.fidelity.as_str());
    add_optional_uuid(&mut values, session.transcript_blob_id);
    values.integer();
    values.optional_integer(session.ended_at.is_some());
    values.integer();
    values.integer();
    values.text(session.sync.visibility.as_str());
    values.text(session.sync.sync_state.as_str());
    values.integer();
    values.optional_integer(session.sync.deleted_at.is_some());
    values.text(&metadata);
    Ok(values.finish())
}

pub(crate) fn event_bind_bytes(event: &Event) -> Result<usize> {
    let event = durable_event(event)?;
    let payload = serde_json::to_string(&event.payload)?;
    let metadata = serde_json::to_string(&event.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&event.id.to_string());
    values.integer();
    add_optional_uuid(&mut values, event.history_record_id);
    add_optional_uuid(&mut values, event.session_id);
    add_optional_uuid(&mut values, event.run_id);
    values.text(event.event_type.as_str());
    values.optional_text(event.role.map(|role| role.as_str()));
    values.integer();
    add_optional_uuid(&mut values, event.capture_source_id);
    values.text(&payload);
    add_optional_uuid(&mut values, event.payload_blob_id);
    values.optional_text(event.dedupe_key.as_deref());
    values.text(event.sync.visibility.as_str());
    values.text(event.sync.fidelity.as_str());
    values.text(event.sync.sync_state.as_str());
    values.integer();
    values.optional_integer(event.sync.deleted_at.is_some());
    values.text(&metadata);
    Ok(values.finish())
}

pub(super) fn run_bind_bytes(run: &Run) -> Result<usize> {
    if !provider_output_run_is_retained_failure(run) {
        return Ok(0);
    }
    let metadata = serde_json::to_string(&run.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&run.id.to_string());
    add_optional_uuid(&mut values, run.history_record_id);
    add_optional_uuid(&mut values, run.session_id);
    values.text(run.run_type.as_str());
    values.text(run.status.as_str());
    values.integer();
    values.optional_integer(run.ended_at.is_some());
    values.optional_integer(run.exit_code.is_some());
    values.optional_text(run.cwd.as_deref());
    values.optional_text(run.command_preview.as_deref());
    add_optional_uuid(&mut values, run.input_blob_id);
    add_optional_uuid(&mut values, run.output_blob_id);
    values.integer();
    values.integer();
    add_optional_uuid(&mut values, run.source_id);
    values.text(run.sync.visibility.as_str());
    values.text(run.sync.fidelity.as_str());
    values.text(run.sync.sync_state.as_str());
    values.integer();
    values.optional_integer(run.sync.deleted_at.is_some());
    values.text(&metadata);
    Ok(values.finish())
}

pub(super) fn file_touch_bind_bytes(file: &FileTouched) -> Result<usize> {
    let metadata = serde_json::to_string(&file.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&file.id.to_string());
    add_optional_uuid(&mut values, file.history_record_id);
    add_optional_uuid(&mut values, file.run_id);
    add_optional_uuid(&mut values, file.event_id);
    add_optional_uuid(&mut values, file.vcs_workspace_id);
    values.text(file.path.as_str());
    values.optional_text(file.change_kind.map(|kind| kind.as_str()));
    values.optional_text(file.old_path.as_deref());
    values.optional_integer(file.line_count_delta.is_some());
    values.text(file.confidence.as_str());
    values.integer();
    values.integer();
    add_optional_uuid(&mut values, file.source_id);
    values.text(file.sync.visibility.as_str());
    values.text(file.sync.fidelity.as_str());
    values.text(file.sync.sync_state.as_str());
    values.integer();
    values.optional_integer(file.sync.deleted_at.is_some());
    values.text(&metadata);
    Ok(values.finish())
}

pub(super) fn session_edge_bind_bytes(edge: &SessionEdge) -> Result<usize> {
    let metadata = serde_json::to_string(&edge.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&edge.id.to_string());
    values.text(&edge.from_session_id.to_string());
    values.text(&edge.to_session_id.to_string());
    values.text(edge.edge_type.as_str());
    values.text(edge.confidence.as_str());
    add_optional_uuid(&mut values, edge.source_id);
    values.integer();
    values.integer();
    values.text(edge.sync.visibility.as_str());
    values.text(edge.sync.fidelity.as_str());
    values.text(edge.sync.sync_state.as_str());
    values.integer();
    values.optional_integer(edge.sync.deleted_at.is_some());
    values.text(&metadata);
    Ok(values.finish())
}

fn add_optional_uuid(values: &mut BoundEncoding, value: Option<Uuid>) {
    match value {
        Some(value) => values.text(&value.to_string()),
        None => values.null(),
    }
}

pub(super) fn encoded_cursor_cas_bytes(expected: Option<&SyncCursor>, next: &SyncCursor) -> usize {
    let mut values = BoundEncoding::mutation();
    match expected {
        Some(expected) => {
            values.text(&next.cursor);
            values.optional_integer(next.last_synced_at.is_some());
            values.integer();
            values.text(&expected.id.to_string());
            values.optional_text(expected.team_id.as_deref());
            values.text(&expected.device_id);
            values.text(&expected.stream);
            values.text(&expected.cursor);
            values.optional_integer(expected.last_synced_at.is_some());
            values.integer();
            values.integer();
        }
        None => {
            values.text(&next.id.to_string());
            values.optional_text(next.team_id.as_deref());
            values.text(&next.device_id);
            values.text(&next.stream);
            values.text(&next.cursor);
            values.optional_integer(next.last_synced_at.is_some());
            values.integer();
            values.integer();
        }
    }
    values.finish()
}

pub(super) fn encode_cursor_envelope(
    envelope: &NativePathCommittedCursorEnvelope,
) -> Result<String> {
    serde_json::to_string(envelope).map_err(StoreError::from)
}

pub(super) fn decode_cursor_envelope(encoded: &str) -> Result<NativePathCommittedCursorEnvelope> {
    let envelope: NativePathCommittedCursorEnvelope =
        serde_json::from_str(encoded).map_err(|_| StoreError::InvalidNativePathCursorSet)?;
    if envelope.version != NATIVE_PATH_CURSOR_ENVELOPE_VERSION || envelope.publication_id.is_empty()
    {
        return Err(StoreError::InvalidNativePathCursorSet);
    }
    Ok(envelope)
}

pub(super) fn validate_limit(limit: &'static str, actual: usize, maximum: usize) -> Result<()> {
    if actual > maximum {
        return Err(StoreError::NativePathGroupLimitExceeded {
            limit,
            actual,
            maximum,
        });
    }
    Ok(())
}

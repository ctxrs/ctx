use super::*;

pub(super) fn frontier_for_position(
    position: &OpenCodeNativeScanPosition,
) -> OpenCodeNativeFrontier {
    OpenCodeNativeFrontier {
        phase: position.phase,
        scan_ordinal: match position.phase {
            OpenCodeNativeScanPhase::Sessions => position.native_sessions_seen,
            OpenCodeNativeScanPhase::Events | OpenCodeNativeScanPhase::Complete => {
                position.native_events_seen
            }
        },
    }
}

pub(super) fn page_logical_units(page: &OpenCodeNativePage) -> usize {
    page.sessions
        .len()
        .saturating_add(page.events.len())
        .saturating_add(page.rejections.len())
}

pub(super) fn finalize_core_page(page: &mut OpenCodeNativePage, terminal: bool) -> Result<()> {
    if !page.excluded_outputs.is_empty() {
        return Err(CaptureError::SystemInvariant(
            "OpenCode Core page contains an output marker",
        ));
    }
    page.terminal = terminal;
    let logical_units = page_logical_units(page);
    let conservative_serialized_bytes = core_page_encoded_bytes(page)?;
    validate_page_bounds(
        logical_units,
        conservative_serialized_bytes,
        "Core",
        page.terminal,
    )?;
    page.accounting = OpenCodeNativePageAccounting {
        logical_units,
        conservative_serialized_bytes,
    };
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-nativepath-core-page-v1\0");
    hash_frontier(&mut hasher, page.expected_frontier);
    hash_frontier(&mut hasher, page.next_frontier);
    hasher.update([u8::from(page.terminal)]);
    for session in &page.sessions {
        hasher.update(b"session");
        hash_str(&mut hasher, &session.native_identity);
        hash_str(&mut hasher, &session.content_digest);
    }
    for event in &page.events {
        hasher.update(b"event");
        hash_str(&mut hasher, &event.native_identity);
        hash_str(&mut hasher, &event.content_digest);
        hash_str(&mut hasher, &event.locator.kind);
        hash_bytes(&mut hasher, &event.locator.payload);
    }
    for rejection in &page.rejections {
        hasher.update(b"rejection");
        hash_str(&mut hasher, &rejection.native_identity);
        if let Some(session_identity) = rejection.session_identity.as_deref() {
            hash_str(&mut hasher, session_identity);
        }
        if let Some(order) = rejection.native_order.as_ref() {
            hash_order(&mut hasher, order);
        }
        hash_str(&mut hasher, rejection.kind.label());
        hash_str(&mut hasher, &rejection.reason);
    }
    page.identity = OpenCodeNativePageIdentity(hasher.finalize().into());
    Ok(())
}

pub(super) fn finalize_pro_page(page: &mut OpenCodeNativeProOutputPage) -> Result<()> {
    let logical_units = page
        .observations
        .len()
        .saturating_add(page.rejections.len());
    let conservative_serialized_bytes = pro_page_encoded_bytes(page)?;
    validate_page_bounds(
        logical_units,
        conservative_serialized_bytes,
        "Pro",
        page.terminal,
    )?;
    page.accounting = OpenCodeNativePageAccounting {
        logical_units,
        conservative_serialized_bytes,
    };
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-nativepath-pro-page-v1\0");
    hash_pro_frontier(&mut hasher, page.expected_frontier);
    hash_pro_frontier(&mut hasher, page.next_frontier);
    hasher.update([u8::from(page.terminal)]);
    for output in &page.observations {
        hasher.update([match output.kind {
            OutputObservationKind::Command => 1,
            OutputObservationKind::Tool => 2,
        }]);
        hash_str(&mut hasher, &output.coordinate.unit_key);
        hasher.update(output.coordinate.native_sequence.to_le_bytes());
        hash_optional_str(&mut hasher, output.coordinate.native_record_id.as_deref());
        hash_optional_u64(&mut hasher, output.coordinate.source_record_ordinal);
        hash_optional_u32(&mut hasher, output.coordinate.source_record_subrecord_index);
        hash_optional_u64(&mut hasher, output.coordinate.byte_start);
        hash_optional_u64(&mut hasher, output.coordinate.byte_end_exclusive);
        hash_optional_i64(&mut hasher, output.occurred_at_unix_ms);
        hash_str(&mut hasher, &output.associations.direct_session_id);
        hash_str(&mut hasher, &output.associations.root_session_id);
        hash_optional_str(
            &mut hasher,
            output.associations.parent_session_id.as_deref(),
        );
        hash_optional_str(
            &mut hasher,
            output.associations.provider_session_id.as_deref(),
        );
        hash_optional_str(&mut hasher, output.associations.agent_id.as_deref());
        match output.associations.repository.as_ref() {
            Some(repository) => {
                hasher.update([1]);
                hash_str(&mut hasher, &repository.repository_id);
                hash_optional_str(&mut hasher, repository.checkout_id.as_deref());
                hash_optional_str(&mut hasher, repository.worktree_id.as_deref());
                hash_optional_str(&mut hasher, repository.object_format.as_deref());
            }
            None => hasher.update([0]),
        }
        hash_optional_str(&mut hasher, output.call_id.as_deref());
        match output.command.as_ref() {
            Some(command) => {
                hasher.update([1]);
                hash_str(&mut hasher, &command.tool_name);
                hash_str(&mut hasher, &command.command);
                hash_optional_str(&mut hasher, command.working_directory.as_deref());
            }
            None => hasher.update([0]),
        }
        hasher.update([match output.outcome.outcome {
            OutputOutcome::Success => 1,
            OutputOutcome::Failure => 2,
            OutputOutcome::Timeout => 3,
            OutputOutcome::Unknown => 4,
        }]);
        match output.outcome.exit_code {
            Some(value) => {
                hasher.update([1]);
                hasher.update(value.to_le_bytes());
            }
            None => hasher.update([0]),
        }
        hash_optional_u64(&mut hasher, output.outcome.duration_ms);
        hasher.update(output.locator.version.to_le_bytes());
        hash_str(&mut hasher, &output.locator.kind);
        hash_bytes(&mut hasher, &output.locator.payload);
        hasher.update((output.content.len() as u64).to_le_bytes());
        hasher.update(&output.content);
    }
    for rejection in &page.rejections {
        hash_str(&mut hasher, &rejection.native_identity);
        hasher.update(rejection.source_event_ordinal.to_le_bytes());
        match rejection.subrecord_index {
            Some(value) => {
                hasher.update([1]);
                hasher.update(value.to_le_bytes());
            }
            None => hasher.update([0]),
        }
        hasher.update([match rejection.kind {
            OpenCodeNativeProRejectionKind::MalformedOutput => 1,
            OpenCodeNativeProRejectionKind::OversizedOutput => 2,
            OpenCodeNativeProRejectionKind::TooManySubrecords => 3,
        }]);
        hash_str(&mut hasher, &rejection.reason);
        hash_str(&mut hasher, &rejection.locator.kind);
        hash_bytes(&mut hasher, &rejection.locator.payload);
    }
    page.identity = OpenCodeNativeProPageIdentity(hasher.finalize().into());
    Ok(())
}

pub(super) fn validate_page_bounds(
    units: usize,
    bytes: usize,
    lane: &str,
    allow_terminal_empty: bool,
) -> Result<()> {
    if (units == 0 && !allow_terminal_empty) || units > model::OPENCODE_NATIVE_PAGE_MAX_UNITS {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {lane} page has {units} logical units"
        )));
    }
    if bytes > OPENCODE_NATIVE_PAGE_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {lane} page has {bytes} conservatively encoded bytes"
        )));
    }
    Ok(())
}

pub(super) fn project_pro_output(
    record: &ProRecordMetadata,
    source_format: &str,
) -> Option<ProOutputObservation> {
    let draft = record.draft.as_ref()?;
    let kind = if draft.kind == 1 {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let command = (kind == OutputObservationKind::Command).then(|| OutputCommandContext {
        tool_name: draft
            .tool_name
            .clone()
            .unwrap_or_else(|| "shell".to_owned()),
        command: draft.command.clone().unwrap_or_else(|| "shell".to_owned()),
        working_directory: draft.working_directory.clone(),
    });
    Some(ProOutputObservation {
        kind,
        coordinate: OutputNativeCoordinate {
            unit_key: if draft.subrecord_index == 0 {
                format!(
                    "{source_format}:{}:{}:output",
                    record.session_identity, record.source_native_identity
                )
            } else {
                format!(
                    "{source_format}:{}:{}:output:subrecord:{}",
                    record.session_identity, record.source_native_identity, draft.subrecord_index
                )
            },
            native_sequence: record.native_record_ordinal,
            native_record_id: Some(record.source_native_identity.clone()),
            source_record_ordinal: Some(record.source_record_ordinal),
            source_record_subrecord_index: Some(draft.subrecord_index),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(record.time_created),
        associations: OutputAssociations {
            direct_session_id: record.session_identity.clone(),
            root_session_id: record.root_session_identity.clone(),
            parent_session_id: record.parent_session_identity.clone(),
            provider_session_id: Some(record.session_identity.clone()),
            agent_id: record.agent_identity.clone(),
            repository: None,
        },
        call_id: draft.call_id.clone(),
        command,
        outcome: OutputOutcomeMetadata {
            outcome: match draft.outcome {
                1 => OutputOutcome::Success,
                2 => OutputOutcome::Failure,
                3 => OutputOutcome::Timeout,
                _ => OutputOutcome::Unknown,
            },
            exit_code: draft.exit_code,
            duration_ms: draft.duration_ms,
        },
        locator: OutputSourceLocator {
            version: record.locator.version,
            kind: record.locator.kind.clone(),
            payload: record.locator.payload.clone(),
        },
        content: draft.content.as_bytes().to_vec(),
    })
}

pub(super) fn project_pro_rejection(
    record: &ProRecordMetadata,
) -> Result<OpenCodeNativeProRejection> {
    let reason = record
        .rejection
        .clone()
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode Pro record has neither output nor rejection",
        ))?;
    let kind = if reason.contains("subrecords; maximum") {
        OpenCodeNativeProRejectionKind::TooManySubrecords
    } else if reason.contains("encoded bytes") {
        OpenCodeNativeProRejectionKind::OversizedOutput
    } else {
        OpenCodeNativeProRejectionKind::MalformedOutput
    };
    Ok(OpenCodeNativeProRejection {
        source_event_ordinal: record.source_event_ordinal,
        native_identity: record.native_identity.clone(),
        subrecord_index: (record.subrecord_index != u32::MAX).then_some(record.subrecord_index),
        kind,
        reason,
        locator: record.locator.clone(),
    })
}

#[derive(Default)]
struct EncodedByteCounter {
    bytes: usize,
}

impl EncodedByteCounter {
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

    fn locator(&mut self, locator: &OpenCodeNativeLocator) {
        self.fixed(4);
        self.string(&locator.kind);
        self.bytes(&locator.payload);
    }
}

pub(super) fn core_page_encoded_bytes(page: &OpenCodeNativePage) -> Result<usize> {
    let mut counter = EncodedByteCounter::default();
    counter.fixed(32 + 1 + 4 * 8);
    counter.string(&page.source_authority.selected_path().to_string_lossy());
    let OpenCodeNativeSourceAuthority::ExactDispatchedDatabase {
        inventory_observation_token,
        ..
    } = &page.source_authority;
    counter.optional_string(inventory_observation_token.as_deref());
    for session in &page.sessions {
        counter.string(&session.native_identity);
        counter.optional_string(session.parent_identity.as_deref());
        counter.string(&session.root_identity);
        counter.optional_string(session.title.as_deref());
        counter.optional_string(session.directory.as_deref());
        counter.optional_string(session.model_identity.as_deref());
        counter.optional_string(session.agent_identity.as_deref());
        counter.fixed(16);
        counter.string(&session.content_digest);
    }
    for event in &page.events {
        counter.string(&event.native_identity);
        counter.string(&event.message_identity);
        counter.string(&event.session_identity);
        count_order(&mut counter, &event.native_order);
        counter.fixed(1 + 16);
        counter.string(&event.role);
        counter.string(&event.searchable_text);
        let body = serde_json::to_vec(&event.body).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "OpenCode Core page body cannot be encoded: {error}"
            ))
        })?;
        counter.bytes(&body);
        counter.string(&event.content_digest);
        counter.fixed(8);
        for touch in &event.file_touches {
            counter.string(&touch.path);
        }
        counter.locator(&event.locator);
    }
    for rejection in &page.rejections {
        counter.string(&rejection.native_identity);
        counter.optional_string(rejection.session_identity.as_deref());
        counter.fixed(1);
        if let Some(order) = rejection.native_order.as_ref() {
            count_order(&mut counter, order);
        }
        counter.fixed(1);
        counter.string(&rejection.reason);
    }
    Ok(counter.bytes)
}

pub(super) fn pro_page_encoded_bytes(page: &OpenCodeNativeProOutputPage) -> Result<usize> {
    let mut counter = EncodedByteCounter::default();
    counter.fixed(32 + 1 + 6 * 8);
    counter.string(&page.source_authority.selected_path().to_string_lossy());
    let OpenCodeNativeSourceAuthority::ExactDispatchedDatabase {
        inventory_observation_token,
        ..
    } = &page.source_authority;
    counter.optional_string(inventory_observation_token.as_deref());
    for output in &page.observations {
        counter.fixed(1);
        counter.string(&output.coordinate.unit_key);
        counter.fixed(8);
        counter.optional_string(output.coordinate.native_record_id.as_deref());
        counter.fixed(1 + 8);
        counter.fixed(1 + 4);
        counter.fixed(2 * (1 + 8));
        counter.fixed(1 + 8);
        counter.string(&output.associations.direct_session_id);
        counter.string(&output.associations.root_session_id);
        counter.optional_string(output.associations.parent_session_id.as_deref());
        counter.optional_string(output.associations.provider_session_id.as_deref());
        counter.optional_string(output.associations.agent_id.as_deref());
        counter.fixed(1);
        if let Some(repository) = output.associations.repository.as_ref() {
            counter.string(&repository.repository_id);
            counter.optional_string(repository.checkout_id.as_deref());
            counter.optional_string(repository.worktree_id.as_deref());
            counter.optional_string(repository.object_format.as_deref());
        }
        counter.optional_string(output.call_id.as_deref());
        counter.fixed(1);
        if let Some(command) = output.command.as_ref() {
            counter.string(&command.tool_name);
            counter.string(&command.command);
            counter.optional_string(command.working_directory.as_deref());
        }
        counter.fixed(1 + 1 + 4 + 1 + 8);
        counter.fixed(4);
        counter.string(&output.locator.kind);
        counter.bytes(&output.locator.payload);
        counter.bytes(&output.content);
    }
    for rejection in &page.rejections {
        counter.fixed(8);
        counter.string(&rejection.native_identity);
        counter.fixed(1 + 4 + 1);
        counter.string(&rejection.reason);
        counter.locator(&rejection.locator);
    }
    Ok(counter.bytes)
}

fn count_order(counter: &mut EncodedByteCounter, order: &OpenCodeNativeOrder) {
    counter.fixed(1);
    match order {
        OpenCodeNativeOrder::ExplicitSequence {
            session_id,
            message_id,
            ..
        }
        | OpenCodeNativeOrder::SynthesizedSequence {
            session_id,
            message_id,
            ..
        } => {
            counter.string(session_id);
            counter.fixed(8);
            counter.string(message_id);
        }
        OpenCodeNativeOrder::MessagePart {
            session_id,
            message_id,
            part_id,
            ..
        } => {
            counter.string(session_id);
            counter.fixed(16);
            counter.string(message_id);
            counter.string(part_id);
        }
    }
}

pub(super) fn hash_frontier(hasher: &mut Sha256, frontier: OpenCodeNativeFrontier) {
    hasher.update([match frontier.phase {
        OpenCodeNativeScanPhase::Sessions => 1,
        OpenCodeNativeScanPhase::Events => 2,
        OpenCodeNativeScanPhase::Complete => 3,
    }]);
    hasher.update(frontier.scan_ordinal.to_le_bytes());
}

pub(super) fn hash_pro_frontier(hasher: &mut Sha256, frontier: OpenCodeNativeProFrontier) {
    hasher.update(frontier.source_event_ordinal.to_le_bytes());
    hasher.update(frontier.subrecord_index.to_le_bytes());
    hasher.update([u8::from(frontier.terminal)]);
}

pub(super) fn normalize_retained_event(
    record: &RecordMetadata,
    retained: OpenCodeRetainedJson,
) -> Result<OpenCodeNativeEvent> {
    let OpenCodeRetainedJson {
        effective_type,
        role,
        mut body,
    } = retained;
    let kind = retained_event_kind(&effective_type, &role, &body);
    let searchable_text = retained_searchable_text(kind, &effective_type, &body);
    let time_created = body
        .pointer("/time/created")
        .and_then(Value::as_i64)
        .unwrap_or(record.time_created);
    let (file_touches, file_touch_count) = retained_file_touches(kind, &body);
    if file_touch_count > file_touches.len() {
        if let Some(object) = body.as_object_mut() {
            object.insert(
                "file_touch_retention".to_owned(),
                serde_json::json!({
                    "observed": file_touch_count,
                    "retained": file_touches.len(),
                    "truncated": true,
                }),
            );
        }
    }
    let content_digest = record
        .content_digest
        .clone()
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode retained projection is missing its snapshot-local digest",
        ))?;
    Ok(OpenCodeNativeEvent {
        native_identity: record.native_identity.clone(),
        message_identity: record.message_identity.clone(),
        session_identity: record.source_session_identity.clone(),
        native_order: record.native_order.clone(),
        kind,
        role,
        provider_event_index: record.stable_native_ordinal,
        legacy_provider_event_index: record.legacy_native_ordinal,
        source_record_ordinal: record.source_record_ordinal,
        time_created,
        time_updated: record.time_updated,
        searchable_text,
        body,
        content_digest,
        file_touches,
        locator: record.locator.clone(),
    })
}

pub(super) fn retained_event_kind(
    effective_type: &str,
    role: &str,
    body: &Value,
) -> OpenCodeNativeEventKind {
    if body.get("result_outcome").is_some() {
        if effective_type == "shell" || body.get("command").is_some() {
            return OpenCodeNativeEventKind::CommandOutput;
        }
        return OpenCodeNativeEventKind::ToolOutput;
    }
    if matches!(
        effective_type,
        "tool" | "tool_call" | "tool-call" | "tool_use" | "tooluse"
    ) || json_contains_tool_call(body)
    {
        OpenCodeNativeEventKind::ToolCall
    } else if matches!(effective_type, "reasoning" | "summary") {
        OpenCodeNativeEventKind::Summary
    } else if matches!(role, "user" | "assistant")
        || matches!(effective_type, "user" | "assistant" | "text")
    {
        OpenCodeNativeEventKind::Message
    } else {
        OpenCodeNativeEventKind::Notice
    }
}

pub(super) fn json_contains_tool_call(body: &Value) -> bool {
    body.get("tool_calls").is_some()
        || body.get("toolCall").is_some()
        || body.get("tool_call").is_some()
        || body
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("tool" | "tool_use" | "toolCall" | "tool_call")
                    )
                })
            })
}

pub(super) fn retained_searchable_text(
    kind: OpenCodeNativeEventKind,
    effective_type: &str,
    body: &Value,
) -> String {
    if let Some(text) = body.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = body.get("summary").and_then(Value::as_str) {
        return text.to_owned();
    }
    if kind == OpenCodeNativeEventKind::ToolCall {
        let tool = body
            .get("tool")
            .or_else(|| body.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let command = body
            .pointer("/state/input/command")
            .or_else(|| body.pointer("/input/command"))
            .or_else(|| body.get("command"))
            .and_then(Value::as_str);
        return command.map_or_else(
            || format!("tool call: {tool}"),
            |command| format!("{tool}\n{command}"),
        );
    }
    if let Some(content) = body.get("content") {
        let text = collect_text(content);
        if !text.is_empty() {
            return text;
        }
    }
    format!("OpenCode {effective_type} event")
}

pub(super) fn collect_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .filter_map(|value| value.get("text").and_then(Value::as_str).map(str::to_owned))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub(super) fn retained_file_touches(
    kind: OpenCodeNativeEventKind,
    body: &Value,
) -> (Vec<OpenCodeNativeFileTouch>, usize) {
    if !matches!(
        kind,
        OpenCodeNativeEventKind::ToolCall | OpenCodeNativeEventKind::Notice
    ) {
        return (Vec::new(), 0);
    }
    let mut paths = BTreeSet::new();
    for pointer in [
        "/path",
        "/file_path",
        "/filePath",
        "/input/path",
        "/input/file_path",
        "/state/input/path",
        "/state/input/file_path",
    ] {
        if let Some(path) = body.pointer(pointer).and_then(Value::as_str) {
            if !path.trim().is_empty() {
                paths.insert(path.to_owned());
            }
        }
    }
    if let Some(files) = body.get("files").and_then(Value::as_array) {
        for file in files {
            let path = file
                .as_str()
                .or_else(|| file.get("path").and_then(Value::as_str));
            if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
                paths.insert(path.to_owned());
            }
        }
    }
    let observed = paths.len();
    let retained = paths
        .into_iter()
        .take(OPENCODE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT)
        .map(|path| OpenCodeNativeFileTouch { path })
        .collect();
    (retained, observed)
}

pub(super) fn rejection(
    record: &RecordMetadata,
    kind: OpenCodeNativeRejectionKind,
    reason: String,
) -> OpenCodeNativeRejection {
    OpenCodeNativeRejection {
        native_identity: record.native_identity.clone(),
        session_identity: Some(record.source_session_identity.clone()),
        native_order: Some(record.native_order.clone()),
        kind,
        reason,
    }
}

pub(super) fn hash_order(hasher: &mut Sha256, order: &OpenCodeNativeOrder) {
    match order {
        OpenCodeNativeOrder::ExplicitSequence {
            session_id,
            sequence,
            message_id,
        } => {
            hasher.update([1]);
            hash_str(hasher, session_id);
            hasher.update(sequence.to_le_bytes());
            hash_str(hasher, message_id);
        }
        OpenCodeNativeOrder::SynthesizedSequence {
            session_id,
            time_created,
            message_id,
        } => {
            hasher.update([2]);
            hash_str(hasher, session_id);
            hasher.update(time_created.to_le_bytes());
            hash_str(hasher, message_id);
        }
        OpenCodeNativeOrder::MessagePart {
            session_id,
            message_time_created,
            message_id,
            part_time_created,
            part_id,
        } => {
            hasher.update([3]);
            hash_str(hasher, session_id);
            hasher.update(message_time_created.to_le_bytes());
            hash_str(hasher, message_id);
            hasher.update(part_time_created.to_le_bytes());
            hash_str(hasher, part_id);
        }
    }
}

pub(super) fn hash_str(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

pub(super) fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

pub(super) fn hash_optional_str(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_str(hasher, value);
        }
        None => hasher.update([0]),
    }
}

pub(super) fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

pub(super) fn hash_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

pub(super) fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

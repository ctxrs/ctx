use super::*;
use crate::provider::normalization::provider_role;
use crate::repository_attribution::{
    apply_annotation, AttributionInput, RepositoryAttributor, UnscopedFileObservation,
};
use ctx_history_core::RepositoryFileObservationKind;

pub(super) fn source_backed_retained_event_kind(
    effective_type: &str,
    role: &str,
    body: &serde_json::Value,
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

fn json_contains_tool_call(body: &serde_json::Value) -> bool {
    body.get("tool_calls").is_some()
        || body.get("toolCall").is_some()
        || body.get("tool_call").is_some()
        || body
            .get("content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    matches!(
                        block.get("type").and_then(serde_json::Value::as_str),
                        Some("tool" | "tool_use" | "toolCall" | "tool_call")
                    )
                })
            })
}

pub(super) fn source_backed_retained_searchable_text(
    kind: OpenCodeNativeEventKind,
    effective_type: &str,
    body: &serde_json::Value,
) -> String {
    if let Some(text) = body.get("text").and_then(serde_json::Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = body.get("summary").and_then(serde_json::Value::as_str) {
        return text.to_owned();
    }
    if kind == OpenCodeNativeEventKind::ToolCall {
        let tool = body
            .get("tool")
            .or_else(|| body.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("tool");
        let command = body
            .pointer("/state/input/command")
            .or_else(|| body.pointer("/input/command"))
            .or_else(|| body.get("command"))
            .and_then(serde_json::Value::as_str);
        return command.map_or_else(
            || format!("tool call: {tool}"),
            |command| format!("{tool}\n{command}"),
        );
    }
    if let Some(content) = body.get("content") {
        let text = match content {
            serde_json::Value::String(value) => value.clone(),
            serde_json::Value::Array(values) => values
                .iter()
                .filter_map(|value| {
                    value
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !text.is_empty() {
            return text;
        }
    }
    format!("OpenCode {effective_type} event")
}

fn source_backed_retained_file_touches(
    kind: OpenCodeNativeEventKind,
    body: &serde_json::Value,
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
        if let Some(path) = body.pointer(pointer).and_then(serde_json::Value::as_str) {
            if !path.trim().is_empty() {
                paths.insert(path.to_owned());
            }
        }
    }
    if let Some(files) = body.get("files").and_then(serde_json::Value::as_array) {
        for file in files {
            let path = file
                .as_str()
                .or_else(|| file.get("path").and_then(serde_json::Value::as_str));
            if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
                paths.insert(path.to_owned());
            }
        }
    }
    let observed = paths.len();
    let retained = paths
        .into_iter()
        .take(SOURCE_BACKED_MAX_FILE_TOUCHES)
        .map(|path| OpenCodeNativeFileTouch { path })
        .collect();
    (retained, observed)
}

fn repository_tool_string(body: &serde_json::Value, pointers: &[&str]) -> Option<String> {
    pointers
        .iter()
        .find_map(|pointer| body.pointer(pointer).and_then(serde_json::Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn repository_file_observation_kind(
    effective_type: &str,
    body: &serde_json::Value,
) -> RepositoryFileObservationKind {
    let tool = body
        .get("tool")
        .or_else(|| body.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(effective_type)
        .trim()
        .to_ascii_lowercase();
    match tool.as_str() {
        "read" | "read_file" | "grep" | "glob" | "search" => RepositoryFileObservationKind::Read,
        "edit" | "edit_file" | "apply_patch" | "patch" => RepositoryFileObservationKind::Modified,
        "write" | "write_file" => RepositoryFileObservationKind::Unknown,
        _ => RepositoryFileObservationKind::Unknown,
    }
}

fn repository_attribution_input(
    session: &SourceSession,
    retained: &OpenCodeRetainedJson,
    kind: OpenCodeNativeEventKind,
    activity_at_unix_ms: i64,
    file_touches: &[OpenCodeNativeFileTouch],
) -> AttributionInput {
    let has_tool_context = matches!(
        kind,
        OpenCodeNativeEventKind::Notice
            | OpenCodeNativeEventKind::ToolCall
            | OpenCodeNativeEventKind::ToolOutput
            | OpenCodeNativeEventKind::CommandOutput
    );
    let command = has_tool_context.then(|| {
        repository_tool_string(
            &retained.body,
            &[
                "/command",
                "/cmd",
                "/input/command",
                "/state/input/command",
                "/state/metadata/command",
            ],
        )
    });
    let declared_tool_workdir = has_tool_context.then(|| {
        repository_tool_string(
            &retained.body,
            &[
                "/working_directory",
                "/workingDirectory",
                "/workdir",
                "/cwd",
                "/input/workdir",
                "/input/cwd",
                "/state/input/workdir",
                "/state/input/cwd",
                "/state/metadata/cwd",
            ],
        )
    });
    let observation_kind =
        repository_file_observation_kind(&retained.effective_type, &retained.body);
    AttributionInput {
        activity_at_unix_ms: Some(activity_at_unix_ms),
        session_cwd: session.directory.clone(),
        declared_tool_workdir: declared_tool_workdir.flatten(),
        command: command.flatten(),
        file_observations: file_touches
            .iter()
            .map(|touch| UnscopedFileObservation {
                path: touch.path.clone(),
                prior_path: None,
                kind: observation_kind,
            })
            .collect(),
        ..AttributionInput::default()
    }
}

pub(super) fn decode_source_event_row(
    row: &Row<'_>,
    schema: &OpenCodeNativeSchema,
    dialect: &OpenCodeSqliteDialect,
    hydrated_payload: Option<(SqliteSourceValue, SqliteSourceValue)>,
) -> OpenCodeSourceBackedResult<SourceEventRow> {
    match row.get::<_, i64>(13)? {
        0 => {}
        1 => {
            return Err(CaptureError::InvalidPayload(format!(
                "OpenCode NativePath {} contains an unsafe native identity/order key",
                schema.family.event_table()
            ))
            .into())
        }
        2 => {
            return Err(CaptureError::InvalidPayload(
                "OpenCode NativePath part.message_id is not a safe native relationship key"
                    .to_owned(),
            )
            .into())
        }
        3 => {
            return Err(CaptureError::InvalidPayload(format!(
                "OpenCode NativePath {} contains a non-integer native ordering value",
                schema.family.event_table()
            ))
            .into())
        }
        4 => {
            return Err(CaptureError::InvalidPayload(
                "OpenCode NativePath message parent identity/order rows are unsafe".to_owned(),
            )
            .into())
        }
        _ => {
            return Err(CaptureError::SystemInvariant(
                "OpenCode source-backed row returned an unknown native validation code",
            )
            .into())
        }
    }
    let native_identity: String = row.get(0)?;
    let message_identity: String = row.get(1)?;
    let session_identity: String = row.get(2)?;
    let order_tag: i64 = row.get(3)?;
    let order_a: i64 = row.get(4)?;
    let order_b: i64 = row.get(5)?;
    let time_created: i64 = row.get(6)?;
    let time_updated: i64 = row.get(7)?;
    let content_bytes = u64::try_from(row.get::<_, i64>(8)?).map_err(|_| {
        CaptureError::InvalidPayload("OpenCode-family content byte count is negative".to_owned())
    })?;
    let column_type: String = row.get(9)?;
    let (source_data, parent_source_data) = hydrated_payload.unwrap_or((
        SqliteSourceValue::from_ref(row.get_ref(12)?),
        SqliteSourceValue::from_ref(row.get_ref(14)?),
    ));
    let (mut projection, has_explicit_event_time) = project_sqlite_json(
        &source_data,
        &parent_source_data,
        &column_type,
        schema.family,
        dialect,
    );
    let relationship_code = row.get::<_, i64>(15)?;
    if !has_explicit_event_time {
        if let Err(error) = provider_required_timestamp_millis(
            time_created,
            dialect.session_message_time_created_field,
        ) {
            projection = OpenCodeJsonProjection::RejectedWithReason(
                OpenCodeNativeRejectionKind::InvalidTimestamp,
                error.to_string(),
            );
        }
    }
    // Relationship failures are the outermost fail-closed classification in
    // the provider contract. Preserve that precedence even when malformed
    // payload or timestamp evidence is present in the same unsafe row.
    projection = apply_relationship_rejection(projection, relationship_code)?;
    Ok(SourceEventRow {
        native_order: source_backed_decode_order(
            order_tag,
            &session_identity,
            &message_identity,
            &native_identity,
            order_a,
            order_b,
        )?,
        native_identity,
        message_identity,
        session_identity,
        time_created,
        time_updated,
        content_bytes,
        projection,
        source_data,
        parent_source_data,
    })
}

fn apply_relationship_rejection(
    projection: OpenCodeJsonProjection,
    relationship_code: i64,
) -> OpenCodeSourceBackedResult<OpenCodeJsonProjection> {
    Ok(match relationship_code {
        0 => projection,
        1 => OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::MissingSession),
        2 => OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::MissingMessage),
        3 => OpenCodeJsonProjection::Rejected(
            OpenCodeNativeRejectionKind::SessionRelationshipMismatch,
        ),
        _ => {
            return Err(CaptureError::SystemInvariant(
                "OpenCode source-backed row returned an unknown relationship code",
            )
            .into())
        }
    })
}

fn project_sqlite_json(
    source_data: &SqliteSourceValue,
    parent_source_data: &SqliteSourceValue,
    column_type: &str,
    family: OpenCodeNativeSchemaFamily,
    dialect: &OpenCodeSqliteDialect,
) -> (OpenCodeJsonProjection, bool) {
    let Some(source_bytes) = source_data.exact_text() else {
        return (
            OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::UnsupportedStorageClass),
            false,
        );
    };
    if source_bytes.len() > MAX_PROVIDER_SQLITE_VALUE_BYTES {
        return (
            OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::OversizedRetainedContent),
            false,
        );
    }
    let Ok(source_text) = std::str::from_utf8(source_bytes) else {
        return (
            super::super::json::malformed_json_projection(column_type),
            false,
        );
    };
    let parent_text = if family == OpenCodeNativeSchemaFamily::MessagePart {
        let Some(parent_bytes) = parent_source_data.exact_text() else {
            return (
                OpenCodeJsonProjection::Rejected(
                    OpenCodeNativeRejectionKind::UnsupportedStorageClass,
                ),
                false,
            );
        };
        if parent_bytes.len() > MAX_PROVIDER_SQLITE_VALUE_BYTES {
            return (
                OpenCodeJsonProjection::Rejected(
                    OpenCodeNativeRejectionKind::OversizedRetainedContent,
                ),
                false,
            );
        }
        let Ok(parent_text) = std::str::from_utf8(parent_bytes) else {
            return (
                super::super::json::malformed_json_projection(column_type),
                false,
            );
        };
        Some(parent_text)
    } else {
        None
    };
    let mut has_explicit_event_time = false;
    let projection = super::super::json::project_json(
        source_text,
        column_type,
        parent_text,
        family,
        dialect,
        &mut has_explicit_event_time,
    );
    (projection, has_explicit_event_time)
}

pub(super) fn retained_projection(
    projection: &OpenCodeJsonProjection,
) -> Option<OpenCodeRetainedJson> {
    match projection {
        OpenCodeJsonProjection::Retained(retained) => Some(retained.clone()),
        OpenCodeJsonProjection::Output(output) => output.diagnostic.clone(),
        OpenCodeJsonProjection::ExcludedOutput
        | OpenCodeJsonProjection::Rejected(_)
        | OpenCodeJsonProjection::RejectedWithReason(_, _) => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn core_record(
    source: &SourceKey,
    family: OpenCodeNativeSchemaFamily,
    _source_path: &Path,
    session: &SourceSession,
    event: SourceEventRow,
    retained: OpenCodeRetainedJson,
    next_sequence: &mut u64,
    repository_attributor: &mut RepositoryAttributor,
) -> OpenCodeSourceBackedResult<CoreRecord> {
    event
        .source_data
        .exact_text()
        .ok_or(OpenCodeSourceBackedError::MissingExactText)?;
    let normalized_time = retained
        .body
        .pointer("/time/created")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(event.time_created);
    let native_record_identity = source_backed_native_record_identity(
        family,
        &event.message_identity,
        &event.native_identity,
    );
    let native_item_key = if family == OpenCodeNativeSchemaFamily::MessagePart {
        NativeItemKey::composite(
            family.identity_semantics(),
            vec![
                TypedKey::utf8(event.message_identity.clone())?,
                TypedKey::utf8(event.native_identity.clone())?,
            ],
        )?
    } else {
        NativeItemKey::native_id(
            family.identity_semantics(),
            TypedKey::utf8(native_record_identity)?,
        )?
    };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: session.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let native_event_id = TypedKey::utf8(event.native_identity.clone())?;

    let kind =
        source_backed_retained_event_kind(&retained.effective_type, &retained.role, &retained.body);
    let searchable =
        source_backed_retained_searchable_text(kind, &retained.effective_type, &retained.body);
    let body = if searchable.is_empty() {
        "OpenCode event".to_owned()
    } else {
        searchable
    };
    let (file_touches, _) = source_backed_retained_file_touches(kind, &retained.body);
    // Keep provider-authentic role/type evidence in the retained projection and
    // expose only the canonical role vocabulary through Core metadata.
    let role = provider_role(Some(&retained.role));
    let event_sequence = *next_sequence;
    *next_sequence = checked_add(*next_sequence, 1)?;
    let native_file_touches = (!file_touches.is_empty()).then(|| {
        serde_json::json!(file_touches
            .iter()
            .map(|touch| &touch.path)
            .collect::<Vec<_>>())
    });
    let is_primary = session.parent_native_identity.is_none();
    let agent_type = if is_primary { "primary" } else { "subagent" };
    let mut record = CoreRecord::new_selected(
        event_id,
        session.session_id,
        session.root_session_id,
        source.clone(),
        event_sequence,
        event_kind_label(kind),
        agent_type,
        is_primary,
        PARSER_REVISION,
        body,
    )?;
    record.parent_session_id = session.parent_session_id;
    record.provider_session_id = Some(session.native_identity.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(normalized_time);
    record.role = Some(role.as_str().to_owned());
    record.branch = session.branch.clone();
    record.cwd = session.directory.clone();
    let attribution = repository_attributor.attribute(repository_attribution_input(
        session,
        &retained,
        kind,
        normalized_time,
        &file_touches,
    ));
    apply_annotation(&mut record, attribution);
    if let Some(native_file_touches) = native_file_touches {
        record.metadata.insert(
            "provider_native_file_touches".to_owned(),
            native_file_touches,
        );
    }
    record.validate_contract()?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationship_rejection_precedes_payload_and_timestamp_rejections() {
        let invalid_timestamp = OpenCodeJsonProjection::RejectedWithReason(
            OpenCodeNativeRejectionKind::InvalidTimestamp,
            "bad timestamp".to_owned(),
        );

        assert!(matches!(
            apply_relationship_rejection(invalid_timestamp.clone(), 1).unwrap(),
            OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::MissingSession)
        ));
        assert!(matches!(
            apply_relationship_rejection(invalid_timestamp.clone(), 2).unwrap(),
            OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::MissingMessage)
        ));
        assert!(matches!(
            apply_relationship_rejection(invalid_timestamp, 3).unwrap(),
            OpenCodeJsonProjection::Rejected(
                OpenCodeNativeRejectionKind::SessionRelationshipMismatch
            )
        ));
    }
}

use super::*;
use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, AgentScope,
    CoreActivity, LiteralFactKind, ProviderDeclaredFact, ProviderNativeSessionRelationship,
    CORE_ACTIVITY_REVISION,
};

pub(super) fn source_backed_retained_event_kind(
    effective_type: &str,
    role: &str,
    body: &serde_json::Value,
) -> OpenCodeNativeEventKind {
    if matches!(
        effective_type,
        "result"
            | "toolresult"
            | "toolresponse"
            | "commandresult"
            | "output"
            | "tooloutput"
            | "commandoutput"
    ) || effective_type.ends_with("result")
        || role == "tool" && !opencode_has_input(body)
    {
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

fn opencode_has_input(body: &serde_json::Value) -> bool {
    body.pointer("/state/input").is_some()
        || body.get("input").is_some()
        || body.get("arguments").is_some()
        || body.get("command").is_some()
        || body.get("toolCall").is_some()
        || body.get("tool_calls").is_some()
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
    effective_type.to_owned()
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
        OpenCodeJsonProjection::Rejected(_) | OpenCodeJsonProjection::RejectedWithReason(_, _) => {
            None
        }
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
        serde_json::to_string(&retained.body).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "OpenCode retained JSON could not be rendered exactly: {error}"
            ))
        })?
    } else {
        searchable
    };
    let event_sequence = *next_sequence;
    *next_sequence = checked_add(*next_sequence, 1)?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session.session_id,
        source.clone(),
        event_sequence,
        event_kind_label(kind),
        PARSER_REVISION,
        body,
    )?;
    if let Some(parent_session_id) = session.parent_session_id {
        record.parent_session_id = Some(parent_session_id);
        record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
        record.agent_scope = Some(AgentScope::Subagent);
    } else {
        record.agent_scope = Some(AgentScope::Primary);
    }
    record.provider_session_id = Some(session.native_identity.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(normalized_time);
    record.role = Some(retained.role.clone());
    record.content.structured_content = Some(retained.body.clone());
    let mut facts = Vec::new();
    if let Some(directory) = session.directory.clone() {
        facts.push(ProviderDeclaredFact {
            kind: LiteralFactKind::SessionCwd,
            value: directory,
        });
    }
    if let Some(branch) = session.branch.clone() {
        facts.push(ProviderDeclaredFact {
            kind: LiteralFactKind::Branch,
            value: branch,
        });
    }
    collect_opencode_facts(&retained.body, &mut facts);
    let (provider_call_id, invocation, result) =
        opencode_activity(kind, &retained.body, normalized_time)?;
    if invocation.is_some() || result.is_some() || !facts.is_empty() {
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result,
            facts,
        });
    }
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(record)
}

fn collect_opencode_facts(value: &serde_json::Value, facts: &mut Vec<ProviderDeclaredFact>) {
    const MAX_EXPLICIT_FACTS: usize = 64;
    let maximum = facts.len().saturating_add(MAX_EXPLICIT_FACTS);
    for (kind, pointers) in [
        (
            LiteralFactKind::File,
            &[
                "/path",
                "/file",
                "/file_path",
                "/filePath",
                "/state/input/path",
                "/state/input/file",
                "/state/input/file_path",
                "/state/input/filePath",
                "/input/path",
                "/arguments/path",
            ][..],
        ),
        (
            LiteralFactKind::ToolWorkdir,
            &[
                "/cwd",
                "/workdir",
                "/state/input/cwd",
                "/state/input/workdir",
                "/input/cwd",
                "/arguments/cwd",
            ][..],
        ),
        (
            LiteralFactKind::Command,
            &[
                "/command",
                "/cmd",
                "/state/input/command",
                "/state/input/cmd",
                "/input/command",
                "/arguments/command",
            ][..],
        ),
        (LiteralFactKind::Url, &["/url", "/uri"][..]),
    ] {
        for pointer in pointers {
            let Some(value) = value.pointer(pointer) else {
                continue;
            };
            collect_opencode_fact_values(kind, value, facts, maximum);
            if facts.len() >= maximum {
                return;
            }
        }
    }
}

fn collect_opencode_fact_values(
    kind: LiteralFactKind,
    value: &serde_json::Value,
    facts: &mut Vec<ProviderDeclaredFact>,
    maximum: usize,
) {
    if facts.len() >= maximum {
        return;
    }
    match value {
        serde_json::Value::String(value) if !value.is_empty() => {
            facts.push(ProviderDeclaredFact {
                kind,
                value: value.clone(),
            });
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_opencode_fact_values(kind, value, facts, maximum);
            }
        }
        _ => {}
    }
}

fn opencode_activity(
    kind: OpenCodeNativeEventKind,
    body: &serde_json::Value,
    occurred_at_unix_ms: i64,
) -> OpenCodeSourceBackedResult<(
    Option<TypedKey>,
    Option<ActivityInvocation>,
    Option<ActivityResult>,
)> {
    let call_id = unique_opencode_string(
        body,
        &[
            "/call_id",
            "/callId",
            "/callID",
            "/tool_call_id",
            "/toolCallId",
            "/state/call_id",
            "/state/callId",
            "/id",
        ],
    );
    let Some(call_id) = call_id else {
        return Ok((None, None, None));
    };
    let provider_call_id = Some(TypedKey::utf8(&call_id)?);
    if kind == OpenCodeNativeEventKind::ToolCall {
        let tool = unique_opencode_string(body, &["/tool", "/tool_name", "/toolName", "/name"]);
        let Some(tool) = tool else {
            return Ok((None, None, None));
        };
        let arguments =
            opencode_json_alias_capture(body, &["/state/input", "/input", "/arguments"]);
        return Ok((
            provider_call_id,
            Some(ActivityInvocation {
                protocol: None,
                server: None,
                tool,
                arguments,
                started_at_unix_ms: Some(occurred_at_unix_ms),
            }),
            None,
        ));
    }
    if kind != OpenCodeNativeEventKind::ToolOutput {
        return Ok((None, None, None));
    }
    Ok((
        provider_call_id,
        None,
        Some(ActivityResult {
            status: unique_opencode_string(
                body,
                &["/state/status", "/status", "/state/outcome", "/outcome"],
            ),
            completed_at_unix_ms: Some(occurred_at_unix_ms),
            duration_ns: None,
            text: ActivityTextCapture::NormalizedBody,
            structured_content: ActivityJsonCapture::Present {
                value: body.clone(),
            },
        }),
    ))
}

fn unique_opencode_string(body: &serde_json::Value, pointers: &[&str]) -> Option<String> {
    let mut selected = None::<&str>;
    for pointer in pointers {
        let Some(value) = body.pointer(pointer).and_then(serde_json::Value::as_str) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        if selected.is_some_and(|selected| selected != value) {
            return None;
        }
        selected = Some(value);
    }
    selected.map(str::to_owned)
}

fn opencode_json_alias_capture(body: &serde_json::Value, pointers: &[&str]) -> ActivityJsonCapture {
    let mut selected = None::<&serde_json::Value>;
    for pointer in pointers {
        let Some(value) = body.pointer(pointer) else {
            continue;
        };
        if selected.is_some_and(|selected| selected != value) {
            return ActivityJsonCapture::Unavailable;
        }
        selected = Some(value);
    }
    selected
        .cloned()
        .map_or(ActivityJsonCapture::Absent, |value| {
            ActivityJsonCapture::Present { value }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_string_treats_empty_aliases_as_absent() {
        let body = serde_json::json!({"callID": "", "call_id": "call-valid"});
        assert_eq!(
            unique_opencode_string(&body, &["/callID", "/call_id"]),
            Some("call-valid".to_owned())
        );
        assert_eq!(unique_opencode_string(&body, &["/callID"]), None);
    }

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

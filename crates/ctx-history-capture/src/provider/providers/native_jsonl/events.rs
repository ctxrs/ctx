pub(crate) fn antigravity_session_id_from_path(path: &Path) -> Option<String> {
    let components: Vec<String> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
        .collect();
    components
        .windows(2)
        .find_map(|window| {
            (window[0] == "brain" && !window[1].trim().is_empty()).then(|| window[1].clone())
        })
        .or_else(|| {
            components.windows(2).find_map(|window| {
                (window[1] == ".system_generated" && !window[0].trim().is_empty())
                    .then(|| window[0].clone())
            })
        })
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(str::to_owned)
        })
}

pub(crate) fn windsurf_session_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn native_jsonl_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
        .or_else(|| {
            value
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_utc)
        })
        .or_else(|| {
            value
                .pointer("/time/created")
                .and_then(Value::as_i64)
                .and_then(DateTime::<Utc>::from_timestamp_millis)
        })
}

pub(crate) fn native_jsonl_session_status(
    provider: CaptureProvider,
    header: &Value,
) -> SessionStatus {
    if provider == CaptureProvider::CopilotCli
        && header.get("type").and_then(Value::as_str) == Some("abort")
    {
        SessionStatus::Interrupted
    } else {
        SessionStatus::Imported
    }
}

pub(crate) fn native_jsonl_session_metadata(
    provider: CaptureProvider,
    source_format: &str,
    header: &Value,
    path: &Path,
) -> Value {
    json!({
        "source_format": source_format,
        "provider": provider.as_str(),
        "source_path": path.display().to_string(),
        "header": provider_capped_json(header, PROVIDER_MAX_PREVIEW_CHARS),
    })
}

pub(crate) fn native_jsonl_event(
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let event_type = native_jsonl_event_type(provider, value);
    let entry_type = native_jsonl_entry_type(provider, value);
    let role = native_jsonl_role(provider, value);
    let text = native_jsonl_event_text(provider, value, event_type, &entry_type);
    let body_value = if provider == CaptureProvider::Windsurf {
        windsurf_event_body(value)
    } else {
        value.clone()
    };
    let retained_text = provider_policy_event_text(event_type, &text, &body_value);
    let event_id = native_jsonl_event_id(provider, value, line_number);
    let tool_calls = if provider == CaptureProvider::Antigravity {
        value.get("tool_calls").map(|calls| {
            provider_capped_json_value(
                &provider_policy_body(EventType::ToolCall, calls),
                PROVIDER_MAX_PREVIEW_CHARS,
            )
        })
    } else {
        None
    };
    let body = provider_capped_json(
        &provider_policy_body(event_type, &body_value),
        PROVIDER_MAX_PREVIEW_CHARS,
    );

    Some(ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(event_id.clone()),
        event_type,
        role: Some(role),
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(format!(
            "provider-event:{}:{source_format}:{event_id}",
            provider.as_str()
        )),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": entry_type,
            "event_id": event_id,
            "native_step_index": value.get("step_index").and_then(Value::as_u64),
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "tool_calls": tool_calls,
            "body": body,
        }),
        metadata: json!({
            "source": source_format,
            "source_format": source_format,
            "line": line_number,
            "entry_type": entry_type,
            "status": value.get("status").and_then(Value::as_str),
            "model": native_jsonl_model(provider, value),
            "tokens": native_jsonl_tokens(provider, value),
        }),
    })
}

pub(crate) fn native_jsonl_event_id(
    provider: CaptureProvider,
    value: &Value,
    line_number: usize,
) -> String {
    if provider == CaptureProvider::Antigravity {
        if let Some(step_index) = value.get("step_index").and_then(Value::as_u64) {
            return format!("step-{step_index}");
        }
    }
    value
        .get("id")
        .or_else(|| value.get("uuid"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("line-{line_number}"))
}

pub(crate) fn native_jsonl_entry_type(provider: CaptureProvider, value: &Value) -> String {
    match provider {
        CaptureProvider::Antigravity => value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            if value.get("$set").is_some() {
                "$set"
            } else if value.get("$rewindTo").is_some() {
                "$rewindTo"
            } else {
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            }
        }
        _ => value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
    }
    .to_owned()
}

pub(crate) fn native_jsonl_event_type(provider: CaptureProvider, value: &Value) -> EventType {
    match provider {
        CaptureProvider::Antigravity => match value.get("type").and_then(Value::as_str) {
            Some("USER_INPUT" | "CONVERSATION_HISTORY") => EventType::Message,
            Some("PLANNER_RESPONSE") => {
                if value.get("tool_calls").is_some() {
                    EventType::ToolCall
                } else {
                    EventType::Message
                }
            }
            Some("CODE_ACTION") => EventType::ToolCall,
            Some("CHECKPOINT") => EventType::Summary,
            Some("SYSTEM_MESSAGE") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            if value.get("$set").is_some() || value.get("$rewindTo").is_some() {
                EventType::Notice
            } else if value.get("toolCalls").is_some() {
                if gemini_tool_calls_have_result(value) {
                    EventType::ToolOutput
                } else {
                    EventType::ToolCall
                }
            } else {
                match value.get("type").and_then(Value::as_str) {
                    Some("user" | "gemini" | "tabnine") => EventType::Message,
                    _ => EventType::Notice,
                }
            }
        }
        CaptureProvider::FactoryAiDroid => match value.get("type").and_then(Value::as_str) {
            Some("message") if droid_content_has(value, "tool_use") => EventType::ToolCall,
            Some("message") if droid_content_has(value, "tool_result") => EventType::ToolOutput,
            Some("message") => EventType::Message,
            Some("compaction_state") => EventType::Summary,
            Some("todo_state" | "session_start") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::CopilotCli => match value.get("type").and_then(Value::as_str) {
            Some("user.message" | "assistant.message") => EventType::Message,
            Some("tool.execution_start") => EventType::ToolCall,
            Some("tool.execution_complete") => EventType::ToolOutput,
            Some("session.truncation") => EventType::Summary,
            Some("abort") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::Cursor => {
            if native_jsonl_content_has(value, "tool_result") {
                EventType::ToolOutput
            } else if native_jsonl_content_has(value, "tool_use") {
                EventType::ToolCall
            } else {
                match value
                    .get("event")
                    .or_else(|| value.get("type"))
                    .or_else(|| value.get("role"))
                    .and_then(Value::as_str)
                {
                    Some("turn_ended" | "summary") => EventType::Summary,
                    Some("user" | "assistant") => EventType::Message,
                    _ => EventType::Notice,
                }
            }
        }
        CaptureProvider::Windsurf => match value.get("type").and_then(Value::as_str) {
            Some("user_input" | "planner_response") => EventType::Message,
            Some("code_action") => EventType::ToolCall,
            Some("summary" | "checkpoint") => EventType::Summary,
            _ => EventType::Notice,
        },
        CaptureProvider::Qoder => match value.get("type").and_then(Value::as_str) {
            Some("assistant") if native_jsonl_content_has(value, "tool_use") => EventType::ToolCall,
            Some("user") if native_jsonl_content_has(value, "tool_result") => EventType::ToolOutput,
            Some("user" | "assistant") => EventType::Message,
            Some("progress") => EventType::Notice,
            Some("session_meta") => EventType::Notice,
            _ if value.get("toolUseResult").is_some() => EventType::ToolOutput,
            _ => EventType::Notice,
        },
        CaptureProvider::QwenCode => match value.get("type").and_then(Value::as_str) {
            Some("user" | "assistant") if native_jsonl_content_has(value, "tool_use") => {
                EventType::ToolCall
            }
            Some("tool_result") => EventType::ToolOutput,
            Some("user" | "assistant") => EventType::Message,
            Some("system") => EventType::Notice,
            _ if value.get("toolCallResult").is_some() => EventType::ToolOutput,
            _ => EventType::Notice,
        },
        _ => EventType::Notice,
    }
}

pub(crate) fn native_jsonl_role(provider: CaptureProvider, value: &Value) -> EventRole {
    match provider {
        CaptureProvider::Antigravity => match value.get("source").and_then(Value::as_str) {
            Some("user") => EventRole::User,
            Some("planner" | "agent" | "assistant") => EventRole::Assistant,
            Some("tool" | "executor") => EventRole::Tool,
            Some("system") => EventRole::System,
            _ => match value.get("type").and_then(Value::as_str) {
                Some("USER_INPUT") => EventRole::User,
                Some("SYSTEM_MESSAGE" | "CHECKPOINT") => EventRole::System,
                _ => EventRole::Assistant,
            },
        },
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            match value.get("type").and_then(Value::as_str) {
                Some("user") => EventRole::User,
                Some("gemini" | "tabnine") => EventRole::Assistant,
                _ => EventRole::System,
            }
        }
        CaptureProvider::FactoryAiDroid => provider_role(
            value
                .get("role")
                .or_else(|| value.pointer("/message/role"))
                .and_then(Value::as_str),
        ),
        CaptureProvider::CopilotCli => match value.get("type").and_then(Value::as_str) {
            Some("user.message") => EventRole::User,
            Some("assistant.message") => EventRole::Assistant,
            Some("tool.execution_start" | "tool.execution_complete") => EventRole::Tool,
            _ => EventRole::System,
        },
        CaptureProvider::Cursor => provider_role(
            value
                .get("role")
                .or_else(|| value.pointer("/message/role"))
                .and_then(Value::as_str),
        ),
        CaptureProvider::Windsurf => match value.get("type").and_then(Value::as_str) {
            Some("user_input") => EventRole::User,
            Some("planner_response") => EventRole::Assistant,
            Some("code_action") => EventRole::Tool,
            _ => EventRole::Unknown,
        },
        CaptureProvider::Qoder => provider_role(
            value
                .pointer("/message/role")
                .or_else(|| value.get("type"))
                .and_then(Value::as_str),
        ),
        CaptureProvider::QwenCode => provider_role(
            value
                .pointer("/message/role")
                .or_else(|| value.get("type"))
                .and_then(Value::as_str),
        ),
        _ => EventRole::Unknown,
    }
}

pub(crate) fn native_jsonl_event_text(
    provider: CaptureProvider,
    value: &Value,
    event_type: EventType,
    entry_type: &str,
) -> String {
    match provider {
        CaptureProvider::Antigravity => value
            .get("content")
            .and_then(provider_value_text)
            .map(|content| {
                value
                    .get("tool_calls")
                    .and_then(antigravity_tool_call_text)
                    .map(|tools| format!("{content}\n{tools}"))
                    .unwrap_or(content)
            })
            .or_else(|| value.get("thinking").and_then(provider_value_text))
            .or_else(|| value.get("tool_calls").and_then(antigravity_tool_call_text))
            .unwrap_or_default(),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value
            .get("content")
            .and_then(provider_value_text)
            .or_else(|| value.get("toolCalls").and_then(provider_value_text))
            .or_else(|| value.get("$set").and_then(provider_value_text))
            .or_else(|| {
                value
                    .get("$rewindTo")
                    .and_then(Value::as_str)
                    .map(|id| format!("rewind to {id}"))
            })
            .unwrap_or_default(),
        CaptureProvider::FactoryAiDroid => value
            .get("content")
            .or_else(|| value.pointer("/message/content"))
            .and_then(provider_value_text)
            .or_else(|| {
                value
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| value.get("items").and_then(provider_value_text))
            .unwrap_or_default(),
        CaptureProvider::CopilotCli => value
            .pointer("/data/content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                value
                    .pointer("/data/result/content")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                value
                    .pointer("/data/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                value
                    .pointer("/data/toolName")
                    .and_then(Value::as_str)
                    .map(|tool| format!("tool {tool}"))
            })
            .unwrap_or_default(),
        CaptureProvider::Cursor => value
            .pointer("/message/content")
            .or_else(|| value.get("content"))
            .and_then(provider_value_text)
            .or_else(|| value.get("text").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default(),
        CaptureProvider::Windsurf => windsurf_event_text(value, entry_type),
        CaptureProvider::Qoder => {
            let primary = if event_type == EventType::ToolOutput {
                value
                    .get("toolUseResult")
                    .or_else(|| value.pointer("/message/content"))
            } else {
                value
                    .pointer("/message/content")
                    .or_else(|| value.get("toolUseResult"))
            };
            primary
                .or_else(|| value.pointer("/data/content"))
                .and_then(provider_value_text)
                .unwrap_or_default()
        }
        CaptureProvider::QwenCode => value
            .pointer("/message/content")
            .or_else(|| value.get("message"))
            .and_then(provider_value_text)
            .or_else(|| value.get("toolCallResult").and_then(provider_value_text))
            .or_else(|| value.get("content").and_then(provider_value_text))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub(crate) fn native_jsonl_model(provider: CaptureProvider, value: &Value) -> Option<Value> {
    match provider {
        CaptureProvider::Antigravity => value.get("model").cloned(),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value.get("model").cloned(),
        CaptureProvider::FactoryAiDroid => value
            .get("model")
            .cloned()
            .or_else(|| value.pointer("/message/model").cloned())
            .or_else(|| value.pointer("/metadata/model").cloned()),
        CaptureProvider::CopilotCli => value.pointer("/data/selectedModel").cloned(),
        CaptureProvider::QwenCode => value
            .get("model")
            .cloned()
            .or_else(|| value.pointer("/message/model").cloned()),
        CaptureProvider::Qoder => value
            .get("model")
            .cloned()
            .or_else(|| value.pointer("/message/model").cloned()),
        _ => None,
    }
}

pub(crate) fn native_jsonl_tokens(_provider: CaptureProvider, value: &Value) -> Option<Value> {
    value
        .get("tokens")
        .or_else(|| value.get("usageMetadata"))
        .cloned()
}

pub(crate) fn gemini_tool_calls_have_result(value: &Value) -> bool {
    value
        .get("toolCalls")
        .and_then(Value::as_array)
        .map(|calls| calls.iter().any(|call| call.get("result").is_some()))
        .unwrap_or(false)
}

pub(crate) fn droid_content_has(value: &Value, expected: &str) -> bool {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}

pub(crate) fn native_jsonl_content_has(value: &Value, expected: &str) -> bool {
    value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}

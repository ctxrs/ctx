#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct AntigravityCliImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for AntigravityCliImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AntigravityCliJsonlAdapter;

impl ProviderCaptureAdapter for AntigravityCliJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Antigravity
    }

    fn source_format(&self) -> &str {
        ANTIGRAVITY_CLI_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::Antigravity,
            ANTIGRAVITY_CLI_SOURCE_FORMAT,
        )
    }
}

pub fn import_antigravity_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: AntigravityCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        AntigravityCliJsonlAdapter,
    )
}

pub(crate) const ANTIGRAVITY_CLI_SOURCE_FORMAT: &str = "antigravity_cli_transcript_jsonl_tree";

pub(crate) fn antigravity_tool_call_text(value: &Value) -> Option<String> {
    value.as_array().and_then(|calls| {
        let names: Vec<&str> = calls
            .iter()
            .filter_map(|call| call.get("name").and_then(Value::as_str))
            .collect();
        if names.is_empty() {
            None
        } else {
            Some(format!("tool calls: {}", names.join(", ")))
        }
    })
}

pub(crate) fn native_jsonl_missing_reason(provider: CaptureProvider) -> &'static str {
    match provider {
        CaptureProvider::Pi => "no Pi session JSONL files found",
        CaptureProvider::Antigravity => {
            "no Antigravity transcript JSONL files found under brain/*/.system_generated/logs"
        }
        CaptureProvider::Gemini => "no Gemini CLI chat JSONL transcripts found under chats",
        CaptureProvider::Tabnine => "no Tabnine CLI chat JSONL transcripts found under chats",
        CaptureProvider::Cursor => {
            "no Cursor agent transcript JSONL files found under projects/*/agent-transcripts"
        }
        CaptureProvider::Windsurf => {
            "no Windsurf Cascade hook transcript JSONL files found under ~/.windsurf/transcripts"
        }
        CaptureProvider::Qoder => {
            "no Qoder transcript JSONL files found under ~/.qoder/projects/*/transcript"
        }
        CaptureProvider::CopilotCli => "no Copilot CLI session events.jsonl transcripts found",
        CaptureProvider::FactoryAiDroid => "no Factory AI Droid session JSONL transcripts found",
        CaptureProvider::QwenCode => "no Qwen Code chat JSONL transcripts found under chats",
        CaptureProvider::KimiCodeCli => "no Kimi Code CLI wire.jsonl transcripts found",
        CaptureProvider::MistralVibe => {
            "no Mistral Vibe meta.json/messages.jsonl session directories found"
        }
        CaptureProvider::Mux => "no Mux chat.jsonl or partial.json session files found",
        _ => "no native provider JSONL transcripts found",
    }
}

pub(crate) fn provider_jsonl_path_is_native(provider: CaptureProvider, path: &Path) -> bool {
    match provider {
        CaptureProvider::Antigravity => {
            matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("transcript_full.jsonl" | "transcript.jsonl")
            )
        }
        CaptureProvider::Gemini | CaptureProvider::Tabnine => path
            .components()
            .any(|component| component.as_os_str() == "chats"),
        CaptureProvider::Cursor => path
            .components()
            .any(|component| component.as_os_str() == "agent-transcripts"),
        CaptureProvider::Windsurf => path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
        CaptureProvider::Qoder => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "transcript")
        }
        CaptureProvider::CopilotCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }
        CaptureProvider::QwenCode => path
            .components()
            .any(|component| component.as_os_str() == "chats"),
        CaptureProvider::KimiCodeCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agents")
        }
        _ => true,
    }
}

pub(crate) fn antigravity_preferred_transcript_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut by_session: BTreeMap<String, PathBuf> = BTreeMap::new();
    for path in paths {
        let session =
            antigravity_session_id_from_path(&path).unwrap_or_else(|| path.display().to_string());
        let prefer_new =
            path.file_name().and_then(|name| name.to_str()) == Some("transcript_full.jsonl");
        let replace = by_session
            .get(&session)
            .map(|current| {
                prefer_new
                    && current.file_name().and_then(|name| name.to_str())
                        != Some("transcript_full.jsonl")
            })
            .unwrap_or(true);
        if replace {
            by_session.insert(session, path);
        }
    }
    by_session.into_values().collect()
}

pub(crate) fn native_jsonl_header_start_time(
    provider: CaptureProvider,
    value: &Value,
) -> Option<DateTime<Utc>> {
    match provider {
        CaptureProvider::Antigravity => value.get("created_at").and_then(Value::as_str),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            value.get("startTime").and_then(Value::as_str)
        }
        CaptureProvider::CopilotCli => value.pointer("/data/startTime").and_then(Value::as_str),
        _ => None,
    }
    .and_then(parse_rfc3339_utc)
}

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
        CaptureProvider::FactoryAiDroid => provider_role(value.get("role").and_then(Value::as_str)),
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
            .unwrap_or_else(|| format!("Antigravity event: {entry_type}")),
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
            .unwrap_or_else(|| {
                let name = if provider == CaptureProvider::Tabnine {
                    "Tabnine"
                } else {
                    "Gemini"
                };
                format!("{name} event: {entry_type}")
            }),
        CaptureProvider::FactoryAiDroid => value
            .get("content")
            .and_then(provider_value_text)
            .or_else(|| {
                value
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| value.get("items").and_then(provider_value_text))
            .unwrap_or_else(|| format!("Factory AI Droid event: {entry_type}")),
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
            .unwrap_or_else(|| format!("Copilot CLI event: {entry_type}")),
        CaptureProvider::Cursor => value
            .pointer("/message/content")
            .or_else(|| value.get("content"))
            .and_then(provider_value_text)
            .or_else(|| value.get("text").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| format!("Cursor event: {entry_type}")),
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
                .unwrap_or_else(|| format!("Qoder event: {entry_type}"))
        }
        CaptureProvider::QwenCode => value
            .pointer("/message/content")
            .or_else(|| value.get("message"))
            .and_then(provider_value_text)
            .or_else(|| value.get("toolCallResult").and_then(provider_value_text))
            .or_else(|| value.get("content").and_then(provider_value_text))
            .unwrap_or_else(|| format!("Qwen Code event: {entry_type}")),
        _ => serde_json::to_string(value).unwrap_or_else(|_| entry_type.to_owned()),
    }
}

pub(crate) fn native_jsonl_model(provider: CaptureProvider, value: &Value) -> Option<Value> {
    match provider {
        CaptureProvider::Antigravity => value.get("model").cloned(),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value.get("model").cloned(),
        CaptureProvider::FactoryAiDroid => value
            .get("model")
            .cloned()
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

use super::*;
use serde_json::value::RawValue;

use crate::nativepath::raw_json::{audit_json, SelectorGroup};

const MAX_GEMINI_ACTIVITY_SELECTOR_BYTES: usize = 64 * 1024;

mod decoding;
mod output;

use decoding::parse_timestamp;
pub(super) use decoding::{
    decode_result_record, decode_retained_event, nonempty, retained_event_bytes,
    GeminiDecodingError,
};
use output::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeminiRecordClass {
    Header,
    Message,
    ToolCall,
    Result,
    StateNotice,
    RewindNotice,
    Ignored,
}

#[derive(Debug, Default)]
struct Presence(bool);

impl<'de> Deserialize<'de> for Presence {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        IgnoredAny::deserialize(deserializer)?;
        Ok(Self(true))
    }
}

#[derive(Debug, Default)]
pub(super) struct GeminiRecordProbe {
    pub(super) id: Option<String>,
    session_id: Option<String>,
    record_type: Option<String>,
    tool_calls: Option<GeminiToolCallSummary>,
    set: Presence,
    rewind_to: Presence,
    result: Presence,
}

impl<'de> Deserialize<'de> for GeminiRecordProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ProbeVisitor;

        impl<'de> Visitor<'de> for ProbeVisitor {
            type Value = GeminiRecordProbe;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one duplicate-tolerant Gemini record")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut probe = GeminiRecordProbe::default();
                let mut id_seen = false;
                let mut session_id_seen = false;
                let mut timestamp_seen = false;
                let mut start_time_seen = false;
                let mut project_hash_seen = false;
                let mut kind_seen = false;
                let mut record_type_seen = false;
                let mut tool_calls_seen = false;
                let mut set_seen = false;
                let mut rewind_to_seen = false;
                let mut result_seen = false;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "id" => {
                            reject_duplicate_selector(&mut id_seen, "id")?;
                            probe.id = map.next_value()?;
                        }
                        "sessionId" => {
                            reject_duplicate_selector(&mut session_id_seen, "sessionId")?;
                            probe.session_id = map.next_value()?;
                        }
                        "timestamp" => {
                            reject_duplicate_selector(&mut timestamp_seen, "timestamp")?;
                            map.next_value::<IgnoredAny>()?;
                        }
                        "startTime" => {
                            reject_duplicate_selector(&mut start_time_seen, "startTime")?;
                            map.next_value::<IgnoredAny>()?;
                        }
                        "projectHash" => {
                            reject_duplicate_selector(&mut project_hash_seen, "projectHash")?;
                            map.next_value::<IgnoredAny>()?;
                        }
                        "kind" => {
                            reject_duplicate_selector(&mut kind_seen, "kind")?;
                            map.next_value::<IgnoredAny>()?;
                        }
                        "type" => {
                            reject_duplicate_selector(&mut record_type_seen, "type")?;
                            probe.record_type = map.next_value()?;
                        }
                        "toolCalls" => {
                            reject_duplicate_selector(&mut tool_calls_seen, "toolCalls")?;
                            probe.tool_calls = map.next_value()?;
                        }
                        "$set" => {
                            reject_duplicate_selector(&mut set_seen, "$set")?;
                            probe.set = map.next_value()?;
                        }
                        "$rewindTo" => {
                            reject_duplicate_selector(&mut rewind_to_seen, "$rewindTo")?;
                            probe.rewind_to = map.next_value()?;
                        }
                        "result" => {
                            reject_duplicate_selector(&mut result_seen, "result")?;
                            probe.result = map.next_value()?;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(probe)
            }
        }

        deserializer.deserialize_map(ProbeVisitor)
    }
}

#[derive(Debug, Default)]
struct GeminiToolCallProbe {
    result: Presence,
}

impl<'de> Deserialize<'de> for GeminiToolCallProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ToolCallProbeVisitor;

        impl<'de> Visitor<'de> for ToolCallProbeVisitor {
            type Value = GeminiToolCallProbe;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one tolerant Gemini tool call")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut probe = GeminiToolCallProbe::default();
                let mut result_seen = false;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "result" {
                        reject_duplicate_selector(&mut result_seen, "result")?;
                        probe.result = map.next_value()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(probe)
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(GeminiToolCallProbe::default())
            }
        }

        deserializer.deserialize_any(ToolCallProbeVisitor)
    }
}

fn reject_duplicate_selector<E: serde::de::Error>(
    seen: &mut bool,
    field: &'static str,
) -> std::result::Result<(), E> {
    if *seen {
        Err(E::duplicate_field(field))
    } else {
        *seen = true;
        Ok(())
    }
}

#[derive(Debug, Default)]
struct GeminiToolCallSummary {
    has_calls: bool,
    has_result: bool,
}

impl<'de> Deserialize<'de> for GeminiToolCallSummary {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SummaryVisitor;

        impl<'de> Visitor<'de> for SummaryVisitor {
            type Value = GeminiToolCallSummary;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Gemini toolCalls array")
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut summary = GeminiToolCallSummary::default();
                while let Some(call) = sequence.next_element::<GeminiToolCallProbe>()? {
                    summary.has_calls = true;
                    summary.has_result |= call.result.0;
                }
                Ok(summary)
            }
        }

        deserializer.deserialize_seq(SummaryVisitor)
    }
}

impl GeminiRecordProbe {
    pub(super) fn classify(&self) -> GeminiRecordClass {
        if self
            .session_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return GeminiRecordClass::Header;
        }
        let has_calls = self
            .tool_calls
            .as_ref()
            .is_some_and(|calls| calls.has_calls);
        let has_result = self.result.0
            || self
                .tool_calls
                .as_ref()
                .is_some_and(|calls| calls.has_result);
        if has_result {
            GeminiRecordClass::Result
        } else if has_calls {
            GeminiRecordClass::ToolCall
        } else if self.set.0 {
            GeminiRecordClass::StateNotice
        } else if self.rewind_to.0 {
            GeminiRecordClass::RewindNotice
        } else if matches!(self.record_type.as_deref(), Some("user" | "gemini")) {
            GeminiRecordClass::Message
        } else {
            GeminiRecordClass::Ignored
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiHeaderDto {
    session_id: String,
    start_time: Option<String>,
    project_hash: Option<String>,
    kind: Option<String>,
    #[serde(default)]
    directories: Vec<String>,
}

pub(super) fn decode_header(
    payload: &[u8],
    layout: &GeminiTranscriptLayout,
) -> std::result::Result<GeminiSession, String> {
    let header: GeminiHeaderDto = serde_json::from_slice(payload)
        .map_err(|error| format!("invalid Gemini header: {error}"))?;
    if header.session_id.trim().is_empty() {
        return Err("Gemini header has an empty sessionId".to_owned());
    }
    let parent_native_session_id = match layout {
        GeminiTranscriptLayout::Primary => None,
        GeminiTranscriptLayout::Subagent {
            parent_native_session_id_hint,
        } => Some(parent_native_session_id_hint.clone()),
    };
    // Layout contributes only an unresolved parent-session hint. Persisted
    // scope semantics must come from provider-native header data so moving an
    // unchanged recording between accepted layouts cannot change its output.
    let agent_scope = if header.kind.as_deref() == Some("subagent") {
        AgentScope::Subagent
    } else {
        AgentScope::Primary
    };
    let mut directories = header
        .directories
        .into_iter()
        .filter(|directory| !directory.trim().is_empty())
        .collect::<Vec<_>>();
    directories.dedup();
    let (cwd, cwd_ambiguous) = match directories.as_slice() {
        [] => (None, false),
        [directory] => (Some(directory.clone()), false),
        _ => (None, true),
    };
    let started_at = header.start_time.as_deref().and_then(parse_timestamp);
    Ok(GeminiSession {
        native_session_id: header.session_id,
        native_start_time: header.start_time,
        project_hash: header.project_hash,
        parent_native_session_id,
        agent_scope,
        started_at,
        cwd,
        cwd_ambiguous,
        native_kind: header.kind,
    })
}

#[derive(Debug, Deserialize)]
struct GeminiMessageDto {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "type")]
    record_type: Option<String>,
    content: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Default)]
struct GeminiToolCallRecordDto {
    id: Option<String>,
    timestamp: Option<String>,
    content: Option<Value>,
    tool_calls: Vec<Box<RawValue>>,
}

impl<'de> Deserialize<'de> for GeminiToolCallRecordDto {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ToolCallRecordVisitor;

        impl<'de> Visitor<'de> for ToolCallRecordVisitor {
            type Value = GeminiToolCallRecordDto;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one duplicate-tolerant Gemini tool-call record")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut record = GeminiToolCallRecordDto::default();
                let mut id_seen = false;
                let mut timestamp_seen = false;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "id" => {
                            reject_duplicate_selector(&mut id_seen, "id")?;
                            record.id = map.next_value()?;
                        }
                        "timestamp" => {
                            reject_duplicate_selector(&mut timestamp_seen, "timestamp")?;
                            record.timestamp = map.next_value()?;
                        }
                        "content" => record.content = map.next_value()?,
                        "toolCalls" => record.tool_calls = map.next_value()?,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(record)
            }
        }

        deserializer.deserialize_map(ToolCallRecordVisitor)
    }
}

fn decode_native_tool_call(
    raw: &RawValue,
    record_selectors_unavailable: bool,
) -> std::result::Result<GeminiToolCall, String> {
    let audit = audit_json(
        raw.get().as_bytes(),
        gemini_selector_group,
        gemini_literal_kind_for_key,
    )
    .map_err(|error| format!("invalid Gemini tool call: {error}"))?;
    let native_content: Value = serde_json::from_str(raw.get())
        .map_err(|error| format!("invalid Gemini tool call: {error}"))?;
    let object = native_content
        .as_object()
        .ok_or_else(|| "Gemini tool call must be an object".to_owned())?;
    let (id, id_invalid) = bounded_native_string(object.get("id"));
    let (name, name_invalid) = bounded_native_string(object.get("name"));
    let (protocol, protocol_invalid) = bounded_native_string(object.get("protocol"));
    let (server, server_invalid) = bounded_native_string(object.get("server"));
    let (explicit_tool, explicit_tool_invalid) = bounded_native_string(object.get("tool"));
    let call_id_unavailable = record_selectors_unavailable
        || audit.selector_ambiguous(SelectorGroup::CallId)
        || id_invalid;
    let tool_name_unavailable = record_selectors_unavailable
        || audit.selector_ambiguous(SelectorGroup::ToolName)
        || name_invalid;
    let arguments_unavailable =
        record_selectors_unavailable || audit.selector_ambiguous(SelectorGroup::Arguments);
    let mcp_identity_unavailable = record_selectors_unavailable
        || audit.selector_ambiguous(SelectorGroup::Protocol)
        || audit.selector_ambiguous(SelectorGroup::Server)
        || audit.selector_ambiguous(SelectorGroup::McpTool)
        || protocol_invalid
        || server_invalid
        || explicit_tool_invalid;
    let args = if arguments_unavailable {
        None
    } else {
        object.get("args").cloned()
    };
    Ok(GeminiToolCall {
        native_content,
        id: (!call_id_unavailable).then_some(id).flatten(),
        name: (!tool_name_unavailable).then_some(name).flatten(),
        args,
        protocol: (!mcp_identity_unavailable).then_some(protocol).flatten(),
        server: (!mcp_identity_unavailable).then_some(server).flatten(),
        explicit_tool: (!mcp_identity_unavailable)
            .then_some(explicit_tool)
            .flatten(),
        call_id_unavailable,
        tool_name_unavailable,
        arguments_unavailable,
        mcp_identity_unavailable,
        native_content_unavailable: record_selectors_unavailable
            || audit.any_selector_ambiguous()
            || id_invalid
            || name_invalid
            || protocol_invalid
            || server_invalid
            || explicit_tool_invalid,
        literal_facts: audit.facts().to_vec(),
    })
}

fn gemini_tool_call_record_selector_group(key: &str) -> Option<SelectorGroup> {
    match key {
        "type" => Some(SelectorGroup::Type),
        "id" => Some(SelectorGroup::Invocation),
        "toolCalls" => Some(SelectorGroup::ToolCalls),
        _ => None,
    }
}

fn bounded_native_string(value: Option<&Value>) -> (Option<String>, bool) {
    match value {
        None | Some(Value::Null) => (None, false),
        Some(Value::String(value))
            if !value.is_empty() && value.len() <= MAX_GEMINI_ACTIVITY_SELECTOR_BYTES =>
        {
            (Some(value.clone()), false)
        }
        Some(Value::String(_)) | Some(_) => (None, true),
    }
}

fn gemini_selector_group(key: &str) -> Option<SelectorGroup> {
    match key {
        "type" => Some(SelectorGroup::Type),
        "id" | "call_id" | "callId" => Some(SelectorGroup::CallId),
        "name" => Some(SelectorGroup::ToolName),
        "args" | "arguments" | "input" => Some(SelectorGroup::Arguments),
        "result" | "output" => Some(SelectorGroup::Result),
        "protocol" => Some(SelectorGroup::Protocol),
        "server" => Some(SelectorGroup::Server),
        "tool" => Some(SelectorGroup::McpTool),
        "content" => Some(SelectorGroup::Content),
        "toolCalls" => Some(SelectorGroup::ToolCalls),
        "invocation" => Some(SelectorGroup::Invocation),
        _ => None,
    }
}

fn gemini_literal_kind_for_key(key: &str) -> Option<ctx_history_core::LiteralFactKind> {
    use ctx_history_core::LiteralFactKind;
    match key {
        "cwd" | "workspaceDirectory" | "workspace_directory" | "workdir" | "working_directory" => {
            Some(LiteralFactKind::ToolWorkdir)
        }
        "file" | "file_path" | "filePath" | "filepath" | "path" | "paths" | "file_paths"
        | "filePaths" | "old_path" | "new_path" => Some(LiteralFactKind::File),
        "url" | "uri" | "repository_url" | "repositoryUrl" | "remote_url" | "remoteUrl" => {
            Some(LiteralFactKind::Url)
        }
        "forge" | "forge_url" | "forgeUrl" => Some(LiteralFactKind::Forge),
        "project" | "project_id" | "projectId" | "repository" | "repo" => {
            Some(LiteralFactKind::Project)
        }
        "vcs" | "git" | "version_control" => Some(LiteralFactKind::Vcs),
        "commit" | "commit_id" | "commitId" | "commit_sha" | "sha" => Some(LiteralFactKind::Commit),
        "pull_request" | "pullRequest" | "pull_request_id" | "pr" | "pr_id" | "merge_request"
        | "mergeRequest" => Some(LiteralFactKind::PullRequest),
        "command" | "cmd" => Some(LiteralFactKind::Command),
        "branch" | "branch_name" | "branchName" => Some(LiteralFactKind::Branch),
        "workspace" | "workspace_id" | "workspaceId" => Some(LiteralFactKind::Workspace),
        _ => None,
    }
}

struct ProbedGeminiOutput {
    native_content: Value,
    result: Option<Value>,
    call_id: Option<String>,
    tool_name: Option<String>,
    arguments: Option<Value>,
    protocol: Option<String>,
    server: Option<String>,
    explicit_tool: Option<String>,
    call_id_unavailable: bool,
    tool_name_unavailable: bool,
    arguments_unavailable: bool,
    result_unavailable: bool,
    mcp_identity_unavailable: bool,
    native_content_unavailable: bool,
    literal_facts: Vec<ctx_history_core::ProviderDeclaredFact>,
    fallback_identity_sha256: [u8; 32],
}

struct ProbedGeminiResult {
    native_record_id: Option<String>,
    occurred_at_unix_ms: Option<i64>,
    outputs: Vec<ProbedGeminiOutput>,
}

pub(super) struct DecodedGeminiResult {
    pub(super) events: Vec<(GeminiRetainedEvent, usize)>,
}

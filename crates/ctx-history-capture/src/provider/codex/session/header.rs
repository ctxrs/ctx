use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Fidelity, ProviderCaptureEnvelope, ProviderCursorCheckpoint,
    ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, SessionStatus, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::importer::{provider_cursor_stream, BoundedParserCheckpoint};
use crate::provider::normalization::provider_capped_json;
use crate::{CaptureError, ProviderAdapterContext, Result, CODEX_SESSION_SOURCE_FORMAT};

const CODEX_MAX_HEADER_SOURCE_CHARS: usize = 32 * 1024;
const CODEX_MAX_RAW_HEADER_KEYS: usize = 128;
const CODEX_MAX_RAW_HEADER_KEY_BYTES: usize = 512;

#[derive(Debug, Clone)]
pub(crate) struct CodexSessionHeader {
    pub(crate) id: String,
    pub(crate) root_session: Option<String>,
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) cwd: Option<String>,
    pub(crate) originator: Option<String>,
    pub(crate) cli_version: Option<String>,
    pub(crate) source: Value,
    pub(crate) parent_session: Option<String>,
    pub(crate) agent_nickname: Option<String>,
    pub(crate) agent_role: Option<String>,
    pub(crate) model_provider: Option<String>,
    pub(crate) raw: Value,
}
pub(crate) fn codex_session_header(value: Value) -> Result<CodexSessionHeader> {
    let payload = value
        .get("payload")
        .ok_or_else(|| CaptureError::InvalidPayload("codex session_meta missing payload".into()))?;
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| CaptureError::InvalidPayload("codex session_meta missing id".into()))?
        .to_owned();
    let timestamp = payload
        .get("timestamp")
        .and_then(Value::as_str)
        .or_else(|| value.get("timestamp").and_then(Value::as_str))
        .and_then(parse_rfc3339_utc)
        .ok_or_else(|| {
            CaptureError::InvalidPayload("codex session_meta missing timestamp".into())
        })?;
    let source = payload.get("source").cloned().unwrap_or(Value::Null);
    let root_session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|root_id| !root_id.trim().is_empty())
        .map(str::to_owned);
    let parent_session = source
        .pointer("/subagent/thread_spawn/parent_thread_id")
        .or_else(|| source.pointer("/thread_spawn/parent_thread_id"))
        .or_else(|| source.get("parent_thread_id"))
        .or_else(|| payload.get("parent_thread_id"))
        .or_else(|| payload.get("forked_from_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned);

    Ok(CodexSessionHeader {
        id,
        root_session,
        timestamp,
        cwd: payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_owned),
        originator: payload
            .get("originator")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cli_version: payload
            .get("cli_version")
            .and_then(Value::as_str)
            .map(str::to_owned),
        source,
        parent_session,
        agent_nickname: payload
            .get("agent_nickname")
            .and_then(Value::as_str)
            .map(str::to_owned),
        agent_role: payload
            .get("agent_role")
            .and_then(Value::as_str)
            .map(str::to_owned),
        model_provider: payload
            .get("model_provider")
            .and_then(Value::as_str)
            .map(str::to_owned),
        raw: value,
    })
}
pub(crate) fn codex_session_capture(
    header: &CodexSessionHeader,
    event: Option<ProviderEventEnvelope>,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let cursor = Some(ProviderCursorRange {
        before: None,
        after: Some(ProviderCursorCheckpoint {
            stream: provider_cursor_stream(CaptureProvider::Codex, CODEX_SESSION_SOURCE_FORMAT),
            cursor: format!("line:{line_number}"),
            observed_at: occurred_at,
        }),
    });
    let is_subagent = header.parent_session.is_some();
    let role_hint = header
        .agent_role
        .clone()
        .or_else(|| is_subagent.then(|| "subagent".to_owned()))
        .or_else(|| Some("primary".to_owned()));

    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::Codex,
        source: ProviderSourceEnvelope {
            source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: context
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            source_root: context.source_root_display(),
            trust: ProviderSourceTrust::ProviderExport,
            fidelity: Fidelity::Imported,
            cursor,
            idempotency_key: Some(format!(
                "provider-source:codex:{CODEX_SESSION_SOURCE_FORMAT}:{}",
                header.id
            )),
            metadata: json!({
                "adapter": CODEX_SESSION_SOURCE_FORMAT,
                "source_fidelity": "codex_rollout_jsonl",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: header.id.clone(),
            parent_provider_session_id: header.parent_session.clone(),
            root_provider_session_id: header
                .root_session
                .as_ref()
                .filter(|root_id| *root_id != &header.id)
                .cloned()
                .or_else(|| header.parent_session.clone()),
            external_agent_id: header.agent_nickname.clone(),
            agent_type: if is_subagent {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint,
            is_primary: !is_subagent,
            status: SessionStatus::Imported,
            started_at: header.timestamp,
            ended_at: None,
            cwd: header.cwd.clone(),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!("provider-session:codex:{}", header.id)),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "source_fidelity": "codex_rollout_jsonl",
                "originator": header.originator,
                "cli_version": header.cli_version,
                "source": header.source,
                "agent_nickname": header.agent_nickname,
                "agent_role": header.agent_role,
                "model_provider": header.model_provider,
                "parent_session": header.parent_session,
                "root_session": header.root_session,
                "raw_session_meta_keys": header.raw.as_object().map(|object| object.keys().cloned().collect::<Vec<_>>()),
                "import_profile": "default",
                "limitations": [
                    "default profile indexes session metadata, user and assistant messages, compacted context summaries, reasoning summaries, tool-call metadata, typed result outcome/evidence, file touches, and parent-child session edges where present",
                    "command and tool result bodies are source-backed and omitted from the Store, FTS, and canonical journal; eligible Codex bodies can be re-read and hash-verified from raw_source_path for a transient Pro request",
                    "raw diffs, encrypted reasoning content, bootstrap context, lifecycle notices, and binary artifacts remain in the raw transcript referenced by raw_source_path"
                ],
            }),
        },
        event,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexHeaderCheckpoint {
    id: String,
    root_session: Option<String>,
    timestamp: chrono::DateTime<chrono::Utc>,
    cwd: Option<String>,
    originator: Option<String>,
    cli_version: Option<String>,
    parent_session: Option<String>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    model_provider: Option<String>,
    raw_keys: Vec<String>,
}

impl CodexHeaderCheckpoint {
    fn from_header(header: &CodexSessionHeader) -> Self {
        let raw_keys = header
            .raw
            .as_object()
            .into_iter()
            .flat_map(|object| object.keys())
            .take(CODEX_MAX_RAW_HEADER_KEYS)
            .map(|key| truncate_header_utf8(key, CODEX_MAX_RAW_HEADER_KEY_BYTES))
            .collect();
        Self {
            id: header.id.clone(),
            root_session: header.root_session.clone(),
            timestamp: header.timestamp,
            cwd: header.cwd.clone(),
            originator: header.originator.clone(),
            cli_version: header.cli_version.clone(),
            parent_session: header.parent_session.clone(),
            agent_nickname: header.agent_nickname.clone(),
            agent_role: header.agent_role.clone(),
            model_provider: header.model_provider.clone(),
            raw_keys,
        }
    }

    fn into_header(self, source: Value) -> CodexSessionHeader {
        let raw = self
            .raw_keys
            .iter()
            .cloned()
            .map(|key| (key, Value::Null))
            .collect();
        CodexSessionHeader {
            id: self.id,
            root_session: self.root_session,
            timestamp: self.timestamp,
            cwd: self.cwd,
            originator: self.originator,
            cli_version: self.cli_version,
            source,
            parent_session: self.parent_session,
            agent_nickname: self.agent_nickname,
            agent_role: self.agent_role,
            model_provider: self.model_provider,
            raw: Value::Object(raw),
        }
    }
}

fn truncate_header_utf8(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value[..boundary].to_owned()
}

fn bounded_codex_header_source(source: &Value) -> Value {
    match serde_json::to_vec(source) {
        Ok(encoded) if encoded.len() <= CODEX_MAX_HEADER_SOURCE_CHARS => source.clone(),
        _ => provider_capped_json(source, CODEX_MAX_HEADER_SOURCE_CHARS),
    }
}

pub(super) fn bounded_codex_header(header: CodexSessionHeader) -> Result<CodexSessionHeader> {
    let source = bounded_codex_header_source(&header.source);
    let bounded = CodexHeaderCheckpoint::from_header(&header);
    BoundedParserCheckpoint::from_serializable(&bounded)?;
    Ok(bounded.into_header(source))
}

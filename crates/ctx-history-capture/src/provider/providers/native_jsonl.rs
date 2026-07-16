use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use crate::common::io::collect_jsonl_paths;
use crate::common::time::parse_rfc3339_utc;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::importer::{
    provider_cursor_stream, provider_event_is_real_conversation_message,
    ProviderNormalizationBatcher,
};
use crate::provider::native::{
    antigravity_tool_call_text, provider_capped_json, provider_capped_json_value,
    provider_policy_body, provider_policy_event_text, provider_role, provider_value_text,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderJsonlReader,
    ProviderNormalizationResult, Result, PROVIDER_MAX_PREVIEW_CHARS,
};

mod windsurf;

pub(crate) use windsurf::{windsurf_event_body, windsurf_event_text};

pub(crate) fn normalize_jsonl_tree(
    path: &Path,
    context: &ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &str,
) -> Result<ProviderNormalizationResult> {
    let mut paths = Vec::new();
    collect_jsonl_paths(path, &mut paths)?;
    paths.retain(|path| provider_jsonl_path_is_native(provider, path));
    if provider == CaptureProvider::Antigravity {
        paths = antigravity_preferred_transcript_paths(paths);
    }
    paths.sort();
    if paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(provider),
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for path in paths {
        let mut result =
            normalize_native_jsonl_session_file(&path, context, provider, source_format)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }
    Ok(merged)
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

pub(crate) fn normalize_native_jsonl_session_file(
    path: &Path,
    context: &ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &str,
) -> Result<ProviderNormalizationResult> {
    let mut reader = ProviderJsonlReader::open_replacement(path)?;
    let authoritative_started_at = if provider == CaptureProvider::Tabnine {
        scan_native_jsonl_session_reader(path, &mut reader, context, provider, source_format)?
            .header
            .as_ref()
            .and_then(|header| native_jsonl_header_start_time(provider, header))
    } else {
        None
    };
    normalize_native_jsonl_session_reader(
        path,
        &mut reader,
        context,
        provider,
        source_format,
        None,
        authoritative_started_at,
    )
}

pub(crate) struct NativeJsonlScan {
    pub(crate) header: Option<Value>,
    pub(crate) has_real_message: bool,
    pub(crate) summary: crate::ProviderImportSummary,
    pub(crate) valid_records: usize,
    pub(crate) first_valid_line: Option<usize>,
}

pub(crate) fn scan_native_jsonl_session_reader(
    path: &Path,
    reader: &mut ProviderJsonlReader,
    context: &ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &str,
) -> Result<NativeJsonlScan> {
    let mut summary = crate::ProviderImportSummary::default();
    let mut header = None;
    let mut valid_records = 0usize;
    let mut first_valid_line = None;
    let mut has_real_message = false;
    let mut line = Vec::new();
    let mut line_number = 0usize;
    while reader.read_record_or_skip_oversized(&mut line, &mut line_number, &mut summary)? {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(err) => {
                summary.failed += 1;
                summary.sample_failure(ProviderImportFailure {
                    line: line_number,
                    error: native_jsonl_file_failure(path, format!("malformed JSONL: {err}")),
                });
                continue;
            }
        };
        valid_records += 1;
        first_valid_line.get_or_insert(line_number);
        if header.is_none()
            && (matches!(
                provider,
                CaptureProvider::Antigravity | CaptureProvider::Windsurf
            ) || native_jsonl_header_session_id(provider, &value).is_some())
        {
            header = Some(value.clone());
        }
        let occurred_at = native_jsonl_timestamp(&value).unwrap_or(context.imported_at);
        has_real_message |=
            native_jsonl_event(provider, source_format, &value, line_number, occurred_at)
                .as_ref()
                .is_some_and(provider_event_is_real_conversation_message);
    }
    reader.restart_import_position()?;
    Ok(NativeJsonlScan {
        header,
        has_real_message,
        summary,
        valid_records,
        first_valid_line,
    })
}

pub(crate) fn normalize_native_jsonl_session_reader(
    path: &Path,
    reader: &mut ProviderJsonlReader,
    context: &ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &str,
    bootstrap_header: Option<Value>,
    authoritative_started_at: Option<DateTime<Utc>>,
) -> Result<ProviderNormalizationResult> {
    let scan = scan_native_jsonl_session_reader(path, reader, context, provider, source_format)?;
    let header = if let Some(header) = bootstrap_header {
        if native_jsonl_header_session_id(provider, &header).is_none() {
            let mut result = ProviderNormalizationResult {
                summary: scan.summary,
                ..ProviderNormalizationResult::default()
            };
            result.summary.failed += 1;
            result.summary.sample_failure(ProviderImportFailure {
                line: 1,
                error: "no importable native JSONL session header".to_owned(),
            });
            return Ok(result);
        }
        header
    } else {
        if provider == CaptureProvider::Antigravity {
            if scan.valid_records == 0 {
                let mut result = ProviderNormalizationResult {
                    summary: scan.summary,
                    ..ProviderNormalizationResult::default()
                };
                if result.summary.failed == 0 {
                    result.summary.failed += 1;
                    result.summary.sample_failure(ProviderImportFailure {
                        line: 0,
                        error: native_jsonl_file_failure(
                            path,
                            native_jsonl_missing_reason(provider),
                        ),
                    });
                }
                return Ok(result);
            }
        } else if provider == CaptureProvider::Windsurf {
            if scan.valid_records == 0 {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: native_jsonl_missing_reason(provider),
                });
            }
        } else {
            if scan.valid_records == 0 {
                let mut result = ProviderNormalizationResult {
                    summary: scan.summary,
                    ..ProviderNormalizationResult::default()
                };
                if result.summary.failed == 0 {
                    result.summary.failed += 1;
                    result.summary.sample_failure(ProviderImportFailure {
                        line: 0,
                        error: native_jsonl_missing_reason(provider).to_owned(),
                    });
                }
                return Ok(result);
            }
            if scan.header.is_none() {
                let mut result = ProviderNormalizationResult {
                    summary: scan.summary,
                    ..ProviderNormalizationResult::default()
                };
                result.summary.failed += 1;
                result.summary.sample_failure(ProviderImportFailure {
                    line: scan.first_valid_line.unwrap_or(0),
                    error: "no importable native JSONL session header".to_owned(),
                });
                return Ok(result);
            }
        }
        scan.header
            .expect("a native JSONL header was established by the bounded scan")
    };
    let started_at = authoritative_started_at.unwrap_or_else(|| {
        native_jsonl_timestamp(&header)
            .or_else(|| native_jsonl_header_start_time(provider, &header))
            .unwrap_or(context.imported_at)
    });
    let mut result = ProviderNormalizationResult::default();
    stream_native_jsonl_session_reader(
        path,
        reader,
        context,
        NativeJsonlStreamOptions {
            provider,
            source_format,
            header,
            started_at,
        },
        |mut batch| {
            result.summary.merge(batch.summary);
            result.captures.append(&mut batch.captures);
            result.files_touched.append(&mut batch.files_touched);
            Ok(())
        },
    )?;
    Ok(result)
}

pub(crate) struct NativeJsonlStreamOptions<'a> {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'a str,
    pub(crate) header: Value,
    pub(crate) started_at: DateTime<Utc>,
}

pub(crate) fn stream_native_jsonl_session_reader<F>(
    path: &Path,
    reader: &mut ProviderJsonlReader,
    context: &ProviderAdapterContext,
    options: NativeJsonlStreamOptions<'_>,
    emit: F,
) -> Result<()>
where
    F: FnMut(ProviderNormalizationResult) -> Result<()>,
{
    let metadata = NativeJsonlNormalizationMetadata::new(
        path,
        context,
        options.provider,
        options.source_format,
        options.header,
        options.started_at,
    );
    let mut batches = ProviderNormalizationBatcher::new(emit);
    let mut line = Vec::new();
    let mut line_number = 0usize;
    while reader.read_record_or_skip_oversized(
        &mut line,
        &mut line_number,
        &mut batches.current_mut().summary,
    )? {
        if line.iter().all(u8::is_ascii_whitespace) {
            batches.record_processed()?;
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(err) => {
                let result = batches.current_mut();
                result.summary.failed += 1;
                result.summary.sample_failure(ProviderImportFailure {
                    line: line_number,
                    error: native_jsonl_file_failure(path, format!("malformed JSONL: {err}")),
                });
                batches.record_processed()?;
                continue;
            }
        };
        metadata.push_row(batches.current_mut(), line_number, value);
        batches.record_processed()?;
    }
    batches.finish()
}

struct NativeJsonlNormalizationMetadata {
    provider: CaptureProvider,
    source_format: String,
    header: Value,
    native_session_id: String,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    external_agent_id: Option<String>,
    agent_type: AgentType,
    is_subagent: bool,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    machine_id: String,
    imported_at: DateTime<Utc>,
    raw_source_path: String,
    source_root: Option<String>,
}

impl NativeJsonlNormalizationMetadata {
    fn new(
        path: &Path,
        context: &ProviderAdapterContext,
        provider: CaptureProvider,
        source_format: &str,
        header: Value,
        started_at: DateTime<Utc>,
    ) -> Self {
        let native_session_id = match provider {
            CaptureProvider::Antigravity => antigravity_session_id_from_path(path)
                .unwrap_or_else(|| "unknown-session".to_owned()),
            CaptureProvider::Windsurf => {
                windsurf_session_id_from_path(path).unwrap_or_else(|| "unknown-session".to_owned())
            }
            _ => native_jsonl_header_session_id(provider, &header)
                .unwrap_or_else(|| "unknown-session".to_owned()),
        };
        let (provider_session_id, parent_provider_session_id, external_agent_id, agent_type) =
            native_jsonl_path_session(provider, path, &header, &native_session_id);
        let is_subagent = parent_provider_session_id.is_some() || agent_type == AgentType::Subagent;
        let raw_source_path = path.display().to_string();
        Self {
            provider,
            source_format: source_format.to_owned(),
            cwd: native_jsonl_header_cwd(provider, &header),
            header,
            native_session_id,
            provider_session_id,
            parent_provider_session_id,
            external_agent_id,
            agent_type,
            is_subagent,
            started_at,
            machine_id: context.machine_id.clone(),
            imported_at: context.imported_at,
            source_root: context
                .source_root_display()
                .or_else(|| Some(raw_source_path.clone())),
            raw_source_path,
        }
    }

    fn push_row(&self, result: &mut ProviderNormalizationResult, line_number: usize, value: Value) {
        let occurred_at = native_jsonl_timestamp(&value).unwrap_or(self.started_at);
        let event = native_jsonl_event(
            self.provider,
            &self.source_format,
            &value,
            line_number,
            occurred_at,
        );
        if let Some(event) = &event {
            result
                .files_touched
                .extend(provider_file_touches_from_raw_value(
                    self.provider,
                    &self.provider_session_id,
                    &self.source_format,
                    Some(self.raw_source_path.as_str()),
                    &value,
                    event,
                    line_number,
                ));
        }
        result.captures.push((
            line_number,
            ProviderCaptureEnvelope {
                schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                provider: self.provider,
                source: ProviderSourceEnvelope {
                    source_format: self.source_format.clone(),
                    machine_id: self.machine_id.clone(),
                    observed_at: self.imported_at,
                    raw_source_path: Some(self.raw_source_path.clone()),
                    source_root: self.source_root.clone(),
                    trust: ProviderSourceTrust::ProviderNative,
                    fidelity: Fidelity::Imported,
                    cursor: Some(ProviderCursorRange {
                        before: None,
                        after: Some(ProviderCursorCheckpoint {
                            stream: provider_cursor_stream(self.provider, &self.source_format),
                            cursor: format!("{}:line:{line_number}", self.raw_source_path),
                            observed_at: occurred_at,
                        }),
                    }),
                    idempotency_key: Some(format!(
                        "provider-source:{}:{}:{}",
                        self.provider.as_str(),
                        self.source_format,
                        self.provider_session_id
                    )),
                    metadata: json!({
                        "adapter": self.source_format,
                        "native_session_id": self.native_session_id,
                        "source_path": self.raw_source_path,
                    }),
                },
                session: ProviderSessionEnvelope {
                    provider_session_id: self.provider_session_id.clone(),
                    parent_provider_session_id: self.parent_provider_session_id.clone(),
                    root_provider_session_id: self.parent_provider_session_id.clone(),
                    external_agent_id: self.external_agent_id.clone(),
                    agent_type: self.agent_type,
                    role_hint: Some(
                        if self.is_subagent {
                            "subagent"
                        } else {
                            "primary"
                        }
                        .to_owned(),
                    ),
                    is_primary: !self.is_subagent,
                    status: native_jsonl_session_status(self.provider, &self.header),
                    started_at: self.started_at,
                    ended_at: None,
                    cwd: self.cwd.clone(),
                    fidelity: Fidelity::Imported,
                    idempotency_key: Some(format!(
                        "provider-session:{}:{}",
                        self.provider.as_str(),
                        self.provider_session_id
                    )),
                    artifacts: Vec::new(),
                    metadata: native_jsonl_session_metadata(
                        self.provider,
                        &self.source_format,
                        &self.header,
                        Path::new(&self.raw_source_path),
                    ),
                },
                event,
            },
        ));
    }
}

fn native_jsonl_file_failure(path: &Path, reason: impl AsRef<str>) -> String {
    format!("{}: {}", path.display(), reason.as_ref())
}

pub(crate) fn native_jsonl_header_session_id(
    provider: CaptureProvider,
    value: &Value,
) -> Option<String> {
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            value.get("sessionId").and_then(Value::as_str)
        }
        CaptureProvider::FactoryAiDroid => (value.get("type").and_then(Value::as_str)
            == Some("session_start"))
        .then(|| {
            value
                .get("sessionId")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
        })
        .flatten(),
        CaptureProvider::CopilotCli => (value.get("type").and_then(Value::as_str)
            == Some("session.start"))
        .then(|| value.pointer("/data/sessionId").and_then(Value::as_str))
        .flatten(),
        CaptureProvider::QwenCode => value.get("sessionId").and_then(Value::as_str),
        CaptureProvider::Qoder => value.get("sessionId").and_then(Value::as_str),
        CaptureProvider::Cursor => (value.get("role").is_some()
            || value.get("event").is_some()
            || value.get("message").is_some())
        .then_some("cursor-path-session"),
        _ => None,
    }
    .filter(|id| !id.trim().is_empty())
    .map(str::to_owned)
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

pub(crate) fn native_jsonl_header_cwd(provider: CaptureProvider, value: &Value) -> Option<String> {
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value
            .get("directories")
            .and_then(Value::as_array)
            .and_then(|dirs| dirs.first())
            .and_then(Value::as_str),
        CaptureProvider::FactoryAiDroid => value.get("cwd").and_then(Value::as_str),
        CaptureProvider::CopilotCli => value.pointer("/data/context/cwd").and_then(Value::as_str),
        CaptureProvider::QwenCode => value.get("cwd").and_then(Value::as_str),
        CaptureProvider::Qoder => value.get("cwd").and_then(Value::as_str),
        _ => None,
    }
    .filter(|cwd| !cwd.trim().is_empty())
    .map(str::to_owned)
}

pub(crate) fn native_jsonl_path_session(
    provider: CaptureProvider,
    path: &Path,
    header: &Value,
    native_session_id: &str,
) -> (String, Option<String>, Option<String>, AgentType) {
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            let parent = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str());
            if parent.is_some_and(|name| name != "chats") {
                return (
                    native_session_id.to_owned(),
                    parent.map(str::to_owned),
                    None,
                    AgentType::Subagent,
                );
            }
            (native_session_id.to_owned(), None, None, AgentType::Primary)
        }
        CaptureProvider::FactoryAiDroid => {
            let parent = header
                .get("parent")
                .or_else(|| header.get("callingSessionId"))
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned);
            let agent_type = if parent.is_some()
                || header.get("decompSessionType").and_then(Value::as_str) == Some("worker")
            {
                AgentType::Subagent
            } else {
                AgentType::Primary
            };
            (
                native_session_id.to_owned(),
                parent,
                header
                    .get("decompMissionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                agent_type,
            )
        }
        CaptureProvider::Cursor => {
            let session = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or(native_session_id)
                .to_owned();
            (session, None, None, AgentType::Primary)
        }
        _ => (native_session_id.to_owned(), None, None, AgentType::Primary),
    }
}

include!("native_jsonl/events.rs");

#[cfg(test)]
include!("native_jsonl/tests.rs");

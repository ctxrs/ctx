use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::common::io::{
    collect_jsonl_paths, ensure_regular_provider_transcript_file, read_provider_jsonl_line,
    read_text_file_limited,
};
use crate::provider::native::{
    native_event, native_provider_capture, provider_capped_json, provider_role,
    provider_timestamp_value, provider_value_text, NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderNormalizationResult,
    Result, MAX_OPENCLAW_SESSION_INDEX_BYTES, MAX_OPENCLAW_SESSION_INDEX_PATHS,
    MAX_OPENCLAW_SESSION_INDEX_VISITED_PATHS, OPENCLAW_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) fn openclaw_agent_id(path: &Path) -> Option<String> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    components.windows(2).find_map(|window| {
        (window[0] == "agents" && !window[1].is_empty()).then(|| window[1].clone())
    })
}

pub(crate) fn provider_path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == expected)
}

pub(crate) fn openclaw_session_indexes(root: &Path) -> BTreeMap<String, Value> {
    let mut indexes = BTreeMap::new();
    let mut paths = Vec::new();
    let mut visited = 0usize;
    collect_named_paths(
        root,
        "sessions.json",
        &mut paths,
        &mut visited,
        MAX_OPENCLAW_SESSION_INDEX_PATHS,
        MAX_OPENCLAW_SESSION_INDEX_VISITED_PATHS,
    );
    for path in paths {
        let Ok(text) = read_text_file_limited(
            &path,
            MAX_OPENCLAW_SESSION_INDEX_BYTES,
            "OpenClaw sessions.json",
        ) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let agent_id = openclaw_agent_id(&path);
        for (key, value) in openclaw_session_index_entries(value) {
            if let Some(session_id) = value
                .get("sessionId")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                if let Some(agent_id) = &agent_id {
                    indexes
                        .entry(format!("{agent_id}/{session_id}"))
                        .or_insert(value.clone());
                }
                indexes
                    .entry(session_id.to_owned())
                    .or_insert(value.clone());
            }
            if let Some(agent_id) = &agent_id {
                indexes
                    .entry(format!("{agent_id}/{key}"))
                    .or_insert(value.clone());
            }
            indexes.entry(key).or_insert(value);
        }
    }
    indexes
}

pub(crate) fn openclaw_session_index_entries(value: Value) -> Vec<(String, Value)> {
    match value {
        Value::Array(items) => items
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                let key = value
                    .get("sessionId")
                    .or_else(|| value.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| index.to_string());
                (key, value)
            })
            .collect(),
        Value::Object(mut map) => {
            if let Some(Value::Array(items)) = map.remove("sessions") {
                return openclaw_session_index_entries(Value::Array(items));
            }
            map.into_iter().collect()
        }
        _ => Vec::new(),
    }
}

pub(crate) fn collect_named_paths(
    root: &Path,
    name: &str,
    paths: &mut Vec<PathBuf>,
    visited: &mut usize,
    max_paths: usize,
    max_visited: usize,
) {
    if paths.len() >= max_paths || *visited >= max_visited {
        return;
    }
    *visited += 1;
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.file_type().is_file() {
        if root.file_name().and_then(|file_name| file_name.to_str()) == Some(name) {
            paths.push(root.to_path_buf());
        }
        return;
    }
    if !metadata.file_type().is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if paths.len() >= max_paths || *visited >= max_visited {
            break;
        }
        collect_named_paths(&entry.path(), name, paths, visited, max_paths, max_visited);
    }
}

pub(crate) fn normalize_openclaw_history(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut paths = Vec::new();
    collect_jsonl_paths(path, &mut paths)?;
    if !path.is_file() {
        paths.retain(|candidate| provider_path_has_component(candidate, "sessions"));
    }
    paths.sort();
    if paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no OpenClaw session JSONL transcripts found",
        });
    }
    let indexes = openclaw_session_indexes(path);
    let mut merged = ProviderNormalizationResult::default();
    for transcript_path in paths {
        let mut result = normalize_openclaw_jsonl_file(&transcript_path, context, &indexes)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }
    Ok(merged)
}

pub(crate) fn normalize_openclaw_jsonl_file(
    path: &Path,
    context: &ProviderAdapterContext,
    indexes: &BTreeMap<String, Value>,
) -> Result<ProviderNormalizationResult> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut result = ProviderNormalizationResult::default();
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("openclaw-session")
        .to_owned();
    let agent_id = openclaw_agent_id(path);
    let mut provider_session_id = agent_id
        .as_ref()
        .map(|agent| format!("{agent}/{fallback_id}"))
        .unwrap_or_else(|| fallback_id.clone());
    let mut started_at = context.imported_at;
    let mut cwd = None;
    let mut header_raw = Value::Null;
    let mut header_seen = false;
    let mut line_number = 0usize;
    let mut line = Vec::new();
    while read_provider_jsonl_line(&mut reader, &mut line)? {
        line_number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(err) => {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: err.to_string(),
                });
                continue;
            }
        };
        let row_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        if row_type == "session" {
            if let Some(id) = value.get("id").and_then(Value::as_str) {
                provider_session_id = agent_id
                    .as_ref()
                    .map(|agent| format!("{agent}/{id}"))
                    .unwrap_or_else(|| id.to_owned());
            }
            started_at = provider_timestamp_value(value.get("timestamp"), context.imported_at);
            cwd = value.get("cwd").and_then(Value::as_str).map(str::to_owned);
            header_raw = value.clone();
            header_seen = true;
            result.captures.push((
                line_number,
                openclaw_capture(
                    OpenClawCaptureDraft {
                        provider_session_id: &provider_session_id,
                        agent_id: agent_id.as_deref(),
                        started_at,
                        ended_at: None,
                        cwd: cwd.clone(),
                        path,
                        indexes,
                        header_raw: header_raw.clone(),
                        event: None,
                    },
                    context,
                ),
            ));
            continue;
        }

        let occurred_at = provider_timestamp_value(value.get("timestamp"), started_at);
        let event_index = (line_number - 1) as u64;
        let event = openclaw_event(
            &provider_session_id,
            event_index,
            line_number,
            &value,
            occurred_at,
        );
        if !header_seen {
            header_seen = true;
            result.captures.push((
                line_number,
                openclaw_capture(
                    OpenClawCaptureDraft {
                        provider_session_id: &provider_session_id,
                        agent_id: agent_id.as_deref(),
                        started_at,
                        ended_at: None,
                        cwd: cwd.clone(),
                        path,
                        indexes,
                        header_raw: header_raw.clone(),
                        event: None,
                    },
                    context,
                ),
            ));
        }
        result.captures.push((
            line_number,
            openclaw_capture(
                OpenClawCaptureDraft {
                    provider_session_id: &provider_session_id,
                    agent_id: agent_id.as_deref(),
                    started_at,
                    ended_at: None,
                    cwd: cwd.clone(),
                    path,
                    indexes,
                    header_raw: header_raw.clone(),
                    event: Some(event),
                },
                context,
            ),
        ));
    }
    Ok(result)
}

pub(crate) struct OpenClawCaptureDraft<'a> {
    pub(crate) provider_session_id: &'a str,
    pub(crate) agent_id: Option<&'a str>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) path: &'a Path,
    pub(crate) indexes: &'a BTreeMap<String, Value>,
    pub(crate) header_raw: Value,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn openclaw_capture(
    draft: OpenClawCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let OpenClawCaptureDraft {
        provider_session_id,
        agent_id,
        started_at,
        ended_at,
        cwd,
        path,
        indexes,
        header_raw,
        event,
    } = draft;
    let local_id = provider_session_id
        .rsplit_once('/')
        .map(|(_, id)| id)
        .unwrap_or(provider_session_id);
    let index = indexes
        .get(provider_session_id)
        .or_else(|| indexes.get(local_id))
        .cloned()
        .unwrap_or(Value::Null);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::OpenClaw,
            source_format: OPENCLAW_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: index
                .get("parentSessionId")
                .or_else(|| index.get("parent_session_id"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            root_provider_session_id: None,
            external_agent_id: agent_id.map(str::to_owned),
            agent_type: AgentType::Primary,
            role_hint: Some("personal-agent".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd,
            fidelity: Fidelity::Partial,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": OPENCLAW_SOURCE_FORMAT,
                "index": provider_capped_json(&index, PROVIDER_MAX_PREVIEW_CHARS),
                "header": provider_capped_json(&header_raw, PROVIDER_MAX_PREVIEW_CHARS),
                "support_level": "beta",
            }),
            session_metadata: json!({
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "agent_id": agent_id,
                "session_index": provider_capped_json(&index, PROVIDER_MAX_PREVIEW_CHARS),
                "fidelity_gap": "OpenClaw session JSONL is current native storage, but upstream keeps a storage-neutral accessor for future schema changes",
            }),
        },
        context,
        event,
    )
}

pub(crate) fn openclaw_event(
    provider_session_id: &str,
    event_index: u64,
    line_number: usize,
    row: &Value,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let row_type = row.get("type").and_then(Value::as_str).unwrap_or("message");
    let message = row.get("message").unwrap_or(row);
    let role = message
        .get("role")
        .or_else(|| row.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    let event_type = match row_type {
        "message" => match role {
            Some(EventRole::Tool) => EventType::ToolOutput,
            _ => EventType::Message,
        },
        "leaf" | "compaction" | "custom" => EventType::Notice,
        _ => EventType::Notice,
    };
    let text = message
        .get("content")
        .or_else(|| message.get("text"))
        .or_else(|| message.get("output"))
        .and_then(provider_value_text)
        .unwrap_or_else(|| format!("OpenClaw {row_type}"));
    native_event(NativeEventDraft {
        provider: CaptureProvider::OpenClaw,
        source_format: OPENCLAW_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: event_index,
        provider_event_hash: row.get("id").and_then(Value::as_str).map(str::to_owned),
        cursor: format!("line:{line_number}"),
        event_type,
        role,
        occurred_at,
        text,
        body: row.clone(),
        metadata: json!({
            "source": "openclaw_jsonl",
            "source_format": OPENCLAW_SOURCE_FORMAT,
            "row_type": row_type,
            "message_id": row.get("id").and_then(Value::as_str),
            "parent_id": row.get("parentId").or_else(|| row.get("parent_id")).cloned(),
        }),
    })
}

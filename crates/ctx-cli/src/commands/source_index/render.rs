use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::{anyhow, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use ctx_history_index::{EventSearchFilters, VerifiedIndex};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    complete_content::{
        enforce_complete_content_cli_output_limit, enforce_complete_content_output_limit,
        ContentPolicy, CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
    },
    output::{compact_json, OutputFormat},
    transcript::{shell_quote_arg, write_output},
};

use super::search::{SearchCollection, SearchHit, SourceSearchRequest};

pub(super) fn pretty_json_stdout_bytes(value: &Value) -> Result<usize> {
    Ok(serde_json::to_string_pretty(value)?.len().saturating_add(1))
}

pub(super) fn stdout_body_bytes(body: &str) -> usize {
    body.len()
        .saturating_add(usize::from(!body.ends_with('\n')))
}

pub(super) fn locate_session_text_output_bytes(value: &Value) -> usize {
    let mut bytes = format!(
        "ctx_session_id: {}\n",
        value["ctx_session_id"].as_str().unwrap_or("")
    )
    .len();
    bytes = bytes
        .saturating_add(optional_json_text_line_bytes(value, "provider"))
        .saturating_add(optional_json_text_line_bytes(value, "provider_session_id"));
    if let Some(source) = value.get("source") {
        bytes = bytes
            .saturating_add(optional_json_text_line_bytes(source, "path"))
            .saturating_add(optional_json_text_line_bytes(source, "source_format"));
        if let Some(exists) = source.get("exists").and_then(Value::as_bool) {
            bytes = bytes.saturating_add(format!("source_exists: {exists}\n").len());
        }
    }
    if let Some(command) = value
        .get("resume")
        .and_then(|resume| resume.get("command"))
        .and_then(Value::as_str)
    {
        bytes = bytes.saturating_add(format!("resume_command: {command}\n").len());
    }
    bytes
}

pub(super) fn locate_event_text_output_bytes(value: &Value) -> usize {
    let mut bytes = format!(
        "ctx_event_id: {}\n",
        value["ctx_event_id"].as_str().unwrap_or("")
    )
    .len();
    for key in [
        "ctx_session_id",
        "provider",
        "provider_session_id",
        "event_type",
        "role",
        "cursor",
    ] {
        bytes = bytes.saturating_add(optional_json_text_line_bytes(value, key));
    }
    if let Some(source) = value.get("source") {
        bytes = bytes.saturating_add(optional_json_text_line_bytes(source, "path"));
    }
    if let Some(source_record) = value.get("source_record") {
        if let Some(ordinal) = source_record.get("ordinal").and_then(Value::as_u64) {
            bytes = bytes.saturating_add(format!("source_record_ordinal: {ordinal}\n").len());
        }
        if let Some(index) = source_record.get("subrecord_index").and_then(Value::as_u64) {
            bytes = bytes.saturating_add(format!("source_record_subrecord_index: {index}\n").len());
        }
    }
    bytes
}

fn optional_json_text_line_bytes(value: &Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|text| format!("{key}: {text}\n").len())
        .unwrap_or_default()
}

pub(super) fn search_json(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
    collection: &SearchCollection,
    filters: &EventSearchFilters,
    snippets: &HashMap<Uuid, String>,
    refresh_status: &str,
    refresh_source_count: usize,
    query_duration: Duration,
) -> Result<Value> {
    let result_scope = if request.events { "event" } else { "session" };
    let results = collection
        .result_window
        .hits
        .iter()
        .map(|hit| {
            let snippet = snippets
                .get(&hit.event.event_id.as_uuid())
                .filter(|snippet| !snippet.is_empty())
                .ok_or_else(|| {
                    anyhow!(
                        "generation-bound source hydration omitted search event {}",
                        hit.event.event_id
                    )
                })?;
            Ok(search_result_json(
                hit,
                snippet,
                result_scope,
                &request.query,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let phase_attribution = phase_attribution(query_duration);
    Ok(compact_json(json!({
        "schema_version": 1,
        "payload_type": "search_results",
        "query": request.query.trim(),
        "filters": {
            "provider": filters.provider,
            "history_source": filters.history_source,
            "provider_key": filters.provider_key,
            "source_id": filters.source_id,
            "source_format": filters.source_format,
            "workspace": request.workspace,
            "since": request.since,
            "event_type": request.event_type,
            "file": request.file.as_ref().map(|path| path.display().to_string()),
            "session": request.session,
            "primary_only": request.primary_only.then_some(true),
            "include_subagents": request.include_subagents.then_some(true),
            "include_current_session": request.include_current_session.then_some(true),
        },
        "freshness": {
            "mode": request.refresh.as_str(),
            "status": refresh_status,
            "source_count": refresh_source_count,
        },
        "retrieval": {
            "requested_mode": collection.requested_backend.as_str(),
            "effective_mode": collection.effective_backend.as_str(),
            "semantic_weight": collection.semantic_weight,
            "semantic_status": collection.semantic_status,
            "semantic_fallback_code": collection.semantic_fallback.as_ref().map(|fallback| fallback.code),
            "semantic_fallback": collection.semantic_fallback.as_ref().map(|fallback| fallback.detail.as_str()),
            "semantic_diagnostics": collection.semantic_diagnostics,
            "index": "source_backed",
            "generation_id": index.generation_id(),
            "indexed_documents": index.document_count(),
            "phase_attribution": phase_attribution,
        },
        "phase_attribution": phase_attribution,
        "results": results,
        "result_window": {
            "limit": collection.result_window.limit,
            "returned": results.len(),
            "more_available": collection.result_window.more_available,
        },
        "truncation": {
            "candidate_pool": collection.candidate_pool,
            "candidate_pool_truncated": collection.candidate_pool_truncated,
        },
    })))
}

fn search_result_json(hit: &SearchHit, snippet: &str, result_scope: &str, query: &str) -> Value {
    let event = &hit.event;
    let event_id = event.event_id.as_uuid();
    let session_id = event.session_id.as_uuid();
    let item_id = if result_scope == "session" {
        session_id
    } else {
        event_id
    };
    let title = match event.role.as_deref() {
        Some(role) => format!("{} {role} {}", event.provider, event.event_type),
        None => format!("{} {}", event.provider, event.event_type),
    };
    let mut next = vec![format!("ctx show event {event_id} --window 10")];
    if result_scope == "session" {
        next.insert(0, format!("ctx show session {session_id}"));
    }
    next.push(format!(
        "ctx search {} --session {session_id}",
        shell_quote_arg(query)
    ));
    compact_json(json!({
        "item_id": item_id,
        "result_type": if result_scope == "session" { "session_result" } else { "event" },
        "ctx_event_id": event_id,
        "ctx_session_id": session_id,
        "session_id": session_id,
        "event_id": event_id,
        "event_seq": event.event_sequence,
        "title": title,
        "snippet": snippet,
        "rank": hit.score,
        "result_scope": result_scope,
        "session_importance": (result_scope == "session").then_some(hit.score),
        "more_matches_in_session": (result_scope == "session")
            .then_some(hit.more_matches_in_session),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "source_format": event.source_format,
        "source_path": event.source_path,
        "parent_ctx_session_id": event.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": event.root_session_id.as_uuid(),
        "branch": event.branch,
        "agent_type": event.agent_type,
        "is_primary": event.is_primary,
        "timestamp": timestamp_json(event.occurred_at_unix_ms),
        "workspace": event.workspace,
        "cwd": event.cwd,
        "suggested_next_commands": next,
        "citations": [{
            "item_id": event_id,
            "target_type": "event",
            "ctx_event_id": event_id,
            "ctx_session_id": session_id,
            "provider": event.provider,
            "session_id": session_id,
            "event_seq": event.event_sequence,
            "source_path": event.source_path,
        }],
        "visibility": "local",
    }))
}

pub(super) fn render_search_text(value: &Value, verbose: bool) -> String {
    let mut output = String::new();
    let results = value["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if results.is_empty() {
        let query = value["query"].as_str().unwrap_or_default();
        output.push_str(&format!("no results for {}\n", shell_quote_arg(query)));
        return output;
    }
    for (position, result) in results.iter().enumerate() {
        let title = result["title"].as_str().unwrap_or("indexed event");
        if verbose {
            output.push_str(title);
            output.push('\n');
            for key in [
                "ctx_event_id",
                "ctx_session_id",
                "provider_session_id",
                "source_format",
            ] {
                if let Some(value) = result[key].as_str() {
                    output.push_str(&format!("  {key}: {value}\n"));
                }
            }
            if let Some(snippet) = result["snippet"].as_str() {
                output.push_str(&format!("  {snippet}\n"));
            }
            if let Some(rank) = result["rank"].as_f64() {
                output.push_str(&format!("  rank: {rank:.2}\n"));
            }
            if let Some(importance) = result["session_importance"].as_f64() {
                output.push_str(&format!("  session_importance: {importance:.2}\n"));
            }
            if let Some(commands) = result["suggested_next_commands"].as_array() {
                for command in commands.iter().filter_map(Value::as_str).take(3) {
                    output.push_str(&format!("  next: {command}\n"));
                }
            }
            if let Some(event_id) = result["ctx_event_id"].as_str() {
                output.push_str(&format!("  citation: event {event_id}\n"));
            }
        } else {
            output.push_str(&format!("{}. {title}\n", position + 1));
            let provider = result["provider"].as_str().unwrap_or("unknown");
            let scope = result["result_scope"].as_str().unwrap_or("event");
            let score = result["session_importance"]
                .as_f64()
                .or_else(|| result["rank"].as_f64())
                .unwrap_or_default();
            output.push_str(&format!("   {provider} | {scope} {score:.2}\n"));
            if let Some(snippet) = result["snippet"].as_str() {
                output.push_str(&format!("   {snippet}\n"));
            }
            if let Some(command) = result["suggested_next_commands"]
                .as_array()
                .and_then(|commands| commands.first())
                .and_then(Value::as_str)
            {
                output.push_str(&format!("   inspect: {command}\n"));
            }
        }
    }
    if value["truncation"]["candidate_pool_truncated"] == true {
        output.push_str(
            "warning: source-backed session diversity was bounded by the current index query API\n",
        );
    }
    if value["result_window"]["more_available"] == true {
        output.push_str("More results available.\n");
    }
    output
}

pub(super) fn write_show_value(
    value: Value,
    format: OutputFormat,
    out: Option<PathBuf>,
    event_id: Uuid,
) -> Result<usize> {
    let body = match format {
        OutputFormat::Json => serde_json::to_string_pretty(&value)?,
        OutputFormat::Jsonl => render_show_jsonl(&value)?,
        OutputFormat::Text => render_show_text(&value),
        OutputFormat::Markdown => render_show_markdown(&value),
    };
    enforce_complete_content_cli_output_limit(
        ContentPolicy::Complete,
        &body,
        out.is_none(),
        CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
        event_id,
    )?;
    let output_bytes = if out.is_some() {
        body.len()
    } else {
        stdout_body_bytes(&body)
    };
    write_output(body, out).map(|()| output_bytes)
}

fn render_show_jsonl(value: &Value) -> Result<String> {
    let events = value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let lines = events
        .iter()
        .map(|event| {
            if value["target"] == "session" {
                serde_json::to_string(&compact_json(json!({
                    "schema_version": 1,
                    "payload_type": "session_transcript_event",
                    "mode": value["mode"],
                    "content_policy": value["content_policy"],
                    "ctx_session_id": value["ctx_session_id"],
                    "provider": value["provider"],
                    "provider_session_id": value["provider_session_id"],
                    "event": event,
                })))
            } else {
                serde_json::to_string(event)
            }
        })
        .collect::<serde_json::Result<Vec<_>>>()?;
    if lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(lines.join("\n") + "\n")
    }
}

pub(super) fn enforce_json_output_limit(
    value: &Value,
    output_limit_bytes: usize,
    event_id: Uuid,
) -> Result<()> {
    let serialized_bytes = serde_json::to_vec(value)?.len();
    enforce_complete_content_output_limit(
        ContentPolicy::Complete,
        serialized_bytes,
        output_limit_bytes,
        event_id,
    )?;
    Ok(())
}

fn render_show_text(value: &Value) -> String {
    let mut output = String::new();
    match value["target"].as_str() {
        Some("session") => {
            output.push_str(&format!(
                "ctx_session_id: {}\nprovider: {}\n",
                value["ctx_session_id"].as_str().unwrap_or("unknown"),
                value["provider"].as_str().unwrap_or("unknown")
            ));
            if let Some(provider_session_id) = value["provider_session_id"].as_str() {
                output.push_str(&format!("provider_session_id: {provider_session_id}\n"));
            }
            output.push_str(&format!(
                "mode: {}\ncontent: {}\nformat: text\n\n",
                value["mode"].as_str().unwrap_or("lite"),
                value["content_policy"].as_str().unwrap_or("indexed")
            ));
        }
        _ => {
            output.push_str(&format!(
                "ctx_event_id: {}\nctx_session_id: {}\ncontent: {}\n\n",
                value["ctx_event_id"].as_str().unwrap_or("unknown"),
                value["ctx_session_id"].as_str().unwrap_or("unknown"),
                value["content_policy"].as_str().unwrap_or("indexed")
            ));
        }
    }
    for event in value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let role = event["role"]
            .as_str()
            .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
        output.push_str(&format!(
            "[{}] {} {} {}\n{}\n\n",
            event["occurred_at"].as_str().unwrap_or("-"),
            role,
            event["event_type"].as_str().unwrap_or("event"),
            event["ctx_event_id"].as_str().unwrap_or("unknown"),
            event["text"].as_str().unwrap_or_default()
        ));
    }
    output
}

fn render_show_markdown(value: &Value) -> String {
    let mut output = match value["target"].as_str() {
        Some("session") => format!(
            "# {} session {}\n\n- ctx_session_id: `{}`\n- content: `{}`\n",
            value["provider"].as_str().unwrap_or("unknown"),
            value["provider_session_id"]
                .as_str()
                .or_else(|| value["ctx_session_id"].as_str())
                .unwrap_or("unknown"),
            value["ctx_session_id"].as_str().unwrap_or("unknown"),
            value["content_policy"].as_str().unwrap_or("indexed")
        ),
        _ => format!(
            "# Event {}\n\n- ctx_session_id: `{}`\n- content: `{}`\n",
            value["ctx_event_id"].as_str().unwrap_or("unknown"),
            value["ctx_session_id"].as_str().unwrap_or("unknown"),
            value["content_policy"].as_str().unwrap_or("indexed")
        ),
    };
    for event in value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let role = event["role"]
            .as_str()
            .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
        output.push_str(&format!(
            "\n## {} - {} - {}\n\nctx_event_id: `{}`\n\n{}\n",
            role,
            event["event_type"].as_str().unwrap_or("event"),
            event["occurred_at"].as_str().unwrap_or("-"),
            event["ctx_event_id"].as_str().unwrap_or("unknown"),
            event["text"].as_str().unwrap_or_default()
        ));
    }
    output
}

pub(super) fn timestamp_json(timestamp: Option<i64>) -> Option<String> {
    timestamp
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub(super) fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn phase_attribution(query: Duration) -> Value {
    json!({
        "discovery_seconds": 0.0,
        "writer_open_seconds": 0.0,
        "scan_and_stage_seconds": 0.0,
        "scanner_worker_busy_seconds": 0.0,
        "writer_add_document_seconds": 0.0,
        "certification_seconds": 0.0,
        "index_commit_seconds": 0.0,
        "refresh_total_seconds": 0.0,
        "query_seconds": query.as_secs_f64(),
        "catalog_sources": 0,
        "catalog_source_bytes": 0,
        "cold_sources": 0,
        "appended_sources": 0,
        "replaced_sources": 0,
        "replayed_sources": 0,
        "deleted_sources": 0,
        "scanner_bytes_read": 0,
        "checkpoint_validation_bytes": 0,
        "scanner_workers": 0,
        "complete_records_scanned": 0,
        "retained_records_scanned": 0,
        "rejected_records_scanned": 0,
        "ignored_records_scanned": 0,
        "staged_documents": 0,
    })
}

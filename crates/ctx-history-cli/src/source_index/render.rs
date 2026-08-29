use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use chrono::SecondsFormat;
use ctx_history_core::utc_now;
use ctx_history_index::{EventSearchFilters, VerifiedIndex};
use ctx_history_platform::managed_data_root;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    output::{compact_json, OutputFormat},
    presentation_limit::{
        enforce_presentation_cli_output_limit, enforce_presentation_output_limit,
        CLI_PRESENTATION_MAX_OUTPUT_BYTES,
    },
    transcript::{shell_quote_arg, write_output},
};

use super::search::{
    semantic_reason_code, NormalizedSearchQuery, SearchCollection, SearchPresentation,
    SourceSearchRequest,
};
use crate::RefreshMode as RefreshArg;

mod activity;
mod human;
mod locate;
mod search;
mod show;

pub(super) use activity::{markdown_code_span, safe_activity_json};
pub(super) use locate::render_locate_document;
pub(super) use search::{render_search_document, render_search_not_ready_document};
pub(super) use show::render_show_document;

#[cfg(test)]
pub(in crate::source_index) use ctx_history_read_application::{
    search_snippet_fragment, SEARCH_SNIPPET_MAX_BYTES, SEARCH_SNIPPET_MAX_CHARS,
};

pub(super) fn pretty_json_stdout_bytes(value: &Value) -> Result<usize> {
    Ok(serde_json::to_string_pretty(value)?.len().saturating_add(1))
}

pub(super) fn stdout_body_bytes(body: &str) -> usize {
    body.len()
        .saturating_add(usize::from(!body.ends_with('\n')))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn search_json(
    request: &SourceSearchRequest,
    data_root: &Path,
    index: &VerifiedIndex,
    collection: &SearchCollection,
    filters: &EventSearchFilters,
    presentations: &[SearchPresentation],
    refresh_status: &str,
    refresh_source_count: usize,
    query_duration: Duration,
) -> Result<Value> {
    search_json_document(
        request,
        data_root,
        index,
        collection,
        filters,
        presentations,
        RefreshArg::Off,
        refresh_status,
        refresh_source_count,
        query_duration,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn search_json_document(
    request: &SourceSearchRequest,
    data_root: &Path,
    index: &VerifiedIndex,
    collection: &SearchCollection,
    filters: &EventSearchFilters,
    presentations: &[SearchPresentation],
    refresh_mode: RefreshArg,
    refresh_status: &str,
    refresh_source_count: usize,
    query_duration: Duration,
) -> Result<Value> {
    let commands = search_result_commands(request, collection, data_root);
    let fallback_code = collection.semantic_fallback.as_ref().and_then(|fallback| {
        fallback
            .code
            .or_else(|| fallback.reason.map(semantic_reason_code))
    });
    let fallback_detail = collection
        .semantic_fallback
        .as_ref()
        .map(|fallback| semantic_fallback_detail(fallback.reason, &fallback.detail));
    let generated_at = utc_now().to_rfc3339_opts(SecondsFormat::Millis, true);
    ctx_history_read_application::render_search_json(
        ctx_history_read_application::SearchJsonInput {
            request,
            index,
            collection,
            filters,
            presentations,
            commands: &commands,
            freshness_mode: refresh_mode.as_str(),
            generated_at: &generated_at,
            semantic_fallback_code: fallback_code,
            semantic_fallback_detail: fallback_detail.as_deref(),
            metrics: ctx_history_read_application::SearchRenderMetrics {
                refresh_status,
                refresh_source_count,
                query_duration,
            },
        },
    )
}

fn search_result_commands(
    request: &SourceSearchRequest,
    collection: &SearchCollection,
    data_root: &Path,
) -> Vec<ctx_history_read_application::SearchResultCommands> {
    let normalized_query = NormalizedSearchQuery::from_request(request);
    let command_prefix = follow_up_command_prefix(data_root);
    let query_arguments = search_query_command_arguments(&normalized_query);
    let result_scope = if request.events || request.session.is_some() {
        "event"
    } else {
        "session"
    };
    collection
        .result_window
        .hits
        .iter()
        .map(|hit| {
            let event_id = hit.event.event_id;
            let session_id = hit.event.session_id;
            let mut suggested_next_commands = vec![format!(
                "{command_prefix} show event {event_id} --window 10"
            )];
            if result_scope == "session" {
                suggested_next_commands
                    .insert(0, format!("{command_prefix} show session {session_id}"));
            }
            if !query_arguments.is_empty() {
                suggested_next_commands.push(format!(
                    "{command_prefix} search {query_arguments} --session {session_id}"
                ));
            }
            ctx_history_read_application::SearchResultCommands {
                suggested_next_commands,
            }
        })
        .collect()
}

fn semantic_fallback_detail(
    reason: Option<ctx_history_read_application::SemanticReason>,
    detail: &str,
) -> String {
    match reason {
        Some(ctx_history_read_application::SemanticReason::PolicyDisabled) => {
            "local semantic retrieval is disabled".to_owned()
        }
        Some(ctx_history_read_application::SemanticReason::ExecutionUnavailable) => {
            "local semantic retrieval is unavailable because the ctx daemon is disabled".to_owned()
        }
        Some(ctx_history_read_application::SemanticReason::ContentScopeUnsupported) => {
            format!("{detail}; use --backend lexical or choose --content-scope all|transcript")
        }
        Some(ctx_history_read_application::SemanticReason::EventTypeUnsupported) => {
            format!("{detail}; use --backend lexical or remove --event-type")
        }
        _ => detail.to_owned(),
    }
}

fn search_query_command_arguments(query: &NormalizedSearchQuery) -> String {
    let mut arguments = Vec::new();
    if let Some(positional) = query.positional() {
        arguments.push(shell_quote_arg(positional));
    }
    for term in query.terms() {
        arguments.push(format!("--term={}", shell_quote_arg(term)));
    }
    arguments.join(" ")
}

pub(super) fn follow_up_command_prefix(data_root: &Path) -> String {
    if managed_data_root().is_ok_and(|default_root| default_root == data_root) {
        return "ctx".to_owned();
    }
    let data_root = data_root.to_string_lossy();
    format!("ctx --data-root {}", shell_quote_arg(data_root.as_ref()))
}

pub(super) fn write_show_value(
    value: Value,
    format: OutputFormat,
    out: Option<PathBuf>,
    event_id: Uuid,
    stdout: &mut dyn Write,
) -> Result<usize> {
    let body = match format {
        OutputFormat::Json => serde_json::to_string_pretty(&value)?,
        OutputFormat::Jsonl => render_show_jsonl(&value)?,
        OutputFormat::Text => render_show_text(&value),
        OutputFormat::Markdown => render_show_markdown(&value),
    };
    enforce_presentation_cli_output_limit(
        &body,
        out.is_none(),
        CLI_PRESENTATION_MAX_OUTPUT_BYTES,
        event_id,
    )?;
    let output_bytes = if out.is_some() {
        body.len()
    } else {
        stdout_body_bytes(&body)
    };
    write_output(body, out, stdout).map(|()| output_bytes)
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
    enforce_presentation_output_limit(serialized_bytes, output_limit_bytes, event_id)?;
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
                "mode: {}\nformat: text\n\n",
                value["mode"].as_str().unwrap_or("lite")
            ));
        }
        _ => {
            output.push_str(&format!(
                "ctx_event_id: {}\nctx_session_id: {}\n\n",
                value["ctx_event_id"].as_str().unwrap_or("unknown"),
                value["ctx_session_id"].as_str().unwrap_or("unknown")
            ));
        }
    }
    append_copied_lineage_text(&mut output, value);
    for event in value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let role = event["role"]
            .as_str()
            .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
        output.push_str(&format!(
            "[{}] {} {} {}\n",
            event["occurred_at"].as_str().unwrap_or("-"),
            role,
            event["event_type"].as_str().unwrap_or("event"),
            event["ctx_event_id"].as_str().unwrap_or("unknown"),
        ));
        append_activity_text(&mut output, event);
        output.push_str(event["text"].as_str().unwrap_or_default());
        output.push_str("\n\n");
    }
    output
}

fn render_show_markdown(value: &Value) -> String {
    let mut output = match value["target"].as_str() {
        Some("session") => format!(
            "# {} session {}\n\n- ctx_session_id: `{}`\n",
            value["provider"].as_str().unwrap_or("unknown"),
            value["provider_session_id"]
                .as_str()
                .or_else(|| value["ctx_session_id"].as_str())
                .unwrap_or("unknown"),
            value["ctx_session_id"].as_str().unwrap_or("unknown")
        ),
        _ => format!(
            "# Event {}\n\n- ctx_session_id: `{}`\n",
            value["ctx_event_id"].as_str().unwrap_or("unknown"),
            value["ctx_session_id"].as_str().unwrap_or("unknown")
        ),
    };
    append_copied_lineage_markdown(&mut output, value);
    for event in value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let role = event["role"]
            .as_str()
            .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
        output.push_str(&format!(
            "\n## {} - {} - {}\n\nctx_event_id: `{}`\n\n",
            role,
            event["event_type"].as_str().unwrap_or("event"),
            event["occurred_at"].as_str().unwrap_or("-"),
            event["ctx_event_id"].as_str().unwrap_or("unknown"),
        ));
        append_activity_markdown(&mut output, event);
        output.push_str(event["text"].as_str().unwrap_or_default());
        output.push('\n');
    }
    output
}

fn append_activity_text(output: &mut String, event: &Value) {
    if let Some(activity) = event.get("activity").filter(|value| !value.is_null()) {
        output.push_str("activity: ");
        output.push_str(&safe_activity_json(activity));
        output.push('\n');
    }
}

fn append_activity_markdown(output: &mut String, event: &Value) {
    if let Some(activity) = event.get("activity").filter(|value| !value.is_null()) {
        output.push_str("activity: ");
        output.push_str(&markdown_code_span(&safe_activity_json(activity)));
        output.push_str("\n\n");
    }
}

fn append_copied_lineage_text(output: &mut String, value: &Value) {
    let Some((lineage, observed, resolution, selected_depth)) =
        super::copied_lineage::copied_lineage_summary(value)
    else {
        return;
    };
    if observed == 0 && resolution.is_none_or(|state| state == "resolved") && selected_depth == 0 {
        return;
    }
    if let Some(resolution) = resolution {
        output.push_str(&format!(
            "lineage_resolution: {resolution} selected_depth={selected_depth}\n"
        ));
    }
    let truncated = lineage["truncated"].as_bool().unwrap_or(true);
    let summary = if truncated {
        format!("copied_to: at least {observed} sessions\n")
    } else {
        format!("copied_to: {observed} sessions\n")
    };
    output.push_str(&summary);
    let command_prefix = value["_command_prefix"].as_str().unwrap_or("ctx");
    for occurrence in lineage["occurrences"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .take(20)
    {
        let session = occurrence["ctx_session_id"].as_str().unwrap_or("unknown");
        let event = occurrence["ctx_event_id"].as_str().unwrap_or("unknown");
        let relationship = occurrence["session_relationship"]
            .as_str()
            .unwrap_or("unspecified");
        let depth = occurrence["depth"].as_u64().unwrap_or(0);
        output.push_str(&format!(
            "copied_by: session={session} event={event} relationship={relationship} depth={depth}\n"
        ));
        output.push_str(&format!("next: {command_prefix} show session {session}\n"));
    }
    output.push('\n');
}

fn append_copied_lineage_markdown(output: &mut String, value: &Value) {
    let Some((lineage, observed, resolution, selected_depth)) =
        super::copied_lineage::copied_lineage_summary(value)
    else {
        return;
    };
    if observed == 0 && resolution.is_none_or(|state| state == "resolved") && selected_depth == 0 {
        return;
    }
    if let Some(resolution) = resolution {
        output.push_str(&format!(
            "\n## Copied lineage\n\nResolution: `{resolution}` at selected depth {selected_depth}.\n"
        ));
    }
    if observed == 0 {
        return;
    }
    let truncated = lineage["truncated"].as_bool().unwrap_or(true);
    let count = if truncated {
        format!("at least {observed}")
    } else {
        observed.to_string()
    };
    output.push_str(&format!("\n### Copied by {count} sessions\n"));
    let command_prefix = value["_command_prefix"].as_str().unwrap_or("ctx");
    for occurrence in lineage["occurrences"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .take(20)
    {
        let session = occurrence["ctx_session_id"].as_str().unwrap_or("unknown");
        let event = occurrence["ctx_event_id"].as_str().unwrap_or("unknown");
        let relationship = occurrence["session_relationship"]
            .as_str()
            .unwrap_or("unspecified");
        let depth = occurrence["depth"].as_u64().unwrap_or(0);
        output.push_str(&format!(
            "\n- `{relationship}` session `{session}`, event `{event}`, depth {depth}\n"
        ));
        output.push_str(&format!("  - `{command_prefix} show session {session}`\n"));
    }
}

#[cfg(test)]
mod tests;

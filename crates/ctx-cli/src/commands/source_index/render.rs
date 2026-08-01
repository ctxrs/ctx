use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use ctx_history_core::{managed_data_root, utc_now};
use ctx_history_index::{EventSearchFilters, VerifiedIndex};
use serde_json::{json, Value};
use unicode_segmentation::UnicodeSegmentation as _;
use uuid::Uuid;

use crate::{
    output::{compact_json, OutputFormat},
    presentation_limit::{
        enforce_presentation_cli_output_limit, enforce_presentation_output_limit,
        CLI_PRESENTATION_MAX_OUTPUT_BYTES,
    },
    transcript::{shell_quote_arg, write_output},
};

use super::search::{NormalizedSearchQuery, SearchCollection, SearchHit, SourceSearchRequest};

mod human;
mod search;
mod show;

pub(super) use search::{render_search_document, render_search_not_ready_document};
pub(super) use show::render_show_document;

pub(in crate::commands::source_index) const SEARCH_SNIPPET_MAX_CHARS: usize = 320;

#[derive(Debug)]
pub(in crate::commands::source_index) struct SearchCorePresentation {
    pub(in crate::commands::source_index) record: ctx_history_index::CoreEventRecord,
    pub(in crate::commands::source_index) snippet_truncated: bool,
}

pub(super) fn pretty_json_stdout_bytes(value: &Value) -> Result<usize> {
    Ok(serde_json::to_string_pretty(value)?.len().saturating_add(1))
}

pub(super) fn stdout_body_bytes(body: &str) -> usize {
    body.len()
        .saturating_add(usize::from(!body.ends_with('\n')))
}

struct SearchJsonInput<'a> {
    request: &'a SourceSearchRequest,
    data_root: &'a Path,
    index: &'a VerifiedIndex,
    collection: &'a SearchCollection,
    filters: &'a EventSearchFilters,
    core_records: &'a HashMap<Uuid, SearchCorePresentation>,
    metrics: SearchRenderMetrics<'a>,
}

struct SearchRenderMetrics<'a> {
    refresh_status: &'a str,
    refresh_source_count: usize,
    query_duration: Duration,
}

// Keep the orchestration call shape stable while rendering consumes one typed input.
type SearchJsonCompatibilityFn = fn(
    &SourceSearchRequest,
    &Path,
    &VerifiedIndex,
    &SearchCollection,
    &EventSearchFilters,
    &HashMap<Uuid, SearchCorePresentation>,
    &str,
    usize,
    Duration,
) -> Result<Value>;

pub(super) const SEARCH_JSON: SearchJsonCompatibilityFn =
    |request,
     data_root,
     index,
     collection,
     filters,
     core_records,
     refresh_status,
     refresh_source_count,
     query_duration| {
        render_search_json(SearchJsonInput {
            request,
            data_root,
            index,
            collection,
            filters,
            core_records,
            metrics: SearchRenderMetrics {
                refresh_status,
                refresh_source_count,
                query_duration,
            },
        })
    };
pub(super) use self::SEARCH_JSON as search_json;

fn render_search_json(input: SearchJsonInput<'_>) -> Result<Value> {
    let SearchJsonInput {
        request,
        data_root,
        index,
        collection,
        filters,
        core_records,
        metrics,
    } = input;
    let normalized_query = NormalizedSearchQuery::from_request(request);
    let result_scope = if request.events { "event" } else { "session" };
    let command_prefix = follow_up_command_prefix(data_root);
    let results = collection
        .result_window
        .hits
        .iter()
        .enumerate()
        .map(|(offset, hit)| {
            let core_record = core_records
                .get(&hit.event.event_id.as_uuid())
                .ok_or_else(|| {
                    anyhow!(
                        "pinned Core lookup omitted search event {}",
                        hit.event.event_id
                    )
                })?;
            search_result_json(
                hit,
                core_record,
                result_scope,
                &normalized_query,
                offset.saturating_add(1),
                &command_prefix,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let phase_attribution = phase_attribution(metrics.query_duration);
    Ok(compact_json(json!({
        "schema_version": 1,
        "payload_type": "search_results",
        "query": normalized_query.display(),
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
            "status": metrics.refresh_status,
            "source_count": metrics.refresh_source_count,
        },
        "retrieval": {
            "requested_mode": collection.requested_backend.as_str(),
            "effective_mode": collection.effective_backend.as_str(),
            "semantic_weight": collection.semantic_weight,
            "semantic_status": collection.semantic_status,
            "semantic_fallback_code": collection.semantic_fallback.as_ref().map(|fallback| fallback.code),
            "semantic_fallback": collection.semantic_fallback.as_ref().map(|fallback| fallback.detail.as_str()),
            "semantic_diagnostics": collection.semantic_diagnostics,
            "index": "core",
            "generation_id": index.generation_id(),
            "indexed_documents": index.document_count(),
            "phase_attribution": phase_attribution,
        },
        "phase_attribution": phase_attribution,
        "generated_at": utc_now().to_rfc3339_opts(SecondsFormat::Millis, true),
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

fn search_result_json(
    hit: &SearchHit,
    core_record: &SearchCorePresentation,
    result_scope: &str,
    query: &NormalizedSearchQuery,
    rank: usize,
    command_prefix: &str,
) -> Result<Value> {
    let (snippet, snippet_truncated) = search_snippet(core_record)?;
    let event = &core_record.record.event;
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
    let mut next = vec![format!(
        "{command_prefix} show event {event_id} --window 10"
    )];
    if result_scope == "session" {
        next.insert(0, format!("{command_prefix} show session {session_id}"));
    }
    let query_arguments = query.shell_arguments();
    if !query_arguments.is_empty() {
        next.push(format!(
            "{command_prefix} search {query_arguments} --session {session_id}"
        ));
    }
    Ok(compact_json(json!({
        "item_id": item_id,
        "result_type": if result_scope == "session" { "session_result" } else { "event" },
        "ctx_event_id": event_id,
        "ctx_session_id": session_id,
        "session_id": session_id,
        "event_id": event_id,
        "event_seq": event.event_sequence,
        "title": title,
        "snippet": snippet,
        "snippet_truncated": snippet_truncated,
        "snippet_max_chars": SEARCH_SNIPPET_MAX_CHARS,
        "rank": rank,
        "retrieval_score": hit.score,
        "result_scope": result_scope,
        "session_importance": (result_scope == "session").then_some(hit.score),
        "more_matches_in_session": (result_scope == "session")
            .then_some(hit.more_matches_in_session),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "source_format": event.source_format,
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
        }],
        "visibility": "local",
    })))
}

fn search_snippet(record: &SearchCorePresentation) -> Result<(&str, bool)> {
    let body = record
        .record
        .core_record
        .content
        .normalized_body
        .as_deref()
        .filter(|body| !body.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "Core search event {} has no normalized body",
                record.record.event_id
            )
        })?;
    Ok((body, record.snippet_truncated))
}

pub(in crate::commands::source_index) fn search_snippet_fragment(
    body: &str,
    query_texts: &[&str],
) -> (String, bool) {
    let grapheme_boundaries = body
        .grapheme_indices(true)
        .map(|(offset, _)| offset)
        .chain(std::iter::once(body.len()))
        .collect::<Vec<_>>();
    let grapheme_count = grapheme_boundaries.len().saturating_sub(1);
    if grapheme_count <= SEARCH_SNIPPET_MAX_CHARS {
        return (body.to_owned(), false);
    }

    let start = query_match_range(body, query_texts).map_or(0, |matched| {
        centered_snippet_start(&grapheme_boundaries, matched)
    });
    let end = start.saturating_add(SEARCH_SNIPPET_MAX_CHARS);
    let snippet = body[grapheme_boundaries[start]..grapheme_boundaries[end]].to_owned();
    let truncated = start > 0 || end < grapheme_count;
    (snippet, truncated)
}

fn centered_snippet_start(grapheme_boundaries: &[usize], matched: Range<usize>) -> usize {
    let grapheme_count = grapheme_boundaries.len().saturating_sub(1);
    let latest_start = grapheme_count.saturating_sub(SEARCH_SNIPPET_MAX_CHARS);
    let match_start = grapheme_boundaries
        .partition_point(|offset| *offset <= matched.start)
        .saturating_sub(1)
        .min(grapheme_count.saturating_sub(1));
    let match_end = grapheme_boundaries
        .partition_point(|offset| *offset < matched.end)
        .min(grapheme_count);
    let match_graphemes = match_end.saturating_sub(match_start).max(1);
    let leading_context = SEARCH_SNIPPET_MAX_CHARS
        .saturating_sub(match_graphemes)
        .saturating_div(2);
    match_start
        .saturating_sub(leading_context)
        .min(latest_start)
}

fn query_match_range(body: &str, query_texts: &[&str]) -> Option<Range<usize>> {
    let (folded_body, boundaries) = lowercase_with_original_boundaries(body);
    let mut best_full_match = None;
    for query_text in query_texts {
        let query_text = query_text.trim();
        if query_text.is_empty() {
            continue;
        }
        update_preferred_match(
            &mut best_full_match,
            folded_match_range(&folded_body, &boundaries, query_text),
            query_text.chars().count(),
        );
    }
    if let Some((_, matched)) = best_full_match {
        return Some(matched);
    }

    let mut best_term_match = None;
    for query_text in query_texts {
        let query_text = query_text.trim();
        for term in query_text.split(|character: char| !character.is_alphanumeric()) {
            if term.is_empty() {
                continue;
            }
            update_preferred_match(
                &mut best_term_match,
                folded_match_range(&folded_body, &boundaries, term),
                term.chars().count(),
            );
        }
    }
    best_term_match.map(|(_, matched)| matched)
}

fn update_preferred_match(
    preferred: &mut Option<(usize, Range<usize>)>,
    candidate: Option<Range<usize>>,
    specificity: usize,
) {
    let Some(candidate) = candidate else {
        return;
    };
    if preferred
        .as_ref()
        .is_none_or(|(current_specificity, current)| {
            specificity > *current_specificity
                || (specificity == *current_specificity && candidate.start < current.start)
        })
    {
        *preferred = Some((specificity, candidate));
    }
}

fn lowercase_with_original_boundaries(value: &str) -> (String, Vec<(usize, usize)>) {
    let mut folded = String::with_capacity(value.len());
    let mut boundaries = Vec::with_capacity(value.chars().count().saturating_add(1));
    for (original_offset, character) in value.char_indices() {
        boundaries.push((folded.len(), original_offset));
        folded.extend(character.to_lowercase());
    }
    boundaries.push((folded.len(), value.len()));
    (folded, boundaries)
}

fn folded_match_range(
    folded_body: &str,
    boundaries: &[(usize, usize)],
    query_text: &str,
) -> Option<Range<usize>> {
    let folded_query = query_text.to_lowercase();
    if folded_query.is_empty() {
        return None;
    }
    let folded_start = folded_body.find(&folded_query)?;
    let folded_end = folded_start.saturating_add(folded_query.len());
    let start_boundary = boundaries
        .partition_point(|(folded_offset, _)| *folded_offset <= folded_start)
        .saturating_sub(1);
    let end_boundary = boundaries.partition_point(|(folded_offset, _)| *folded_offset < folded_end);
    Some(boundaries[start_boundary].1..boundaries[end_boundary].1)
}

fn follow_up_command_prefix(data_root: &Path) -> String {
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

#[cfg(test)]
mod tests;

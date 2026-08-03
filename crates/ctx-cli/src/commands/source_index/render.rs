use std::{
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

use super::search::{
    NormalizedSearchQuery, SearchCollection, SearchHit, SearchPresentation, SourceSearchRequest,
};

mod human;
mod search;
mod show;

pub(super) use search::{render_search_document, render_search_not_ready_document};
pub(super) use show::render_show_document;

pub(in crate::commands::source_index) const SEARCH_SNIPPET_MAX_CHARS: usize = 320;
pub(in crate::commands::source_index) const SEARCH_SNIPPET_MAX_BYTES: usize = 16 * 1024;

pub(super) fn pretty_json_stdout_bytes(value: &Value) -> Result<usize> {
    Ok(serde_json::to_string_pretty(value)?.len().saturating_add(1))
}

pub(super) fn stdout_body_bytes(body: &str) -> usize {
    body.len()
        .saturating_add(usize::from(!body.ends_with('\n')))
}

struct SearchJsonInput<'input, 'event> {
    request: &'input SourceSearchRequest,
    data_root: &'input Path,
    index: &'input VerifiedIndex,
    collection: &'input SearchCollection,
    filters: &'input EventSearchFilters,
    presentations: &'input [SearchPresentation<'event>],
    metrics: SearchRenderMetrics<'input>,
}

struct SearchRenderMetrics<'a> {
    refresh_status: &'a str,
    refresh_source_count: usize,
    query_duration: Duration,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn search_json<'event>(
    request: &SourceSearchRequest,
    data_root: &Path,
    index: &VerifiedIndex,
    collection: &SearchCollection,
    filters: &EventSearchFilters,
    presentations: &[SearchPresentation<'event>],
    refresh_status: &str,
    refresh_source_count: usize,
    query_duration: Duration,
) -> Result<Value> {
    render_search_json(SearchJsonInput {
        request,
        data_root,
        index,
        collection,
        filters,
        presentations,
        metrics: SearchRenderMetrics {
            refresh_status,
            refresh_source_count,
            query_duration,
        },
    })
}

fn render_search_json(input: SearchJsonInput<'_, '_>) -> Result<Value> {
    let SearchJsonInput {
        request,
        data_root,
        index,
        collection,
        filters,
        presentations,
        metrics,
    } = input;
    let normalized_query = NormalizedSearchQuery::from_request(request);
    let result_scope = if request.events { "event" } else { "session" };
    let command_prefix = follow_up_command_prefix(data_root);
    if presentations.len() != collection.result_window.hits.len() {
        return Err(anyhow!(
            "pinned Core lookup returned {} search presentations for {} hits",
            presentations.len(),
            collection.result_window.hits.len()
        ));
    }
    let results = collection
        .result_window
        .hits
        .iter()
        .zip(presentations)
        .enumerate()
        .map(|(offset, (hit, presentation))| {
            if presentation.event.event_id != hit.event.event_id {
                return Err(anyhow!(
                    "pinned Core lookup returned an out-of-order search presentation for event {}",
                    hit.event.event_id
                ));
            }
            search_result_json(
                hit,
                presentation,
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
    presentation: &SearchPresentation<'_>,
    result_scope: &str,
    query: &NormalizedSearchQuery,
    rank: usize,
    command_prefix: &str,
) -> Result<Value> {
    let (snippet, snippet_truncated) = search_snippet(presentation);
    let event = &presentation.event;
    let event_id = event.event_id;
    let session_id = event.session_id;
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
        "parent_ctx_session_id": event.parent_session_id,
        "root_ctx_session_id": event.root_session_id,
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

fn search_snippet<'presentation>(
    presentation: &'presentation SearchPresentation<'_>,
) -> (&'presentation str, bool) {
    (
        presentation.snippet.as_str(),
        presentation.snippet_truncated,
    )
}

pub(in crate::commands::source_index) fn search_snippet_fragment(
    body: &str,
    query_texts: &[&str],
) -> (String, bool) {
    let direct_ascii_offsets = ascii_grapheme_offsets_are_bytes(body);
    let grapheme_count = if direct_ascii_offsets {
        body.len()
    } else {
        body.graphemes(true).count()
    };
    if grapheme_count <= SEARCH_SNIPPET_MAX_CHARS {
        return byte_bounded_search_snippet(body, query_texts, false);
    }

    let start = query_match_range(body, query_texts).map_or(0, |matched| {
        if direct_ascii_offsets {
            centered_snippet_start_from_match(grapheme_count, matched.start, matched.end)
        } else {
            centered_snippet_start(body, grapheme_count, matched)
        }
    });
    let end = start.saturating_add(SEARCH_SNIPPET_MAX_CHARS);
    let byte_range = if direct_ascii_offsets {
        start..end
    } else {
        grapheme_byte_range(body, start, end)
    };
    let snippet = &body[byte_range];
    let truncated = start > 0 || end < grapheme_count;
    byte_bounded_search_snippet(snippet, query_texts, truncated)
}

fn byte_bounded_search_snippet(
    snippet: &str,
    query_texts: &[&str],
    truncated: bool,
) -> (String, bool) {
    if snippet.len() <= SEARCH_SNIPPET_MAX_BYTES {
        return (snippet.to_owned(), truncated);
    }

    let graphemes = snippet
        .grapheme_indices(true)
        .map(|(start, grapheme)| start..start.saturating_add(grapheme.len()))
        .collect::<Vec<_>>();
    let matched = query_match_range(snippet, query_texts);
    let match_containing = matched
        .as_ref()
        .and_then(|matched| grapheme_span_covering_match(&graphemes, matched))
        .filter(|required| grapheme_window_bytes(&graphemes, required) <= SEARCH_SNIPPET_MAX_BYTES)
        .and_then(|required| {
            match_containing_grapheme_window(&graphemes, &required, matched.as_ref())
        });
    let window =
        match_containing.or_else(|| fallback_grapheme_window(&graphemes, matched.as_ref()));
    let Some(window) = window else {
        // A valid Core body can contain one extended grapheme cluster larger
        // than the presentation byte cap. Keep the hit and its truncation
        // signal without retaining or splitting that cluster.
        return (String::new(), true);
    };
    (snippet[window].to_owned(), true)
}

fn match_containing_grapheme_window(
    graphemes: &[Range<usize>],
    required: &Range<usize>,
    matched: Option<&Range<usize>>,
) -> Option<Range<usize>> {
    let required_center = matched.map_or_else(
        || {
            graphemes[required.start]
                .start
                .saturating_add(graphemes[required.end - 1].end)
        },
        |matched| matched.start.saturating_add(matched.end),
    );
    // Retain the largest complete-grapheme window containing the required
    // match span, then prefer the window whose byte midpoint is closest to the
    // match midpoint. The final start offset makes ties deterministic.
    let mut best: Option<(Range<usize>, usize, usize)> = None;
    for start in 0..=required.start {
        for end in required.end..=graphemes.len() {
            let bytes = graphemes[end - 1]
                .end
                .saturating_sub(graphemes[start].start);
            if bytes > SEARCH_SNIPPET_MAX_BYTES {
                break;
            }
            let center = graphemes[start]
                .start
                .saturating_add(graphemes[end - 1].end);
            let center_distance = center.abs_diff(required_center);
            let replace = best
                .as_ref()
                .is_none_or(|(best_range, best_bytes, best_distance)| {
                    bytes > *best_bytes
                        || (bytes == *best_bytes && center_distance < *best_distance)
                        || (bytes == *best_bytes
                            && center_distance == *best_distance
                            && graphemes[start].start < best_range.start)
                });
            if replace {
                best = Some((
                    graphemes[start].start..graphemes[end - 1].end,
                    bytes,
                    center_distance,
                ));
            }
        }
    }
    best.map(|(window, _, _)| window)
}

fn fallback_grapheme_window(
    graphemes: &[Range<usize>],
    matched: Option<&Range<usize>>,
) -> Option<Range<usize>> {
    let match_center = matched.map(|matched| matched.start.saturating_add(matched.end));
    let mut best: Option<(Range<usize>, usize, usize)> = None;
    for start in 0..graphemes.len() {
        for end in start.saturating_add(1)..=graphemes.len() {
            let bytes = graphemes[end - 1]
                .end
                .saturating_sub(graphemes[start].start);
            if bytes > SEARCH_SNIPPET_MAX_BYTES {
                break;
            }
            let window = graphemes[start].start..graphemes[end - 1].end;
            let match_distance = match_center.map_or(0, |center| {
                if center < window.start.saturating_mul(2) {
                    window.start.saturating_mul(2).saturating_sub(center)
                } else if center > window.end.saturating_mul(2) {
                    center.saturating_sub(window.end.saturating_mul(2))
                } else {
                    0
                }
            });
            let replace = best
                .as_ref()
                .is_none_or(|(best_window, best_bytes, best_distance)| {
                    (matched.is_some() && match_distance < *best_distance)
                        || (matched.is_some()
                            && match_distance == *best_distance
                            && bytes > *best_bytes)
                        || (matched.is_none() && bytes > *best_bytes)
                        || (match_distance == *best_distance
                            && bytes == *best_bytes
                            && window.start < best_window.start)
                });
            if replace {
                best = Some((window, bytes, match_distance));
            }
        }
    }
    best.map(|(window, _, _)| window)
}

fn grapheme_window_bytes(graphemes: &[Range<usize>], window: &Range<usize>) -> usize {
    graphemes[window.end - 1]
        .end
        .saturating_sub(graphemes[window.start].start)
}

fn grapheme_span_covering_match(
    graphemes: &[Range<usize>],
    matched: &Range<usize>,
) -> Option<Range<usize>> {
    let start = graphemes
        .iter()
        .position(|grapheme| grapheme.end > matched.start)?;
    let end = graphemes
        .iter()
        .rposition(|grapheme| grapheme.start < matched.end)?
        .saturating_add(1);
    (start < end).then_some(start..end)
}

fn centered_snippet_start(body: &str, grapheme_count: usize, matched: Range<usize>) -> usize {
    let mut match_start = 0;
    let mut match_end = 0;
    for (index, (offset, _)) in body.grapheme_indices(true).enumerate() {
        if offset <= matched.start {
            match_start = index;
        }
        if offset < matched.end {
            match_end = index.saturating_add(1);
        } else {
            break;
        }
    }
    let match_start = match_start.min(grapheme_count.saturating_sub(1));
    let match_end = match_end.min(grapheme_count);
    centered_snippet_start_from_match(grapheme_count, match_start, match_end)
}

fn centered_snippet_start_from_match(
    grapheme_count: usize,
    match_start: usize,
    match_end: usize,
) -> usize {
    let latest_start = grapheme_count.saturating_sub(SEARCH_SNIPPET_MAX_CHARS);
    let match_graphemes = match_end.saturating_sub(match_start).max(1);
    let leading_context = SEARCH_SNIPPET_MAX_CHARS
        .saturating_sub(match_graphemes)
        .saturating_div(2);
    match_start
        .saturating_sub(leading_context)
        .min(latest_start)
}

fn ascii_grapheme_offsets_are_bytes(body: &str) -> bool {
    let mut previous_was_carriage_return = false;
    for byte in body.bytes() {
        if !byte.is_ascii() || (previous_was_carriage_return && byte == b'\n') {
            return false;
        }
        previous_was_carriage_return = byte == b'\r';
    }
    true
}

fn grapheme_byte_range(body: &str, start: usize, end: usize) -> Range<usize> {
    let mut start_offset = None;
    let mut end_offset = None;
    for (index, (offset, _)) in body.grapheme_indices(true).enumerate() {
        if index == start {
            start_offset = Some(offset);
        }
        if index == end {
            end_offset = Some(offset);
            break;
        }
    }
    start_offset.unwrap_or(body.len())..end_offset.unwrap_or(body.len())
}

fn query_match_range(body: &str, query_texts: &[&str]) -> Option<Range<usize>> {
    let folded_body = if body.is_ascii() {
        body.to_ascii_lowercase()
    } else {
        body.to_lowercase()
    };
    let mut best_full_match = None;
    for query_text in query_texts {
        let query_text = query_text.trim();
        if query_text.is_empty() {
            continue;
        }
        update_preferred_match(
            &mut best_full_match,
            folded_match_range(body, &folded_body, query_text),
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
                folded_match_range(body, &folded_body, term),
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

fn folded_match_range(body: &str, folded_body: &str, query_text: &str) -> Option<Range<usize>> {
    let folded_query = query_text.to_lowercase();
    if folded_query.is_empty() {
        return None;
    }
    let folded_start = folded_body.find(&folded_query)?;
    let folded_end = folded_start.saturating_add(folded_query.len());
    if body.is_ascii() {
        return Some(folded_start..folded_end);
    }
    original_range_for_folded_match(body, folded_start, folded_end)
}

fn original_range_for_folded_match(
    body: &str,
    folded_start: usize,
    folded_end: usize,
) -> Option<Range<usize>> {
    let mut folded_offset = 0_usize;
    let mut original_start = None;
    for (original_offset, character) in body.char_indices() {
        let folded_character_bytes = character.to_lowercase().map(char::len_utf8).sum::<usize>();
        let next_folded_offset = folded_offset.saturating_add(folded_character_bytes);
        if original_start.is_none() && folded_start < next_folded_offset {
            original_start = Some(original_offset);
        }
        if folded_end <= next_folded_offset {
            return original_start.map(|start| start..original_offset + character.len_utf8());
        }
        folded_offset = next_folded_offset;
    }
    None
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

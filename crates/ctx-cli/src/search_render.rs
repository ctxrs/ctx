use anyhow::{anyhow, Result};
use ctx_protocol::SEARCH_MAX_SERIALIZED_RESPONSE_BYTES;
use serde_json::{json, Value};
use uuid::Uuid;

use ctx_history_core::{ContextCitation, ContextCitationType, HistoryRecord};
use ctx_history_store::Store;

use crate::commands::search::SearchRefreshReport;
use crate::output::compact_json;
use crate::semantic::SemanticRetrievalReport;
use crate::transcript::shell_quote_arg;

pub(crate) struct SearchDto;
impl SearchDto {
    pub(crate) fn packet(
        store: &Store,
        packet: &ctx_history_search::SearchPacket,
        refresh: &SearchRefreshReport,
        retrieval: &SemanticRetrievalReport,
        suggested_next_query: Option<&str>,
    ) -> Result<Value> {
        bounded_search_dto(compact_json(json!({
            "schema_version": 2,
            "payload_type": "search_results",
            "query": packet.query_spec,
            "query_execution": packet.query_execution,
            "filters": packet.filters,
            "freshness": refresh.to_json(),
            "retrieval": retrieval.to_json(),
            "generated_at": packet.generated_at,
            "results": packet
                .results
                .iter()
                .map(|result| {
                    compact_json(json!({
                        "item_id": result.record_id,
                        "result_type": search_result_type(store, result),
                        "ctx_event_id": result.event_id,
                        "ctx_session_id": result.session_id,
                        "session_id": result.session_id,
                        "event_id": result.event_id,
                        "event_seq": result.event_seq,
                        "title": result.title,
                        "snippet": result.snippet,
                        "rank": result.rank,
                        "result_scope": result.result_scope,
                        "session_importance": (result.result_scope == ctx_history_search::SearchResultScope::Session)
                            .then_some(result.session_importance),
                        "more_matches_in_session": (result.result_scope == ctx_history_search::SearchResultScope::Session)
                            .then_some(result.more_matches_in_session),
                        "provider": result.provider,
                        "provider_session_id": result.provider_session_id,
                        "history_source": result.history_source,
                        "history_source_plugin": result.history_source_plugin,
                        "provider_key": result.provider_key,
                        "source_id": result.source_id,
                        "source_format": result.source_format,
                        "timestamp": result.timestamp,
                        "cwd": result.cwd,
                        "source_path": result.raw_source_path,
                        "source_exists": result.raw_source_exists,
                        "cursor": result.cursor,
                        "suggested_next_commands": search_next_commands(result, suggested_next_query),
                        "why_matched": result.why_matched,
                        "citations": public_citations(&result.citations),
                        "links": result.links,
                        "visibility": result.visibility,
                    }))
                })
                .collect::<Vec<_>>(),
            "pagination": packet.pagination,
            "truncation": packet.truncation,
        })))
    }
}

fn bounded_search_dto(mut structured: Value) -> Result<Value> {
    let result_count = structured
        .get("results")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if render_search_dto_bytes(&mut structured)? <= SEARCH_MAX_SERIALIZED_RESPONSE_BYTES {
        return Ok(structured);
    }

    let mut smallest = 0_usize;
    let mut largest = result_count;
    let mut best = None;
    while smallest <= largest {
        let keep = smallest + (largest - smallest) / 2;
        let mut candidate = structured.clone();
        truncate_search_transport(&mut candidate, keep);
        if render_search_dto_bytes(&mut candidate)? <= SEARCH_MAX_SERIALIZED_RESPONSE_BYTES {
            best = Some(candidate);
            smallest = keep.saturating_add(1);
        } else {
            if keep == 0 {
                break;
            }
            largest = keep - 1;
        }
    }

    best.ok_or_else(|| {
        anyhow!(
            "bounded search metadata exceeds the {SEARCH_MAX_SERIALIZED_RESPONSE_BYTES}-byte response cap"
        )
    })
}

fn render_search_dto_bytes(structured: &mut Value) -> Result<usize> {
    for _ in 0..8 {
        let serialized_bytes = serde_json::to_vec_pretty(structured)?
            .len()
            .saturating_add(1);
        let Some(consumed) = structured
            .get_mut("query_execution")
            .and_then(Value::as_object_mut)
            .and_then(|execution| execution.get_mut("consumed"))
            .and_then(Value::as_object_mut)
        else {
            return Ok(serialized_bytes);
        };
        if consumed
            .get("serialized_response_bytes")
            .and_then(Value::as_u64)
            == Some(serialized_bytes as u64)
        {
            return Ok(serialized_bytes);
        }
        consumed.insert(
            "serialized_response_bytes".to_owned(),
            json!(serialized_bytes),
        );
    }
    Err(anyhow!(
        "bounded search response byte accounting did not converge"
    ))
}

pub(crate) fn truncate_search_transport(structured: &mut Value, keep: usize) {
    let Some(results) = structured.get_mut("results").and_then(Value::as_array_mut) else {
        return;
    };
    let keep = keep.min(results.len());
    let removed_results = results.len().saturating_sub(keep);
    let removed_text_bytes = results[keep..]
        .iter()
        .map(search_result_text_bytes)
        .fold(0_usize, usize::saturating_add);
    results.truncate(keep);
    if removed_results == 0 {
        return;
    }

    if let Some(pagination) = structured
        .get_mut("pagination")
        .and_then(Value::as_object_mut)
    {
        pagination.insert("has_more".to_owned(), Value::Bool(true));
    }
    if let Some(truncation) = structured
        .get_mut("truncation")
        .and_then(Value::as_object_mut)
    {
        truncation.insert("truncated".to_owned(), Value::Bool(true));
        truncation.insert("reason".to_owned(), json!("serialized_response_bytes"));
        let omitted = truncation
            .get("omitted_results")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .saturating_add(removed_results as u64);
        truncation.insert("omitted_results".to_owned(), json!(omitted));
    }
    let Some(execution) = structured
        .get_mut("query_execution")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    execution.insert("truncated".to_owned(), Value::Bool(true));
    let reasons = execution
        .entry("truncation_reasons".to_owned())
        .or_insert_with(|| json!([]));
    if let Some(reasons) = reasons.as_array_mut() {
        let reason = json!("serialized_response_bytes");
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    if let Some(consumed) = execution.get_mut("consumed").and_then(Value::as_object_mut) {
        let returned = consumed
            .get("returned_results")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .saturating_sub(removed_results as u64);
        consumed.insert("returned_results".to_owned(), json!(returned));
        let returned_text_bytes = consumed
            .get("returned_text_bytes")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .saturating_sub(removed_text_bytes as u64);
        consumed.insert("returned_text_bytes".to_owned(), json!(returned_text_bytes));
    }
}

fn search_result_text_bytes(result: &Value) -> usize {
    result
        .get("title")
        .and_then(Value::as_str)
        .map_or(0, str::len)
        .saturating_add(
            result
                .get("snippet")
                .and_then(Value::as_str)
                .map_or(0, str::len),
        )
}

pub(crate) fn search_result_type(
    store: &Store,
    result: &ctx_history_search::SearchPacketResult,
) -> String {
    if result.result_scope == ctx_history_search::SearchResultScope::Session {
        return "session_result".to_owned();
    }
    if result.event_id == Some(result.record_id) {
        return "event".to_owned();
    }
    if result.session_id == Some(result.record_id) {
        return "session".to_owned();
    }
    result_type_for_id(store, result.record_id)
}

pub(crate) fn search_next_commands(
    result: &ctx_history_search::SearchPacketResult,
    query: Option<&str>,
) -> Vec<String> {
    let mut commands = Vec::new();
    if result.result_scope == ctx_history_search::SearchResultScope::Session {
        if let Some(id) = result.session_id {
            commands.push(format!("ctx show session {id}"));
            if let Some(event_id) = result.event_id {
                commands.push(format!("ctx show event {event_id} --window 10"));
            }
            if let Some(query) = query.filter(|query| !query.trim().is_empty()) {
                commands.push(format!(
                    "ctx search {} --session {id}",
                    shell_quote_arg(query)
                ));
            }
            commands.push(format!("ctx locate session {id}"));
            if let Some(event_id) = result.event_id {
                commands.push(format!("ctx locate event {event_id}"));
            }
        }
        return commands;
    }
    if let Some(id) = result.event_id {
        commands.push(format!("ctx show event {id} --window 10"));
        commands.push(format!("ctx locate event {id}"));
    }
    if result.result_scope != ctx_history_search::SearchResultScope::Session {
        if let Some(id) = result.session_id {
            if let Some(query) = query.filter(|query| !query.trim().is_empty()) {
                commands.push(format!(
                    "ctx search {} --session {id}",
                    shell_quote_arg(query)
                ));
            }
            commands.push(format!("ctx show session {id}"));
            commands.push(format!("ctx locate session {id}"));
        }
    }
    commands
}

pub(crate) fn public_citations(citations: &[ContextCitation]) -> Vec<Value> {
    citations
        .iter()
        .map(|citation| {
            let ctx_event_id = if citation.citation_type == ContextCitationType::Event {
                Some(citation.id)
            } else {
                None
            };
            let ctx_session_id = if citation.citation_type == ContextCitationType::Session {
                Some(citation.id)
            } else {
                citation.session_id
            };
            compact_json(json!({
                "item_id": citation.id,
                "target_type": public_citation_target_type(citation.citation_type),
                "ctx_event_id": ctx_event_id,
                "ctx_session_id": ctx_session_id,
                "label": citation.label,
                "time": citation.time,
                "provider": citation.provider,
                "session_id": citation.session_id,
                "event_seq": citation.event_seq,
                "source_path": citation.raw_source_path,
                "source_exists": citation.raw_source_exists,
                "cursor": citation.cursor,
            }))
        })
        .collect()
}

pub(crate) fn public_citation_target_type(citation_type: ContextCitationType) -> &'static str {
    match citation_type {
        ContextCitationType::HistoryRecord => "indexed_item",
        ContextCitationType::Session => "session",
        ContextCitationType::Run => "run",
        ContextCitationType::Event => "event",
        ContextCitationType::VcsChange => "vcs_change",
        ContextCitationType::Artifact => "artifact",
        ContextCitationType::Summary => "summary",
        ContextCitationType::File => "file",
    }
}

pub(crate) fn public_record_type(record: &HistoryRecord) -> String {
    let record_type = record.kind.trim();
    match record_type {
        "" | "record" => "indexed_item".to_owned(),
        value => value.to_owned(),
    }
}

pub(crate) fn result_type_for_id(store: &Store, item_id: Uuid) -> String {
    if let Ok(record) = store.get_record(item_id) {
        return public_record_type(&record);
    }
    if store.get_event(item_id).is_ok() {
        return "event".to_owned();
    }
    if store.get_session(item_id).is_ok() {
        return "session".to_owned();
    }
    if store.get_run(item_id).is_ok() {
        return "run".to_owned();
    }
    "indexed_item".to_owned()
}

pub(crate) fn print_search_result_compact(
    index: usize,
    result: &ctx_history_search::SearchPacketResult,
) {
    println!("{index}. {}", result.title);
    let summary = search_result_summary(result);
    if !summary.is_empty() {
        println!("   {}", summary.join(" | "));
    }
    let snippet = result.snippet.trim();
    if !snippet.is_empty() {
        println!("   {snippet}");
    }
    if result.result_scope == ctx_history_search::SearchResultScope::Session
        && result.more_matches_in_session > 0
    {
        println!(
            "   {} more results from this session",
            result.more_matches_in_session
        );
    }
    if let Some(command) = search_inspect_command(result) {
        println!("   inspect: {command}");
    }
}

pub(crate) fn print_search_result_verbose(
    result: &ctx_history_search::SearchPacketResult,
    suggested_next_query: Option<&str>,
) {
    println!("{}", result.title);
    if let Some(event_id) = result.event_id {
        println!("  ctx_event_id: {event_id}");
    }
    if let Some(session_id) = result.session_id {
        println!("  ctx_session_id: {session_id}");
    }
    if let Some(provider_session_id) = &result.provider_session_id {
        println!("  provider_session_id: {provider_session_id}");
    }
    if let Some(history_source) = &result.history_source {
        println!("  history_source: {history_source}");
    }
    if let Some(provider_key) = &result.provider_key {
        println!("  provider_key: {provider_key}");
    }
    if let Some(source_id) = &result.source_id {
        println!("  source_id: {source_id}");
    }
    if let Some(source_format) = &result.source_format {
        println!("  source_format: {source_format}");
    }
    println!("  {}", result.snippet);
    println!("  rank: {:.2}", result.rank);
    if result.result_scope == ctx_history_search::SearchResultScope::Session {
        println!("  session_importance: {:.2}", result.session_importance);
        if result.more_matches_in_session > 0 {
            println!(
                "  more_matches_in_session: {}",
                result.more_matches_in_session
            );
        }
    }
    for command in search_next_commands(result, suggested_next_query)
        .into_iter()
        .take(3)
    {
        println!("  next: {command}");
    }
    for citation in result.citations.iter().take(2) {
        println!(
            "  citation: {} {}",
            public_citation_target_type(citation.citation_type),
            citation.id
        );
    }
}

pub(crate) fn search_result_summary(
    result: &ctx_history_search::SearchPacketResult,
) -> Vec<String> {
    let mut summary = Vec::new();
    if let Some(provider) = result.provider {
        summary.push(provider.as_str().to_owned());
    }
    if let Some(history_source) = &result.history_source {
        summary.push(history_source.clone());
    } else if let (Some(provider_key), Some(source_id)) = (&result.provider_key, &result.source_id)
    {
        summary.push(format!("{provider_key}/{source_id}"));
    }
    if result.result_scope == ctx_history_search::SearchResultScope::Session {
        summary.push(format!("importance {:.2}", result.session_importance));
    } else {
        summary.push(format!("rank {:.2}", result.rank));
    }
    if let Some(session_id) = result.session_id {
        summary.push(format!("session {}", short_uuid(session_id)));
    }
    if let Some(event_id) = result.event_id {
        summary.push(format!("event {}", short_uuid(event_id)));
    }
    if let Some(timestamp) = result.timestamp {
        summary.push(timestamp.to_rfc3339());
    }
    summary
}

pub(crate) fn short_uuid(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

pub(crate) fn search_inspect_command(
    result: &ctx_history_search::SearchPacketResult,
) -> Option<String> {
    result
        .event_id
        .map(|id| format!("ctx show event {id} --window 10"))
        .or_else(|| {
            result
                .session_id
                .map(|id| format!("ctx show session {id} --mode lite"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_pretty_json_transport_is_bounded_and_accounted() {
        let large_text = "x".repeat(900_000);
        let results = (0..3)
            .map(|index| json!({"title": format!("result {index}"), "snippet": large_text}))
            .collect::<Vec<_>>();
        let value = json!({
            "query_execution": {
                "truncated": false,
                "truncation_reasons": [],
                "consumed": {
                    "returned_results": 3,
                    "returned_text_bytes": 2_700_024,
                    "serialized_response_bytes": 0,
                }
            },
            "pagination": {"has_more": false},
            "truncation": {"truncated": false, "omitted_results": 0},
            "results": results,
        });

        let bounded = bounded_search_dto(value).unwrap();
        let serialized_bytes = serde_json::to_vec_pretty(&bounded)
            .unwrap()
            .len()
            .saturating_add(1);
        assert!(serialized_bytes <= SEARCH_MAX_SERIALIZED_RESPONSE_BYTES);
        assert_eq!(bounded["results"].as_array().unwrap().len(), 2);
        assert_eq!(bounded["pagination"]["has_more"], true);
        assert_eq!(bounded["query_execution"]["truncated"], true);
        assert_eq!(
            bounded["query_execution"]["consumed"]["serialized_response_bytes"],
            serialized_bytes
        );
    }
}

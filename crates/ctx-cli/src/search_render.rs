use serde_json::{json, Value};
use uuid::Uuid;

use ctx_history_core::{ContextCitation, ContextCitationType, HistoryRecord};
use ctx_history_store::Store;

use crate::commands::search::SearchRefreshReport;
use crate::output::compact_json;
use crate::transcript::shell_quote_arg;

pub(crate) struct SearchDto;
impl SearchDto {
    pub(crate) fn packet(
        store: &Store,
        packet: &ctx_history_search::SearchPacket,
        refresh: &SearchRefreshReport,
        suggested_next_query: Option<&str>,
    ) -> Value {
        compact_json(json!({
            "schema_version": packet.schema_version,
            "query": packet.query,
            "filters": packet.filters,
            "freshness": refresh.to_json(),
            "generated_at": packet.generated_at,
            "results": packet
                .results
                .iter()
                .map(|result| {
                    compact_json(json!({
                        "item_id": result.record_id,
                        "item_type": search_result_item_type(store, result),
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
        }))
    }
}

pub(crate) fn search_result_item_type(
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
    item_type_for_id(store, result.record_id)
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
                "item_type": public_citation_item_type(citation.citation_type),
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

pub(crate) fn public_citation_item_type(citation_type: ContextCitationType) -> &'static str {
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

pub(crate) fn public_record_item_type(record: &HistoryRecord) -> String {
    let item_type = record.kind.trim();
    match item_type {
        "" | "record" => "indexed_item".to_owned(),
        value => value.to_owned(),
    }
}

pub(crate) fn item_type_for_id(store: &Store, item_id: Uuid) -> String {
    if let Ok(record) = store.get_record(item_id) {
        return public_record_item_type(&record);
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
            public_citation_item_type(citation.citation_type),
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

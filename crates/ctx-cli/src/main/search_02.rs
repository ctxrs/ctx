#[allow(unused_imports)]
use super::*;

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

pub(crate) fn search_refresh_sources(provider: Option<ProviderArg>) -> Vec<SourceInfo> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let mut sources = if let Some(provider) = provider {
        discover_provider_sources_for_provider(&home, provider.capture_provider())
    } else {
        discovered_sources()
    };
    sources
        .drain(..)
        .filter(|source| {
            source.exists
                && source.import_support.is_auto_importable()
                && source.status == ProviderSourceStatus::Available
                && source.source_format != "codex_history_jsonl"
        })
        .collect()
}

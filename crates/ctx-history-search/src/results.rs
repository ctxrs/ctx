use std::{cmp::Ordering, collections::BTreeMap, path::Path};

use ctx_history_core::{
    ContextCitation, ContextCitationType, ContextLinks, EventType, HistoryRecord, Visibility,
};
use ctx_history_store::EventSearchHit;
use uuid::Uuid;

use crate::filters::hit_matches_history_source_filter;
use crate::model::{Candidate, HitMetadata};
use crate::packet::{SearchPacketResult, SearchResultScope};
use crate::query::{query_terms, PacketOptions, SearchFilters, SearchResultMode};
use crate::snippets::{local_snippet, matched_snippet, search_snippet};
use crate::source::{event_hit, session_hit};

pub(crate) fn search_result_merge_key(
    result: &SearchPacketResult,
    result_mode: SearchResultMode,
) -> Uuid {
    if result_mode == SearchResultMode::Sessions {
        result.session_id.unwrap_or(result.record_id)
    } else {
        result.event_id.unwrap_or(result.record_id)
    }
}

pub(crate) fn merge_search_result(existing: &mut SearchPacketResult, incoming: SearchPacketResult) {
    let incoming_rank = incoming.rank;
    let existing_rank = existing.rank;
    if incoming_rank > existing_rank {
        existing.title = incoming.title.clone();
        existing.snippet = incoming.snippet.clone();
        existing.record_id = incoming.record_id;
        existing.event_id = incoming.event_id;
        existing.event_seq = incoming.event_seq;
        existing.timestamp = incoming.timestamp;
        existing.cwd = incoming.cwd.clone();
        existing.provider = incoming.provider;
        existing.provider_session_id = incoming.provider_session_id.clone();
        existing.history_source = incoming.history_source.clone();
        existing.history_source_plugin = incoming.history_source_plugin.clone();
        existing.provider_key = incoming.provider_key.clone();
        existing.source_id = incoming.source_id.clone();
        existing.source_format = incoming.source_format.clone();
        existing.raw_source_path = incoming.raw_source_path.clone();
        existing.raw_source_exists = incoming.raw_source_exists;
        existing.cursor = incoming.cursor.clone();
    } else {
        existing.history_source = existing
            .history_source
            .clone()
            .or(incoming.history_source.clone());
        existing.history_source_plugin = existing
            .history_source_plugin
            .clone()
            .or(incoming.history_source_plugin.clone());
        existing.provider_key = existing
            .provider_key
            .clone()
            .or(incoming.provider_key.clone());
        existing.source_id = existing.source_id.clone().or(incoming.source_id.clone());
        existing.source_format = existing
            .source_format
            .clone()
            .or(incoming.source_format.clone());
    }
    existing.rank = existing_rank.max(incoming_rank) + 0.08;
    existing.more_matches_in_session = existing
        .more_matches_in_session
        .saturating_add(1)
        .saturating_add(incoming.more_matches_in_session);
    if existing.result_scope == SearchResultScope::Session {
        existing.session_importance =
            session_importance(existing.rank, existing.more_matches_in_session);
    }
    for reason in incoming.why_matched {
        push_unique_why(&mut existing.why_matched, reason);
    }
    for citation in incoming.citations {
        let duplicate = existing.citations.iter().any(|existing_citation| {
            existing_citation.citation_type == citation.citation_type
                && existing_citation.id == citation.id
        });
        if !duplicate {
            existing.citations.push(citation);
        }
    }
}

pub(crate) fn push_unique_why(why_matched: &mut Vec<String>, reason: String) {
    if !why_matched.iter().any(|value| value == &reason) {
        why_matched.push(reason);
    }
}

pub(crate) fn compare_search_results(
    left: &SearchPacketResult,
    right: &SearchPacketResult,
) -> Ordering {
    right
        .rank
        .partial_cmp(&left.rank)
        .unwrap_or(Ordering::Equal)
        .then_with(|| right.timestamp.cmp(&left.timestamp))
        .then_with(|| left.record_id.cmp(&right.record_id))
}

pub(crate) fn push_candidate_results(
    results: &mut Vec<SearchPacketResult>,
    candidates: &[Candidate],
    query: &str,
    options: &PacketOptions,
) {
    let mut clustered_index = BTreeMap::<Uuid, usize>::new();
    for candidate in candidates {
        let mut result = candidate_search_result(candidate, query, options);
        if options.result_mode == SearchResultMode::Sessions {
            let cluster_id = result.session_id.unwrap_or(result.record_id);
            if let Some(index) = clustered_index.get(&cluster_id).copied() {
                let existing = &mut results[index];
                existing.more_matches_in_session =
                    existing.more_matches_in_session.saturating_add(1);
                existing.session_importance =
                    session_importance(existing.rank, existing.more_matches_in_session);
                continue;
            }
            if result.session_id.is_some() {
                result.result_scope = SearchResultScope::Session;
                result.session_importance = session_importance(result.rank, 0);
            }
            clustered_index.insert(cluster_id, results.len());
        }
        results.push(result);
        if results.len() >= options.limit {
            break;
        }
    }
}

pub(crate) fn candidate_search_result(
    candidate: &Candidate,
    query: &str,
    options: &PacketOptions,
) -> SearchPacketResult {
    let display_hit = candidate_display_hit(candidate, &options.filters);
    let record_id = candidate
        .primary_hit
        .as_ref()
        .and_then(|hit| hit.event_id)
        .unwrap_or(candidate.record.id);
    SearchPacketResult {
        record_id,
        session_id: display_hit.as_ref().and_then(|hit| hit.session_id),
        event_id: display_hit.as_ref().and_then(|hit| hit.event_id),
        event_seq: display_hit.as_ref().and_then(|hit| hit.event_seq),
        title: local_snippet(&candidate.record.title, 240),
        snippet: search_snippet(
            &candidate.record,
            &candidate.context,
            query,
            options.snippet_chars,
            &options.filters,
        ),
        rank: candidate.score,
        result_scope: SearchResultScope::Event,
        more_matches_in_session: 0,
        session_importance: 0.0,
        provider: display_hit.as_ref().and_then(|hit| hit.provider),
        provider_session_id: display_hit
            .as_ref()
            .and_then(|hit| hit.provider_session_id.clone()),
        history_source: display_hit
            .as_ref()
            .and_then(|hit| hit.history_source.clone()),
        history_source_plugin: display_hit
            .as_ref()
            .and_then(|hit| hit.history_source_plugin.clone()),
        provider_key: display_hit
            .as_ref()
            .and_then(|hit| hit.provider_key.clone()),
        source_id: display_hit.as_ref().and_then(|hit| hit.source_id.clone()),
        source_format: display_hit
            .as_ref()
            .and_then(|hit| hit.source_format.clone()),
        timestamp: display_hit.as_ref().map(|hit| hit.time),
        cwd: display_hit.as_ref().and_then(|hit| hit.cwd.clone()),
        raw_source_path: display_hit
            .as_ref()
            .and_then(|hit| hit.raw_source_path.clone()),
        raw_source_exists: display_hit.as_ref().and_then(|hit| hit.raw_source_exists),
        cursor: display_hit.as_ref().and_then(|hit| hit.cursor.clone()),
        why_matched: candidate.why_matched.clone(),
        citations: candidate.citations.clone(),
        links: links_for(&candidate.record, options),
        visibility: Visibility::LocalOnly,
    }
}

pub(crate) fn candidate_display_hit(
    candidate: &Candidate,
    filters: &SearchFilters,
) -> Option<HitMetadata> {
    if let Some(hit) = &candidate.primary_hit {
        if hit.event_id.is_some() {
            return Some(hit.clone());
        }
    }
    if let Some(event) = candidate.context.events.iter().find(|event| {
        let hit = event_hit(event, &candidate.context);
        filters
            .provider
            .map_or(true, |provider| hit.provider == Some(provider))
            && filters
                .session
                .map_or(true, |id| hit.session_id == Some(id))
            && hit_matches_history_source_filter(&hit, filters)
    }) {
        return Some(event_hit(event, &candidate.context));
    }
    if let Some(hit) = &candidate.primary_hit {
        if hit.provider.is_some() || hit.session_id.is_some() {
            return Some(hit.clone());
        }
    }
    candidate
        .context
        .sessions
        .iter()
        .find(|session| {
            filters
                .provider
                .map_or(true, |provider| session.provider == provider)
                && filters.session.map_or(true, |id| session.id == id)
                && hit_matches_history_source_filter(
                    &session_hit(session, &candidate.context),
                    filters,
                )
        })
        .or_else(|| candidate.context.sessions.first())
        .map(|session| session_hit(session, &candidate.context))
}

pub(crate) fn event_search_result(
    hit: &EventSearchHit,
    query: &str,
    snippet_chars: usize,
) -> SearchPacketResult {
    let terms = query_terms(query);
    let raw_source_exists = hit
        .raw_source_path
        .as_deref()
        .map(|path| Path::new(path).exists());
    let mut citations = vec![ContextCitation {
        citation_type: ContextCitationType::Event,
        id: hit.event_id,
        label: event_result_label(hit).to_owned(),
        time: hit.occurred_at,
        provider: hit.provider,
        session_id: hit.session_id,
        event_seq: Some(hit.seq),
        raw_source_path: hit.raw_source_path.clone(),
        raw_source_exists,
        cursor: hit.cursor.clone(),
    }];
    if let Some(session_id) = hit.session_id {
        citations.push(ContextCitation {
            citation_type: ContextCitationType::Session,
            id: session_id,
            label: "session".to_owned(),
            time: hit.occurred_at,
            provider: hit.provider,
            session_id: Some(session_id),
            event_seq: None,
            raw_source_path: hit.raw_source_path.clone(),
            raw_source_exists,
            cursor: hit.cursor.clone(),
        });
    }

    SearchPacketResult {
        record_id: hit.event_id,
        session_id: hit.session_id,
        event_id: Some(hit.event_id),
        event_seq: Some(hit.seq),
        title: event_result_title(hit),
        snippet: matched_snippet(&hit.preview, &terms, snippet_chars),
        rank: (-hit.score as f32).max(0.0),
        result_scope: SearchResultScope::Event,
        more_matches_in_session: 0,
        session_importance: 0.0,
        provider: hit.provider,
        provider_session_id: hit.session_external_session_id.clone(),
        history_source: hit.history_source.clone(),
        history_source_plugin: hit.history_source_plugin.clone(),
        provider_key: hit.provider_key.clone(),
        source_id: hit.source_id.clone(),
        source_format: hit.source_format.clone(),
        timestamp: Some(hit.occurred_at),
        cwd: hit.cwd.clone(),
        raw_source_path: hit.raw_source_path.clone(),
        raw_source_exists,
        cursor: hit.cursor.clone(),
        why_matched: vec![event_reason(hit.event_type).to_owned()],
        citations,
        links: ContextLinks::default(),
        visibility: Visibility::LocalOnly,
    }
}

pub(crate) fn event_result_title(hit: &EventSearchHit) -> String {
    let provider = hit
        .provider
        .map(|provider| provider.as_str())
        .unwrap_or("agent");
    let source = hit
        .session_external_session_id
        .as_deref()
        .or_else(|| {
            hit.raw_source_path
                .as_deref()
                .and_then(|path| Path::new(path).file_name().and_then(|value| value.to_str()))
        })
        .map(|value| local_snippet(value, 80));
    match source {
        Some(source) => format!("{provider} {} - {source}", event_result_label(hit)),
        None => format!("{provider} {}", event_result_label(hit)),
    }
}

pub(crate) fn event_result_label(hit: &EventSearchHit) -> &'static str {
    match hit.event_type {
        EventType::Message => match hit.role {
            Some(ctx_history_core::EventRole::User) => "user message",
            Some(ctx_history_core::EventRole::Assistant) => "assistant message",
            Some(ctx_history_core::EventRole::System) => "system message",
            _ => "message",
        },
        EventType::ToolCall => "tool call",
        EventType::ToolOutput => "tool output",
        EventType::CommandStarted => "command started",
        EventType::CommandOutput => "command output",
        EventType::CommandFinished => "command finished",
        EventType::FileTouched => "file touched",
        EventType::VcsChange => "vcs change",
        EventType::Artifact => "artifact",
        EventType::Summary => "summary",
        EventType::Notice => "notice",
    }
}

pub(crate) fn event_reason(event_type: EventType) -> &'static str {
    match event_type {
        EventType::Message => "message",
        EventType::ToolCall => "tool_call",
        EventType::ToolOutput => "tool_output",
        EventType::CommandStarted | EventType::CommandOutput | EventType::CommandFinished => {
            "command_event"
        }
        EventType::FileTouched => "file_touched",
        EventType::VcsChange => "vcs_change",
        EventType::Artifact => "artifact",
        EventType::Summary => "summary",
        EventType::Notice => "notice",
    }
}

pub(crate) fn normalize_search_result_ranks(results: &mut [SearchPacketResult]) {
    let max_rank = results
        .iter()
        .map(|result| result.rank)
        .fold(0.0_f32, f32::max);
    if max_rank <= 0.0 {
        return;
    }
    for result in results.iter_mut() {
        result.rank = (result.rank / max_rank).clamp(0.0, 1.0);
    }
    for result in results.iter_mut() {
        if result.result_scope == SearchResultScope::Session {
            result.session_importance =
                session_importance(result.rank, result.more_matches_in_session);
        } else {
            result.session_importance = 0.0;
        }
    }
}

pub(crate) fn session_importance(rank: f32, more_matches_in_session: usize) -> f32 {
    let coverage_boost = ((more_matches_in_session as f32).ln_1p() * 0.08).min(0.24);
    (rank + coverage_boost).clamp(0.0, 1.0)
}

pub(crate) fn links_for(_record: &HistoryRecord, _options: &PacketOptions) -> ContextLinks {
    ContextLinks {}
}

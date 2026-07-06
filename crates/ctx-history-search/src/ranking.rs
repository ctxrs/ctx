use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use ctx_history_core::{ContextCitation, ContextCitationType, HistoryRecord};
use ctx_history_store::{FileTouchScope, Store};
use uuid::Uuid;

use crate::filters::{
    context_has_excluded_provider_session, has_filters, hit_matches_excluded_provider_session,
    is_agent_history_bookkeeping_record, item_matches_agent_scope, record_matches_filters,
    record_text_matches_agent_scope, session_matches_agent_scope,
    source_id_matches_history_source_filter,
};
use crate::model::{Candidate, CandidateSearch, HitMetadata, RecordContext, SearchSection};
use crate::query::{
    query_terms, PacketOptions, Result, SearchFilters, FILTERED_SEARCH_MAX_PAGES,
    FILTERED_SEARCH_PAGE_SIZE,
};
use crate::snippets::{event_text, event_weight, joined, matches_terms};
use crate::source::{
    artifact_hit, citation, empty_hit, event_hit, file_hit, file_touched_search_text,
    record_context_display_hit, run_hit, session_hit, source_hit,
};

pub(crate) fn ranked_candidates(
    store: &Store,
    query: Option<&str>,
    options: &PacketOptions,
    file_scope: Option<&FileTouchScope>,
) -> Result<CandidateSearch> {
    let target_candidates = options.limit.saturating_add(1);
    let terms = query_terms(query.unwrap_or_default());
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::<Uuid>::new();
    let mut scan_budget_exhausted = false;
    let file_only = terms.is_empty() && file_scope.is_some();
    if terms.is_empty() && !file_only {
        return Ok(CandidateSearch {
            candidates,
            scan_budget_exhausted,
        });
    }

    if file_only {
        let Some(scope) = file_scope else {
            return Ok(CandidateSearch {
                candidates,
                scan_budget_exhausted,
            });
        };
        for record_id in &scope.history_record_ids {
            if !seen.insert(*record_id) {
                continue;
            }
            let record = store.get_record(*record_id)?;
            if let Some(candidate) =
                candidate_for_record(store, record, &terms, &options.filters, file_scope)?
            {
                candidates.push(candidate);
            }
        }
        normalize_scores(&mut candidates);
        candidates.sort_by(compare_candidates);
        if candidates.len() > target_candidates {
            candidates.truncate(target_candidates);
        }
        return Ok(CandidateSearch {
            candidates,
            scan_budget_exhausted,
        });
    }

    let filtered = has_filters(&options.filters);
    if filtered {
        let page_size = FILTERED_SEARCH_PAGE_SIZE.max(target_candidates);
        let mut offset = 0_usize;
        let mut pages_scanned = 0_usize;
        loop {
            pages_scanned = pages_scanned.saturating_add(1);
            let records = match query {
                Some(query) if !query.trim().is_empty() => {
                    store.search_records_page(query, page_size, offset)?
                }
                _ => Vec::new(),
            };
            let page_len = records.len();

            for record in records {
                if !seen.insert(record.id) {
                    continue;
                }
                if let Some(scope) = file_scope {
                    if !scope.history_record_ids.is_empty()
                        && !scope.history_record_ids.contains(&record.id)
                    {
                        continue;
                    }
                }
                if let Some(candidate) =
                    candidate_for_record(store, record, &terms, &options.filters, file_scope)?
                {
                    candidates.push(candidate);
                }
            }

            if candidates.len() >= target_candidates || page_len < page_size {
                break;
            }
            if pages_scanned >= FILTERED_SEARCH_MAX_PAGES {
                scan_budget_exhausted = true;
                break;
            }
            let next_offset = offset.saturating_add(page_size);
            if next_offset == offset {
                break;
            }
            offset = next_offset;
        }
    } else {
        let fetch_limit = target_candidates;
        let records = match query {
            Some(query) if !query.trim().is_empty() => store.search_records(query, fetch_limit)?,
            _ => Vec::new(),
        };
        for record in records {
            if !seen.insert(record.id) {
                continue;
            }
            if file_scope.is_some_and(|scope| !scope.history_record_ids.contains(&record.id)) {
                continue;
            }
            if let Some(candidate) =
                candidate_for_record(store, record, &terms, &options.filters, file_scope)?
            {
                candidates.push(candidate);
            }
        }
    }

    normalize_scores(&mut candidates);
    candidates.sort_by(compare_candidates);
    if candidates.len() > target_candidates {
        candidates.truncate(target_candidates);
    }
    Ok(CandidateSearch {
        candidates,
        scan_budget_exhausted,
    })
}

pub(crate) fn compare_candidates(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| right.record.updated_at.cmp(&left.record.updated_at))
        .then_with(|| left.record.title.cmp(&right.record.title))
        .then_with(|| left.record.id.cmp(&right.record.id))
}

pub(crate) fn candidate_for_record(
    store: &Store,
    record: HistoryRecord,
    terms: &[String],
    filters: &SearchFilters,
    file_scope: Option<&FileTouchScope>,
) -> Result<Option<Candidate>> {
    let context = hydrate_record_context(store, record.id, filters.file.as_deref())?;
    if !record_matches_filters(&record, &context, filters, file_scope) {
        return Ok(None);
    }
    let analysis = analyze_record(&record, &context, terms, filters);
    if terms.is_empty() || analysis.score > 0.0 {
        Ok(Some(Candidate {
            record,
            context,
            score: analysis.score,
            why_matched: analysis.why_matched,
            citations: analysis.citations,
            primary_hit: analysis.primary_hit,
        }))
    } else {
        Ok(None)
    }
}

pub(crate) fn hydrate_record_context(
    store: &Store,
    record_id: Uuid,
    file_filter: Option<&str>,
) -> Result<RecordContext> {
    let sessions = store.sessions_for_record(record_id)?;
    let runs = store.runs_for_record(record_id)?;
    let events = store.events_for_record(record_id)?;
    let artifacts = store.artifacts_for_record(record_id)?;
    let files_touched =
        if let Some(file) = file_filter.map(str::trim).filter(|value| !value.is_empty()) {
            store.files_touched_for_record_matching(record_id, file)?
        } else {
            store.files_touched_for_record(record_id)?
        };
    let vcs_changes = store.vcs_changes_for_record(record_id)?;
    let summaries = store.summaries_for_record(record_id)?;
    let mut source_ids = BTreeSet::new();
    for session in &sessions {
        if let Some(id) = session.capture_source_id {
            source_ids.insert(id);
        }
    }
    for run in &runs {
        if let Some(id) = run.source_id {
            source_ids.insert(id);
        }
    }
    for event in &events {
        if let Some(id) = event.capture_source_id {
            source_ids.insert(id);
        }
    }
    for artifact in &artifacts {
        if let Some(id) = artifact.source_id {
            source_ids.insert(id);
        }
    }
    for file in &files_touched {
        if let Some(id) = file.source_id {
            source_ids.insert(id);
        }
    }
    for change in &vcs_changes {
        if let Some(id) = change.source_id {
            source_ids.insert(id);
        }
    }
    for summary in &summaries {
        if let Some(id) = summary.source_id {
            source_ids.insert(id);
        }
    }
    let mut sources = BTreeMap::new();
    for source_id in source_ids {
        if let Ok(source) = store.get_capture_source(source_id) {
            sources.insert(source_id, source);
        }
    }

    Ok(RecordContext {
        sessions,
        runs,
        events,
        artifacts,
        files_touched,
        vcs_changes,
        summaries,
        sources,
    })
}

struct MatchAnalysis {
    score: f32,
    why_matched: Vec<String>,
    citations: Vec<ContextCitation>,
    primary_hit: Option<HitMetadata>,
}

fn analyze_record(
    record: &HistoryRecord,
    context: &RecordContext,
    terms: &[String],
    filters: &SearchFilters,
) -> MatchAnalysis {
    let mut score = 0.0_f32;
    let mut why = Vec::new();
    let mut citations = Vec::new();

    if terms.is_empty() {
        if filters
            .file
            .as_ref()
            .is_some_and(|file| !file.trim().is_empty())
        {
            let mut primary_hit = None;
            for section in search_sections(record, context, filters)
                .into_iter()
                .filter(|section| section.reason == "file_touched")
            {
                if primary_hit.is_none() {
                    primary_hit = Some(section.hit.clone());
                }
                score += section.weight;
                add_match(
                    &mut why,
                    &mut citations,
                    section.reason,
                    section.citation,
                    &section.hit,
                );
            }
            if !why.is_empty() {
                return MatchAnalysis {
                    score,
                    why_matched: why,
                    citations,
                    primary_hit,
                };
            }
        }
        add_match(
            &mut why,
            &mut citations,
            "recent_activity",
            ContextCitation {
                citation_type: ContextCitationType::HistoryRecord,
                id: record.id,
                label: "recent session".to_owned(),
                time: record.updated_at,
                provider: None,
                session_id: None,
                event_seq: None,
                raw_source_path: None,
                raw_source_exists: None,
                cursor: None,
            },
            &empty_hit(record.updated_at),
        );
        return MatchAnalysis {
            score: 1.0,
            why_matched: why,
            citations,
            primary_hit: None,
        };
    }

    let mut primary_hit = None;
    let mut primary_weight = f32::MIN;
    for section in search_sections(record, context, filters) {
        if hit_matches_excluded_provider_session(&section.hit, filters) {
            continue;
        }
        if matches_terms(&section.text, terms) {
            score += section.weight;
            if section.weight > primary_weight {
                primary_weight = section.weight;
                primary_hit = Some(section.hit.clone());
            }
            add_match(
                &mut why,
                &mut citations,
                section.reason,
                section.citation,
                &section.hit,
            );
        }
    }

    MatchAnalysis {
        score,
        why_matched: why,
        citations,
        primary_hit,
    }
}

pub(crate) fn add_match(
    why: &mut Vec<String>,
    citations: &mut Vec<ContextCitation>,
    reason: &str,
    mut citation: ContextCitation,
    hit: &HitMetadata,
) {
    if !why.iter().any(|value| value == reason) {
        why.push(reason.to_owned());
    }
    citation.provider = hit.provider;
    citation.session_id = hit.session_id;
    citation.event_seq = hit.event_seq;
    citation.raw_source_path = hit.raw_source_path.clone();
    citation.raw_source_exists = hit.raw_source_exists;
    citation.cursor = hit.cursor.clone().or_else(|| {
        hit.provider_session_id
            .as_ref()
            .map(|session_id| format!("session:{session_id}"))
    });
    if !citations.iter().any(|existing| {
        existing.citation_type == citation.citation_type && existing.id == citation.id
    }) {
        citations.push(citation);
    }
}

pub(crate) fn search_sections(
    record: &HistoryRecord,
    context: &RecordContext,
    filters: &SearchFilters,
) -> Vec<SearchSection> {
    let mut sections = Vec::new();
    let record_hit = record_context_display_hit(context, filters, record.updated_at);
    let include_record_bookkeeping_text = !is_agent_history_bookkeeping_record(record);
    if include_record_bookkeeping_text {
        sections.push(SearchSection {
            reason: "title",
            weight: 8.0,
            text: record.title.clone(),
            citation: citation(
                ContextCitationType::HistoryRecord,
                record.id,
                "session title",
                record.updated_at,
            ),
            hit: record_hit.clone(),
        });
    }
    let include_record_text = include_record_bookkeeping_text
        && record_text_matches_agent_scope(context, filters)
        && !context_has_excluded_provider_session(context, filters);
    if include_record_text {
        sections.push(SearchSection {
            reason: "primary_user_message",
            weight: 5.0,
            text: record.body.clone(),
            citation: citation(
                ContextCitationType::HistoryRecord,
                record.id,
                "session text",
                record.updated_at,
            ),
            hit: record_hit.clone(),
        });
    }
    if include_record_text {
        for tag in &record.tags {
            sections.push(SearchSection {
                reason: "tag",
                weight: 3.0,
                text: tag.clone(),
                citation: citation(
                    ContextCitationType::HistoryRecord,
                    record.id,
                    "session tag",
                    record.updated_at,
                ),
                hit: record_hit.clone(),
            });
        }
    }
    for session in &context.sessions {
        if !session_matches_agent_scope(session, filters)
            || !source_id_matches_history_source_filter(session.capture_source_id, context, filters)
        {
            continue;
        }
        let hit = session_hit(session, context);
        sections.push(SearchSection {
            reason: "session_metadata",
            weight: 2.5,
            text: joined([
                session.provider.as_str(),
                session.agent_type.as_str(),
                session.status.as_str(),
                session.external_session_id.as_deref().unwrap_or_default(),
                session.external_agent_id.as_deref().unwrap_or_default(),
                session.role_hint.as_deref().unwrap_or_default(),
            ]),
            citation: citation(
                ContextCitationType::Session,
                session.id,
                "session",
                session.started_at,
            ),
            hit,
        });
    }

    for run in &context.runs {
        if !item_matches_agent_scope(run.session_id, run.source_id, context, filters) {
            continue;
        }
        let hit = run_hit(run, context);
        sections.push(SearchSection {
            reason: "run_command",
            weight: if run.exit_code.unwrap_or(0) == 0 {
                3.0
            } else {
                4.0
            },
            text: joined([
                run.run_type.as_str(),
                run.status.as_str(),
                run.cwd.as_deref().unwrap_or_default(),
                run.command_preview.as_deref().unwrap_or_default(),
            ]),
            citation: citation(
                ContextCitationType::Run,
                run.id,
                "run command",
                run.started_at,
            ),
            hit,
        });
    }

    for event in &context.events {
        if !item_matches_agent_scope(event.session_id, event.capture_source_id, context, filters) {
            continue;
        }
        let event_text = event_text(event);
        let hit = event_hit(event, context);
        sections.push(SearchSection {
            reason: match event.event_type {
                ctx_history_core::EventType::Message => "message",
                ctx_history_core::EventType::ToolCall => "tool_call",
                ctx_history_core::EventType::ToolOutput => "tool_output",
                ctx_history_core::EventType::CommandStarted
                | ctx_history_core::EventType::CommandOutput
                | ctx_history_core::EventType::CommandFinished => "command_event",
                _ => "event",
            },
            weight: event_weight(event),
            text: event_text,
            citation: citation(
                ContextCitationType::Event,
                event.id,
                "event",
                event.occurred_at,
            ),
            hit,
        });
    }

    for artifact in &context.artifacts {
        if !item_matches_agent_scope(None, artifact.source_id, context, filters) {
            continue;
        }
        let hit = artifact_hit(artifact, context);
        sections.push(SearchSection {
            reason: "artifact",
            weight: 2.5,
            text: joined([
                artifact.kind.as_str(),
                artifact.media_type.as_deref().unwrap_or_default(),
                artifact.preview_text.as_deref().unwrap_or_default(),
                artifact.blob_path.as_str(),
            ]),
            citation: citation(
                ContextCitationType::Artifact,
                artifact.id,
                "artifact",
                artifact.timestamps.updated_at,
            ),
            hit,
        });
    }

    for file in &context.files_touched {
        let session_id = file.event_id.and_then(|id| {
            context
                .events
                .iter()
                .find(|event| event.id == id)
                .and_then(|event| event.session_id)
        });
        if !item_matches_agent_scope(session_id, file.source_id, context, filters) {
            continue;
        }
        let hit = file_hit(file, context);
        sections.push(SearchSection {
            reason: "file_touched",
            weight: 3.0,
            text: file_touched_search_text(file),
            citation: citation(
                ContextCitationType::File,
                file.id,
                "file touched",
                file.timestamps.updated_at,
            ),
            hit,
        });
    }

    for change in &context.vcs_changes {
        if !item_matches_agent_scope(None, change.source_id, context, filters) {
            continue;
        }
        let parent_change_ids = change.parent_change_ids.join(" ");
        let hit = source_hit(
            change.source_id,
            change.author_time.unwrap_or(change.timestamps.updated_at),
            context,
        );
        sections.push(SearchSection {
            reason: "vcs_change",
            weight: 3.0,
            text: joined([
                change.kind.as_str(),
                change.change_id.as_str(),
                change.branch_or_bookmark.as_deref().unwrap_or_default(),
                change.tree_hash.as_deref().unwrap_or_default(),
                parent_change_ids.as_str(),
            ]),
            citation: citation(
                ContextCitationType::VcsChange,
                change.id,
                "vcs change",
                change.author_time.unwrap_or(change.timestamps.updated_at),
            ),
            hit,
        });
    }

    for summary in &context.summaries {
        if !item_matches_agent_scope(None, summary.source_id, context, filters) {
            continue;
        }
        let hit = source_hit(summary.source_id, summary.timestamps.updated_at, context);
        sections.push(SearchSection {
            reason: "summary",
            weight: 4.0,
            text: summary.text.clone(),
            citation: citation(
                ContextCitationType::Summary,
                summary.id,
                "summary",
                summary.timestamps.updated_at,
            ),
            hit,
        });
    }

    sections
}

pub(crate) fn normalize_scores(candidates: &mut [Candidate]) {
    let max_score = candidates
        .iter()
        .map(|candidate| candidate.score)
        .fold(0.0_f32, f32::max);
    if max_score <= 0.0 {
        return;
    }
    for candidate in candidates {
        candidate.score = (candidate.score / max_score).clamp(0.0, 1.0);
    }
}

use std::collections::BTreeSet;

use chrono::Utc;
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;
use work_record_core::{
    redact_share_safe_markers, AgentContextPacket, Artifact, ContextBudget, ContextCitation,
    ContextCitationType, ContextEvidence, ContextLinks, ContextPagination, ContextResult,
    ContextTruncation, Event, Evidence, EvidenceFreshness, EvidenceKind, EvidenceStatus,
    FileTouched, PullRequest, Run, Session, Summary, VcsChange, Visibility, WorkRecord,
};
use work_record_store::Store;

pub const AGENT_CONTEXT_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_MAX_TOKENS: u32 = 12_000;
pub const DEFAULT_RESULT_LIMIT: usize = 10;
pub const DEFAULT_SNIPPET_CHARS: usize = 320;
pub const DEFAULT_EVIDENCE_PER_RESULT: usize = 4;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("store error: {0}")]
    Store(#[from] work_record_store::StoreError),
}

pub type Result<T> = std::result::Result<T, SearchError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketOptions {
    pub limit: usize,
    pub max_tokens: u32,
    pub snippet_chars: usize,
    pub evidence_per_result: usize,
    pub dashboard_base_url: Option<String>,
}

impl Default for PacketOptions {
    fn default() -> Self {
        Self {
            limit: DEFAULT_RESULT_LIMIT,
            max_tokens: DEFAULT_MAX_TOKENS,
            snippet_chars: DEFAULT_SNIPPET_CHARS,
            evidence_per_result: DEFAULT_EVIDENCE_PER_RESULT,
            dashboard_base_url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchPacket {
    pub schema_version: u32,
    pub query: String,
    pub generated_at: chrono::DateTime<Utc>,
    pub results: Vec<SearchPacketResult>,
    pub pagination: ContextPagination,
    pub truncation: ContextTruncation,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchPacketResult {
    pub record_id: Uuid,
    pub title: String,
    pub snippet: String,
    pub rank: f32,
    #[serde(default)]
    pub why_matched: Vec<String>,
    #[serde(default)]
    pub citations: Vec<ContextCitation>,
    #[serde(default)]
    pub links: ContextLinks,
    #[serde(default)]
    pub visibility: Visibility,
}

#[derive(Debug, Clone)]
struct Candidate {
    record: WorkRecord,
    context: RecordContext,
    score: f32,
    why_matched: Vec<String>,
    citations: Vec<ContextCitation>,
}

#[derive(Debug, Clone, Default)]
struct RecordContext {
    evidence: Vec<Evidence>,
    sessions: Vec<Session>,
    runs: Vec<Run>,
    events: Vec<Event>,
    artifacts: Vec<Artifact>,
    files_touched: Vec<FileTouched>,
    vcs_changes: Vec<VcsChange>,
    pull_requests: Vec<PullRequest>,
    summaries: Vec<Summary>,
}

#[derive(Debug, Clone)]
struct SearchSection {
    reason: &'static str,
    weight: f32,
    text: String,
    citation: ContextCitation,
}

pub fn context_packet(
    store: &Store,
    query: Option<&str>,
    options: &PacketOptions,
) -> Result<AgentContextPacket> {
    let options = normalized_options(options);
    let candidates = ranked_candidates(store, query, &options)?;
    let mut truncation = ContextTruncation::default();
    let mut estimated_tokens = base_context_tokens(query);
    let mut omitted_evidence = 0_u32;
    let mut results = Vec::new();

    for candidate in candidates.iter().take(options.limit) {
        let safe_summary = context_summary(
            &candidate.record,
            &candidate.context,
            query.unwrap_or_default(),
            options.snippet_chars,
        );
        let evidence = context_evidence(&candidate.context.evidence, options.evidence_per_result);
        if candidate.context.evidence.len() > evidence.len() {
            omitted_evidence = omitted_evidence
                .saturating_add((candidate.context.evidence.len() - evidence.len()) as u32);
        }

        let mut result = ContextResult {
            record_id: candidate.record.id,
            title: safe_snippet(&candidate.record.title, 240),
            summary: non_empty(safe_summary),
            rank: candidate.score,
            why_matched: candidate.why_matched.clone(),
            citations: candidate.citations.clone(),
            evidence,
            links: links_for(&candidate.record, &options),
            visibility: Visibility::LocalOnly,
        };

        let result_tokens = estimate_context_result_tokens(&result);
        if estimated_tokens.saturating_add(result_tokens) > options.max_tokens {
            if result.summary.is_some() {
                result.summary = None;
                let without_summary = estimate_context_result_tokens(&result);
                if estimated_tokens.saturating_add(without_summary) <= options.max_tokens {
                    estimated_tokens = estimated_tokens.saturating_add(without_summary);
                    truncation.truncated = true;
                    truncation.reason = Some("token_budget".to_owned());
                    results.push(result);
                    continue;
                }
            }

            truncation.truncated = true;
            truncation.reason = Some("token_budget".to_owned());
            break;
        }

        estimated_tokens = estimated_tokens.saturating_add(result_tokens);
        results.push(result);
    }

    let limited_by_count = candidates.len() > results.len();
    if limited_by_count {
        truncation.omitted_results = (candidates.len() - results.len()) as u32;
        truncation.truncated = true;
        if truncation.reason.is_none() {
            truncation.reason = Some("limit".to_owned());
        }
    }
    truncation.omitted_evidence = omitted_evidence;
    if omitted_evidence > 0 {
        truncation.truncated = true;
        if truncation.reason.is_none() {
            truncation.reason = Some("evidence_limit".to_owned());
        }
    }

    let has_more = limited_by_count;
    let cursor_offset = results.len();
    Ok(AgentContextPacket {
        schema_version: AGENT_CONTEXT_SCHEMA_VERSION,
        query: query.map(str::to_owned),
        generated_at: Utc::now(),
        budget: ContextBudget {
            max_tokens: options.max_tokens,
            estimated_tokens,
        },
        results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation: Some(truncation),
    })
}

pub fn search_packet(store: &Store, query: &str, options: &PacketOptions) -> Result<SearchPacket> {
    let options = normalized_options(options);
    let candidates = ranked_candidates(store, Some(query), &options)?;
    let mut truncation = ContextTruncation::default();
    let mut results = Vec::new();

    for candidate in candidates.iter().take(options.limit) {
        results.push(SearchPacketResult {
            record_id: candidate.record.id,
            title: safe_snippet(&candidate.record.title, 240),
            snippet: search_snippet(
                &candidate.record,
                &candidate.context,
                query,
                options.snippet_chars,
            ),
            rank: candidate.score,
            why_matched: candidate.why_matched.clone(),
            citations: candidate.citations.clone(),
            links: links_for(&candidate.record, &options),
            visibility: Visibility::LocalOnly,
        });
    }

    let has_more = candidates.len() > results.len();
    if has_more {
        truncation.truncated = true;
        truncation.omitted_results = (candidates.len() - results.len()) as u32;
        truncation.reason = Some("limit".to_owned());
    }

    let cursor_offset = results.len();
    Ok(SearchPacket {
        schema_version: AGENT_CONTEXT_SCHEMA_VERSION,
        query: query.to_owned(),
        generated_at: Utc::now(),
        results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation,
    })
}

pub fn redacted_snippet(input: &str, max_chars: usize) -> String {
    safe_snippet(input, max_chars)
}

pub fn share_safe_dashboard_base_url(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty()
        || trimmed.contains('@')
        || trimmed.contains('?')
        || trimmed.contains('#')
        || trimmed.split_once("://").is_none()
    {
        return None;
    }

    let (scheme, rest) = trimmed.split_once("://")?;
    if !matches!(scheme, "http" | "https") || rest.is_empty() || rest.starts_with('/') {
        return None;
    }

    Some(trimmed.to_owned())
}

fn normalized_options(options: &PacketOptions) -> PacketOptions {
    PacketOptions {
        limit: options.limit.max(1),
        max_tokens: options.max_tokens.max(32),
        snippet_chars: options.snippet_chars.clamp(32, 2_000),
        evidence_per_result: options.evidence_per_result,
        dashboard_base_url: options
            .dashboard_base_url
            .as_deref()
            .and_then(share_safe_dashboard_base_url),
    }
}

fn ranked_candidates(
    store: &Store,
    query: Option<&str>,
    options: &PacketOptions,
) -> Result<Vec<Candidate>> {
    let fetch_limit = options.limit.saturating_mul(8).max(32);
    let mut records = Vec::<WorkRecord>::new();
    let mut seen = BTreeSet::<Uuid>::new();

    match query {
        Some(query) if !query.trim().is_empty() => {
            for record in store.search_records(query, fetch_limit)? {
                if seen.insert(record.id) {
                    records.push(record);
                }
            }
            for record in store.list_records(usize::MAX)? {
                if seen.insert(record.id) {
                    records.push(record);
                }
            }
        }
        _ => {
            records = store.list_records(fetch_limit)?;
        }
    }

    let terms = query_terms(query.unwrap_or_default());
    let mut candidates = Vec::new();
    for record in records {
        let context = hydrate_record_context(store, record.id)?;
        let analysis = analyze_record(&record, &context, &terms);
        if terms.is_empty() || analysis.score > 0.0 {
            candidates.push(Candidate {
                record,
                context,
                score: analysis.score,
                why_matched: analysis.why_matched,
                citations: analysis.citations,
            });
        }
    }

    normalize_scores(&mut candidates);
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.record.updated_at.cmp(&left.record.updated_at))
            .then_with(|| left.record.title.cmp(&right.record.title))
    });
    Ok(candidates)
}

fn hydrate_record_context(store: &Store, record_id: Uuid) -> Result<RecordContext> {
    Ok(RecordContext {
        evidence: store.evidence_for_record(record_id)?,
        sessions: store.sessions_for_record(record_id)?,
        runs: store.runs_for_record(record_id)?,
        events: store.events_for_record(record_id)?,
        artifacts: store.artifacts_for_record(record_id)?,
        files_touched: store.files_touched_for_record(record_id)?,
        vcs_changes: store.vcs_changes_for_record(record_id)?,
        pull_requests: store.pull_requests_for_record(record_id)?,
        summaries: store.summaries_for_record(record_id)?,
    })
}

struct MatchAnalysis {
    score: f32,
    why_matched: Vec<String>,
    citations: Vec<ContextCitation>,
}

fn analyze_record(record: &WorkRecord, context: &RecordContext, terms: &[String]) -> MatchAnalysis {
    let mut score = 0.0_f32;
    let mut why = Vec::new();
    let mut citations = Vec::new();

    if terms.is_empty() {
        add_match(
            &mut why,
            &mut citations,
            "recent_activity",
            ContextCitation {
                citation_type: ContextCitationType::WorkRecord,
                id: record.id,
                label: "recent work record".to_owned(),
                time: record.updated_at,
            },
        );
        return MatchAnalysis {
            score: 1.0,
            why_matched: why,
            citations,
        };
    }

    for section in search_sections(record, context) {
        if matches_terms(&section.text, terms) {
            score += section.weight;
            add_match(&mut why, &mut citations, section.reason, section.citation);
        }
    }

    MatchAnalysis {
        score,
        why_matched: why,
        citations,
    }
}

fn add_match(
    why: &mut Vec<String>,
    citations: &mut Vec<ContextCitation>,
    reason: &str,
    citation: ContextCitation,
) {
    if !why.iter().any(|value| value == reason) {
        why.push(reason.to_owned());
    }
    if !citations.iter().any(|existing| {
        existing.citation_type == citation.citation_type && existing.id == citation.id
    }) {
        citations.push(citation);
    }
}

fn search_sections(record: &WorkRecord, context: &RecordContext) -> Vec<SearchSection> {
    let mut sections = Vec::new();
    sections.push(SearchSection {
        reason: "title",
        weight: 8.0,
        text: record.title.clone(),
        citation: citation(
            ContextCitationType::WorkRecord,
            record.id,
            "work record title",
            record.updated_at,
        ),
    });
    sections.push(SearchSection {
        reason: "primary_user_message",
        weight: 5.0,
        text: record.body.clone(),
        citation: citation(
            ContextCitationType::WorkRecord,
            record.id,
            "record summary",
            record.updated_at,
        ),
    });
    for tag in &record.tags {
        sections.push(SearchSection {
            reason: "tag",
            weight: 3.0,
            text: tag.clone(),
            citation: citation(
                ContextCitationType::WorkRecord,
                record.id,
                "record tag",
                record.updated_at,
            ),
        });
    }
    if let Some(url) = &record.pr_url {
        sections.push(SearchSection {
            reason: "pr_link",
            weight: 2.0,
            text: url.clone(),
            citation: citation(
                ContextCitationType::WorkRecord,
                record.id,
                "linked pull request",
                record.updated_at,
            ),
        });
    }

    for item in &context.evidence {
        let failed = item.exit_code != 0;
        sections.push(SearchSection {
            reason: if failed {
                "failed_command"
            } else {
                "evidence_command"
            },
            weight: if failed { 4.0 } else { 2.0 },
            text: item.command.clone(),
            citation: citation(
                ContextCitationType::Evidence,
                item.id,
                "evidence command",
                item.started_at,
            ),
        });
        sections.push(SearchSection {
            reason: if failed {
                "failed_evidence_output"
            } else {
                "evidence_output"
            },
            weight: if failed { 5.0 } else { 3.0 },
            text: format!("{} {}", item.stdout, item.stderr),
            citation: citation(
                ContextCitationType::Evidence,
                item.id,
                "evidence output",
                item.started_at,
            ),
        });
    }

    for session in &context.sessions {
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
        });
    }

    for run in &context.runs {
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
        });
    }

    for event in &context.events {
        let event_text = event_text(event);
        sections.push(SearchSection {
            reason: match event.event_type {
                work_record_core::EventType::Message => "message",
                work_record_core::EventType::ToolCall => "tool_call",
                work_record_core::EventType::ToolOutput => "tool_output",
                work_record_core::EventType::CommandStarted
                | work_record_core::EventType::CommandOutput
                | work_record_core::EventType::CommandFinished => "command_event",
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
        });
    }

    for artifact in &context.artifacts {
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
        });
    }

    for file in &context.files_touched {
        sections.push(SearchSection {
            reason: "file_touched",
            weight: 3.0,
            text: joined([
                file.path.as_str(),
                file.old_path.as_deref().unwrap_or_default(),
                file.change_kind
                    .map(|kind| kind.as_str())
                    .unwrap_or_default(),
            ]),
            citation: citation(
                ContextCitationType::File,
                file.id,
                "file touched",
                file.timestamps.updated_at,
            ),
        });
    }

    for change in &context.vcs_changes {
        let parent_change_ids = change.parent_change_ids.join(" ");
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
        });
    }

    for pr in &context.pull_requests {
        sections.push(SearchSection {
            reason: "pull_request",
            weight: 3.5,
            text: joined([
                pr.url.as_str(),
                pr.title.as_deref().unwrap_or_default(),
                pr.state.as_deref().unwrap_or_default(),
                pr.head_ref.as_deref().unwrap_or_default(),
                pr.base_ref.as_deref().unwrap_or_default(),
                pr.head_sha.as_deref().unwrap_or_default(),
                pr.owner.as_deref().unwrap_or_default(),
                pr.repo.as_deref().unwrap_or_default(),
            ]),
            citation: citation(
                ContextCitationType::PullRequest,
                pr.id,
                "pull request",
                pr.timestamps.updated_at,
            ),
        });
    }

    for summary in &context.summaries {
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
        });
    }

    sections
}

fn citation(
    citation_type: ContextCitationType,
    id: Uuid,
    label: &str,
    time: chrono::DateTime<Utc>,
) -> ContextCitation {
    ContextCitation {
        citation_type,
        id,
        label: label.to_owned(),
        time,
    }
}

fn joined<const N: usize>(parts: [&str; N]) -> String {
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn event_weight(event: &Event) -> f32 {
    match event.event_type {
        work_record_core::EventType::Message => 4.0,
        work_record_core::EventType::ToolCall | work_record_core::EventType::ToolOutput => 3.5,
        work_record_core::EventType::CommandStarted
        | work_record_core::EventType::CommandOutput
        | work_record_core::EventType::CommandFinished => 3.0,
        _ => 2.0,
    }
}

fn event_text(event: &Event) -> String {
    let payload_text = json_search_text(&event.payload);
    joined([
        event.event_type.as_str(),
        event.role.map(|role| role.as_str()).unwrap_or_default(),
        payload_text.as_str(),
        event.dedupe_key.as_deref().unwrap_or_default(),
    ])
}

fn json_search_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(json_search_text)
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        serde_json::Value::Object(map) => map
            .iter()
            .flat_map(|(key, value)| [key.clone(), json_search_text(value)])
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn normalize_scores(candidates: &mut [Candidate]) {
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

fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .filter_map(|term| {
            let term = term.trim().to_lowercase();
            if term.is_empty() {
                None
            } else {
                Some(term)
            }
        })
        .collect()
}

fn matches_terms(value: &str, terms: &[String]) -> bool {
    if terms.is_empty() {
        return false;
    }
    let haystack = value.to_lowercase();
    terms.iter().all(|term| haystack.contains(term))
}

fn context_evidence(evidence: &[Evidence], limit: usize) -> Vec<ContextEvidence> {
    evidence
        .iter()
        .take(limit)
        .map(|item| ContextEvidence {
            id: item.id,
            kind: EvidenceKind::Manual,
            status: evidence_status(item.exit_code),
            freshness: EvidenceFreshness::Unbound,
        })
        .collect()
}

fn evidence_status(exit_code: i32) -> EvidenceStatus {
    if exit_code == 0 {
        EvidenceStatus::Passed
    } else {
        EvidenceStatus::Failed
    }
}

fn search_snippet(
    record: &WorkRecord,
    context: &RecordContext,
    query: &str,
    max_chars: usize,
) -> String {
    let terms = query_terms(query);
    for section in search_sections(record, context) {
        if matches_terms(&section.text, &terms) {
            return matched_snippet(&section.text, &terms, max_chars);
        }
    }
    if !record.body.trim().is_empty() {
        return safe_snippet(&record.body, max_chars);
    }
    context
        .evidence
        .iter()
        .find_map(|item| {
            if !item.stdout.trim().is_empty() {
                Some(safe_snippet(&item.stdout, max_chars))
            } else if !item.stderr.trim().is_empty() {
                Some(safe_snippet(&item.stderr, max_chars))
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn context_summary(
    record: &WorkRecord,
    context: &RecordContext,
    query: &str,
    max_chars: usize,
) -> String {
    let terms = query_terms(query);
    if terms.is_empty() || matches_terms(&record.body, &terms) {
        return safe_snippet(&record.body, max_chars);
    }
    for section in search_sections(record, context) {
        if matches_terms(&section.text, &terms) {
            return matched_snippet(&section.text, &terms, max_chars);
        }
    }
    safe_snippet(&record.body, max_chars)
}

fn matched_snippet(input: &str, terms: &[String], max_chars: usize) -> String {
    let body = input.trim();
    if body.is_empty() {
        return String::new();
    }
    let lower = body.to_lowercase();
    let start = terms
        .iter()
        .filter_map(|term| lower.find(term))
        .min()
        .unwrap_or(0);
    let start = start.saturating_sub(max_chars / 4);
    let snippet = take_chars_from(body, start, max_chars);
    safe_snippet(&snippet, max_chars)
}

fn safe_snippet(input: &str, max_chars: usize) -> String {
    let redacted = redact_share_safe_markers(input);
    truncate_chars(redacted.trim(), max_chars)
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn take_chars_from(input: &str, start: usize, max_chars: usize) -> String {
    input.chars().skip(start).take(max_chars).collect()
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn links_for(record: &WorkRecord, options: &PacketOptions) -> ContextLinks {
    ContextLinks {
        dashboard: options
            .dashboard_base_url
            .as_deref()
            .map(|base| format!("{}/records/{}", base, record.id)),
        pr: record.pr_url.as_deref().and_then(safe_external_url),
    }
}

fn safe_external_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.starts_with("https://")
        && !trimmed.contains('@')
        && !trimmed.contains('?')
        && !trimmed.contains('#')
    {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

fn pagination(cursor_base: Option<usize>, has_more: bool) -> ContextPagination {
    ContextPagination {
        cursor: if has_more {
            cursor_base.map(|value| format!("offset:{value}"))
        } else {
            None
        },
        has_more,
    }
}

fn base_context_tokens(query: Option<&str>) -> u32 {
    32_u32.saturating_add(estimate_tokens(query.unwrap_or_default()))
}

fn estimate_context_result_tokens(result: &ContextResult) -> u32 {
    let mut total = 40_u32
        .saturating_add(estimate_tokens(&result.title))
        .saturating_add(estimate_tokens(
            result.summary.as_deref().unwrap_or_default(),
        ));
    total = total.saturating_add((result.why_matched.len() as u32).saturating_mul(4));
    total = total.saturating_add((result.citations.len() as u32).saturating_mul(12));
    total = total.saturating_add((result.evidence.len() as u32).saturating_mul(8));
    total
}

fn estimate_tokens(value: &str) -> u32 {
    let chars = value.chars().count() as u32;
    chars.saturating_add(3) / 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use work_record_core::{
        AgentType, ArtifactKind, CaptureProvider, Confidence, EntityTimestamps, EventRole,
        EventType, Fidelity, FileChangeKind, PullRequestLinkSource, PullRequestProvider,
        RedactionState, RunStatus, RunType, SessionStatus, SummaryKind, SyncMetadata, SyncState,
        VcsChangeKind, VcsHost, VcsKind, VcsWorkspace, WorkRecordLink, WorkRecordLinkTargetType,
        WorkRecordLinkType,
    };

    fn tempdir() -> tempfile::TempDir {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("target/test-data");
        std::fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("work-record-search-")
            .tempdir_in(root)
            .unwrap()
    }

    fn fixed_time() -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn timestamps() -> EntityTimestamps {
        EntityTimestamps {
            created_at: fixed_time(),
            updated_at: fixed_time(),
        }
    }

    fn sync_metadata() -> SyncMetadata {
        SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: serde_json::json!({}),
        }
    }

    fn test_store() -> (tempfile::TempDir, work_record_store::Store) {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        let store = work_record_store::Store::open(path).unwrap();
        (temp, store)
    }

    #[test]
    fn redacts_secret_like_values_in_snippets() {
        let snippet = redacted_snippet(
            "token=ghp_1234567890abcdef1234567890abcdef and password=hunter2",
            200,
        );

        assert!(snippet.contains("token=[REDACTED_SECRET]"));
        assert!(snippet.contains("password=[REDACTED_SECRET]"));
        assert!(!snippet.contains("ghp_123456"));
        assert!(!snippet.contains("hunter2"));
    }

    #[test]
    fn dashboard_base_url_must_be_share_safe() {
        assert_eq!(
            share_safe_dashboard_base_url(" http://127.0.0.1:3000/ "),
            Some("http://127.0.0.1:3000".to_owned())
        );
        assert_eq!(share_safe_dashboard_base_url("file:///tmp/ctx"), None);
        assert_eq!(
            share_safe_dashboard_base_url("https://token@example.test"),
            None
        );
        assert_eq!(
            share_safe_dashboard_base_url("https://example.test?q=secret"),
            None
        );
    }

    #[test]
    fn rich_search_matches_typed_context_with_citations_and_redaction() {
        let (_temp, store) = test_store();
        let mut record = WorkRecord::new(
            "Plain work",
            "ordinary body without the query",
            vec!["needle-tag".into()],
            "task",
            None,
        );
        record.pr_url = Some("https://github.com/ctxrs/ctx/pull/44".into());
        store.insert_record(&record).unwrap();

        let evidence = Evidence::new(
            Some(record.id),
            "cargo test needle-command",
            1,
            "needle-output token=ghp_1234567890abcdef1234567890abcdef".into(),
            "password=hunter2".into(),
            fixed_time(),
            50,
        );
        store.insert_evidence(&evidence).unwrap();

        let artifact = Artifact {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000201").unwrap(),
            kind: ArtifactKind::Markdown,
            blob_hash: "hash-rich-search-artifact".into(),
            blob_path: "blobs/rich-search-artifact".into(),
            byte_size: 32,
            media_type: Some("text/markdown".into()),
            preview_text: Some("needle-artifact /home/daddy/private/repo".into()),
            redaction_state: RedactionState::SafePreview,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_artifact(&artifact).unwrap();

        let session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000202").unwrap(),
            work_record_id: Some(record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("needle-session".into()),
            external_agent_id: Some("agent-needle".into()),
            agent_type: AgentType::Primary,
            role_hint: Some("needle-role".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&session).unwrap();

        let run = Run {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000203").unwrap(),
            work_record_id: Some(record.id),
            session_id: Some(session.id),
            run_type: RunType::Command,
            status: RunStatus::Failed,
            started_at: fixed_time(),
            ended_at: Some(fixed_time()),
            exit_code: Some(1),
            cwd: Some("/home/daddy/private/repo".into()),
            command_preview: Some("cargo test needle-run password=hunter2".into()),
            input_blob_id: None,
            output_blob_id: Some(artifact.id),
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_run(&run).unwrap();

        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000204").unwrap(),
            seq: 1,
            work_record_id: Some(record.id),
            session_id: Some(session.id),
            run_id: Some(run.id),
            event_type: EventType::ToolCall,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({
                "tool": "shell",
                "arguments": "needle-event token=secretvalue"
            }),
            payload_blob_id: None,
            dedupe_key: Some("needle-dedupe".into()),
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };
        store.upsert_event(&event).unwrap();

        let workspace = VcsWorkspace {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000205").unwrap(),
            kind: VcsKind::Git,
            root_path: "/repo".into(),
            repo_fingerprint: "git:needle".into(),
            primary_remote_url_normalized: Some("https://github.com/ctxrs/ctx".into()),
            host: VcsHost::Github,
            owner: Some("ctxrs".into()),
            name: Some("ctx".into()),
            monorepo_subpath: None,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        let workspace_id = store.upsert_vcs_workspace(&workspace).unwrap();

        let change = VcsChange {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000206").unwrap(),
            vcs_workspace_id: workspace_id,
            kind: VcsChangeKind::GitCommit,
            change_id: "needle-change".into(),
            parent_change_ids: vec!["parent".into()],
            branch_or_bookmark: Some("ctx/needle-branch".into()),
            tree_hash: Some("tree".into()),
            author_time: Some(fixed_time()),
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_vcs_change(&change).unwrap();

        let pr = PullRequest {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000207").unwrap(),
            vcs_workspace_id: Some(workspace_id),
            provider: PullRequestProvider::Github,
            url: "https://github.com/ctxrs/ctx/pull/77".into(),
            number: Some(77),
            owner: Some("ctxrs".into()),
            repo: Some("ctx".into()),
            title: Some("needle pull request".into()),
            state: Some("open".into()),
            head_ref: Some("ctx/needle-pr".into()),
            base_ref: Some("main".into()),
            head_sha: Some("needle-sha".into()),
            confidence: Confidence::Explicit,
            link_source: PullRequestLinkSource::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_pull_request(&pr).unwrap();

        let file = FileTouched {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000208").unwrap(),
            work_record_id: Some(record.id),
            run_id: Some(run.id),
            event_id: Some(event.id),
            vcs_workspace_id: Some(workspace_id),
            path: "crates/work-record-search/src/needle_file.rs".into(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some(12),
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_file_touched(&file).unwrap();

        let summary = Summary {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000209").unwrap(),
            work_record_id: Some(record.id),
            session_id: Some(session.id),
            kind: SummaryKind::ImportedProviderSummary,
            model_or_source: Some("codex".into()),
            text: "needle summary password=hunter2".into(),
            citations: Vec::new(),
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        store.upsert_summary(&summary).unwrap();

        for (target_type, target_id, link_type) in [
            (
                WorkRecordLinkTargetType::VcsChange,
                change.id,
                WorkRecordLinkType::References,
            ),
            (
                WorkRecordLinkTargetType::PullRequest,
                pr.id,
                WorkRecordLinkType::PublishedTo,
            ),
            (
                WorkRecordLinkTargetType::Artifact,
                artifact.id,
                WorkRecordLinkType::Produced,
            ),
        ] {
            store
                .upsert_work_record_link(&WorkRecordLink {
                    id: new_link_id(target_id),
                    work_record_id: record.id,
                    target_type,
                    target_id,
                    link_type,
                    confidence: Confidence::Explicit,
                    source_id: None,
                    timestamps: timestamps(),
                    sync: sync_metadata(),
                })
                .unwrap();
        }

        let packet = search_packet(
            &store,
            "needle",
            &PacketOptions {
                limit: 5,
                snippet_chars: 600,
                ..PacketOptions::default()
            },
        )
        .unwrap();

        assert_eq!(packet.results.len(), 1);
        let result = &packet.results[0];
        for reason in [
            "tag",
            "failed_command",
            "failed_evidence_output",
            "session_metadata",
            "run_command",
            "tool_call",
            "artifact",
            "file_touched",
            "vcs_change",
            "pull_request",
            "summary",
        ] {
            assert!(
                result.why_matched.iter().any(|value| value == reason),
                "missing why_matched reason {reason}: {:?}",
                result.why_matched
            );
        }

        for citation_type in [
            ContextCitationType::WorkRecord,
            ContextCitationType::Evidence,
            ContextCitationType::Session,
            ContextCitationType::Run,
            ContextCitationType::Event,
            ContextCitationType::Artifact,
            ContextCitationType::File,
            ContextCitationType::VcsChange,
            ContextCitationType::PullRequest,
            ContextCitationType::Summary,
        ] {
            assert!(
                result
                    .citations
                    .iter()
                    .any(|citation| citation.citation_type == citation_type),
                "missing citation type {citation_type:?}: {:?}",
                result.citations
            );
        }

        assert_eq!(result.visibility, Visibility::LocalOnly);

        let secret_packet = search_packet(
            &store,
            "hunter2",
            &PacketOptions {
                limit: 1,
                snippet_chars: 600,
                ..PacketOptions::default()
            },
        )
        .unwrap();
        let secret_snippet = &secret_packet.results[0].snippet;
        assert!(secret_snippet.contains("[REDACTED_SECRET]"));
        assert!(!secret_snippet.contains("hunter2"));
        assert!(!secret_snippet.contains("ghp_123456"));
    }

    fn new_link_id(target_id: Uuid) -> Uuid {
        let mut bytes = *target_id.as_bytes();
        bytes[15] = bytes[15].wrapping_add(80);
        Uuid::from_bytes(bytes)
    }

    #[test]
    fn context_packet_budget_is_deterministic_for_large_history() {
        let (_temp, store) = test_store();
        for index in 0..64 {
            let record = WorkRecord::new(
                format!("Budget record {index:03}"),
                format!(
                    "needle password=hunter2 deterministic body {index:03} {}",
                    "detail ".repeat(24)
                ),
                vec!["budget".into()],
                "task",
                None,
            );
            store.insert_record(&record).unwrap();
        }

        let packet = context_packet(
            &store,
            Some("needle"),
            &PacketOptions {
                limit: 40,
                max_tokens: 260,
                snippet_chars: 160,
                evidence_per_result: 1,
                dashboard_base_url: None,
            },
        )
        .unwrap();

        assert!(packet.results.len() < 40);
        assert!(packet.budget.estimated_tokens <= packet.budget.max_tokens);
        let truncation = packet.truncation.as_ref().unwrap();
        assert!(truncation.truncated);
        assert_eq!(truncation.reason.as_deref(), Some("token_budget"));
        assert!(truncation.omitted_results > 0);
        let serialized = serde_json::to_string(&packet).unwrap();
        assert!(serialized.contains("[REDACTED_SECRET]"));
        assert!(!serialized.contains("hunter2"));
    }
}

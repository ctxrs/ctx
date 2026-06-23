use std::collections::BTreeSet;

use chrono::Utc;
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;
use work_record_core::{
    redact_share_safe_markers, AgentContextPacket, ContextBudget, ContextCitation,
    ContextCitationType, ContextEvidence, ContextLinks, ContextPagination, ContextResult,
    ContextTruncation, Evidence, EvidenceFreshness, EvidenceKind, EvidenceStatus, Visibility,
    WorkRecord,
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
    evidence: Vec<Evidence>,
    score: f32,
    why_matched: Vec<String>,
    citations: Vec<ContextCitation>,
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
            &candidate.evidence,
            query.unwrap_or_default(),
            options.snippet_chars,
        );
        let evidence = context_evidence(&candidate.evidence, options.evidence_per_result);
        if candidate.evidence.len() > evidence.len() {
            omitted_evidence =
                omitted_evidence.saturating_add((candidate.evidence.len() - evidence.len()) as u32);
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
                &candidate.evidence,
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
            for record in store.list_records(fetch_limit)? {
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
        let evidence = store.evidence_for_record(record.id)?;
        let analysis = analyze_record(&record, &evidence, &terms);
        if terms.is_empty() || analysis.score > 0.0 {
            candidates.push(Candidate {
                record,
                evidence,
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

struct MatchAnalysis {
    score: f32,
    why_matched: Vec<String>,
    citations: Vec<ContextCitation>,
}

fn analyze_record(record: &WorkRecord, evidence: &[Evidence], terms: &[String]) -> MatchAnalysis {
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

    if matches_terms(&record.title, terms) {
        score += 8.0;
        add_match(
            &mut why,
            &mut citations,
            "title",
            ContextCitation {
                citation_type: ContextCitationType::WorkRecord,
                id: record.id,
                label: "work record title".to_owned(),
                time: record.updated_at,
            },
        );
    }

    if matches_terms(&record.body, terms) {
        score += 5.0;
        add_match(
            &mut why,
            &mut citations,
            "primary_user_message",
            ContextCitation {
                citation_type: ContextCitationType::WorkRecord,
                id: record.id,
                label: "record summary".to_owned(),
                time: record.updated_at,
            },
        );
    }

    if record.tags.iter().any(|tag| matches_terms(tag, terms)) {
        score += 3.0;
        add_match(
            &mut why,
            &mut citations,
            "tag",
            ContextCitation {
                citation_type: ContextCitationType::WorkRecord,
                id: record.id,
                label: "record tag".to_owned(),
                time: record.updated_at,
            },
        );
    }

    if record
        .pr_url
        .as_deref()
        .map(|url| matches_terms(url, terms))
        .unwrap_or(false)
    {
        score += 2.0;
        add_match(
            &mut why,
            &mut citations,
            "pr_link",
            ContextCitation {
                citation_type: ContextCitationType::WorkRecord,
                id: record.id,
                label: "linked pull request".to_owned(),
                time: record.updated_at,
            },
        );
    }

    for item in evidence {
        if matches_terms(&item.command, terms) {
            score += if item.exit_code == 0 { 2.0 } else { 4.0 };
            add_match(
                &mut why,
                &mut citations,
                if item.exit_code == 0 {
                    "evidence_command"
                } else {
                    "failed_command"
                },
                ContextCitation {
                    citation_type: ContextCitationType::Evidence,
                    id: item.id,
                    label: "evidence command".to_owned(),
                    time: item.started_at,
                },
            );
        }
        if matches_terms(&item.stdout, terms) || matches_terms(&item.stderr, terms) {
            score += if item.exit_code == 0 { 3.0 } else { 5.0 };
            add_match(
                &mut why,
                &mut citations,
                if item.exit_code == 0 {
                    "evidence_output"
                } else {
                    "failed_evidence_output"
                },
                ContextCitation {
                    citation_type: ContextCitationType::Evidence,
                    id: item.id,
                    label: "evidence output".to_owned(),
                    time: item.started_at,
                },
            );
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
    evidence: &[Evidence],
    query: &str,
    max_chars: usize,
) -> String {
    let terms = query_terms(query);
    if matches_terms(&record.body, &terms) {
        return matched_snippet(&record.body, &terms, max_chars);
    }
    for item in evidence {
        if matches_terms(&item.stdout, &terms) {
            return matched_snippet(&item.stdout, &terms, max_chars);
        }
        if matches_terms(&item.stderr, &terms) {
            return matched_snippet(&item.stderr, &terms, max_chars);
        }
        if matches_terms(&item.command, &terms) {
            return matched_snippet(&item.command, &terms, max_chars);
        }
    }
    if !record.body.trim().is_empty() {
        return safe_snippet(&record.body, max_chars);
    }
    evidence
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
    evidence: &[Evidence],
    query: &str,
    max_chars: usize,
) -> String {
    let terms = query_terms(query);
    if terms.is_empty() || matches_terms(&record.body, &terms) {
        return safe_snippet(&record.body, max_chars);
    }
    for item in evidence {
        if matches_terms(&item.stdout, &terms) {
            return matched_snippet(&item.stdout, &terms, max_chars);
        }
        if matches_terms(&item.stderr, &terms) {
            return matched_snippet(&item.stderr, &terms, max_chars);
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

    #[test]
    fn redacts_secret_like_values_in_snippets() {
        let snippet = redacted_snippet(
            "token=ghp_1234567890abcdef1234567890abcdef and password=hunter2",
            200,
        );

        assert!(snippet.contains("token=[redacted]"));
        assert!(snippet.contains("password=[redacted]"));
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
}

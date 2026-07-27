use std::collections::BTreeSet;

use ctx_history_core::EventType;
use serde_json::{json, Value};

use super::value::provider_local_preview;

pub(super) const MAX_RESULT_EVIDENCE_SCAN_CHARS: usize = 64 * 1024;
pub(super) const MAX_RESULT_EVIDENCE_IDENTIFIERS: usize = 32;
pub(super) const MAX_RESULT_EVIDENCE_CALL_ID_CHARS: usize = 256;

/// Extracts only bounded, allowlisted artifact identifiers from successful output.
/// Arbitrary stdout remains absent from both the canonical payload and local FTS.
pub(crate) fn provider_result_identifier_evidence(
    event_type: EventType,
    text: &str,
    body: &Value,
) -> Value {
    if !matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
        return Value::Null;
    }
    let mut identifiers = BTreeSet::<(String, String)>::new();
    provider_collect_result_call_ids(body, &mut identifiers);
    if provider_output_event_is_failure(body) {
        return result_identifiers_value(identifiers);
    }
    let mut candidates = Vec::new();
    let (bounded, _) = provider_local_preview(text, MAX_RESULT_EVIDENCE_SCAN_CHARS);
    candidates.push(bounded);
    let mut remaining =
        MAX_RESULT_EVIDENCE_SCAN_CHARS.saturating_sub(candidates[0].chars().count());
    provider_collect_result_strings(body, &mut remaining, &mut candidates);
    for line in candidates.iter().flat_map(|candidate| candidate.lines()) {
        let commit_context = line.to_ascii_lowercase().contains("commit");
        if identifiers.len() < MAX_RESULT_EVIDENCE_IDENTIFIERS {
            if let Some(identifier) = provider_git_commit_summary_id(line) {
                identifiers.insert(("git_commit_summary_id".to_owned(), identifier));
            }
        }
        for raw in line.split_whitespace() {
            let token = raw.trim_matches(|ch: char| {
                matches!(
                    ch,
                    '[' | ']' | '(' | ')' | '{' | '}' | ',' | ';' | ':' | '\'' | '"'
                )
            });
            let hex = token.trim_end_matches(['.', '/']);
            if (hex.len() == 40 || hex.len() == 64)
                && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                identifiers.insert(("git_oid".to_owned(), hex.to_ascii_lowercase()));
            } else if commit_context
                && (7..=12).contains(&hex.len())
                && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                identifiers.insert(("git_abbrev_oid".to_owned(), hex.to_ascii_lowercase()));
            } else if let Some(url) = provider_forge_artifact_url(token) {
                identifiers.insert(("forge_url".to_owned(), url));
            }
            if identifiers.len() >= MAX_RESULT_EVIDENCE_IDENTIFIERS {
                break;
            }
        }
        if identifiers.len() >= MAX_RESULT_EVIDENCE_IDENTIFIERS {
            break;
        }
    }
    let commit_summaries = identifiers
        .iter()
        .filter_map(|(kind, value)| (kind == "git_commit_summary_id").then_some(value.clone()))
        .collect::<BTreeSet<_>>();
    identifiers.retain(|(kind, value)| {
        !matches!(kind.as_str(), "git_oid" | "git_abbrev_oid") || !commit_summaries.contains(value)
    });
    result_identifiers_value(identifiers)
}

fn result_identifiers_value(identifiers: BTreeSet<(String, String)>) -> Value {
    if identifiers.is_empty() {
        return Value::Null;
    }
    Value::Array(
        identifiers
            .into_iter()
            .map(|(kind, value)| json!({ "kind": kind, "value": value }))
            .collect(),
    )
}

fn provider_git_commit_summary_id(line: &str) -> Option<String> {
    let summary = line.trim_start().strip_prefix('[')?.split_once(']')?.0;
    let mut tokens = summary.split_ascii_whitespace();
    let candidate = tokens.next_back()?;
    if tokens.next().is_none()
        || !(7..=64).contains(&candidate.len())
        || !candidate.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(candidate.to_ascii_lowercase())
}

fn provider_collect_result_call_ids(value: &Value, identifiers: &mut BTreeSet<(String, String)>) {
    if identifiers.len() >= MAX_RESULT_EVIDENCE_IDENTIFIERS {
        return;
    }
    match value {
        Value::Array(values) => {
            for value in values {
                provider_collect_result_call_ids(value, identifiers);
                if identifiers.len() >= MAX_RESULT_EVIDENCE_IDENTIFIERS {
                    break;
                }
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                let normalized = provider_normalized_key(key);
                if matches!(
                    normalized.as_str(),
                    "callid" | "toolcallid" | "toolresultid" | "tooluseid"
                ) {
                    if let Some(call_id) = value.as_str().and_then(provider_bounded_result_call_id)
                    {
                        identifiers.insert(("call_id".to_owned(), call_id.to_owned()));
                    }
                }
                provider_collect_result_call_ids(value, identifiers);
                if identifiers.len() >= MAX_RESULT_EVIDENCE_IDENTIFIERS {
                    break;
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn provider_bounded_result_call_id(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.len() <= MAX_RESULT_EVIDENCE_CALL_ID_CHARS
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        }))
    .then_some(value)
}

fn provider_collect_result_strings(value: &Value, remaining: &mut usize, output: &mut Vec<String>) {
    if *remaining == 0 {
        return;
    }
    match value {
        Value::String(text) => {
            let (bounded, _) = provider_local_preview(text, *remaining);
            *remaining = remaining.saturating_sub(bounded.chars().count());
            output.push(bounded);
        }
        Value::Array(values) => {
            for value in values {
                provider_collect_result_strings(value, remaining, output);
                if *remaining == 0 {
                    break;
                }
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                provider_collect_result_strings(value, remaining, output);
                if *remaining == 0 {
                    break;
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn provider_forge_artifact_url(token: &str) -> Option<String> {
    let candidate = token
        .trim_end_matches(['.', ',', ';', ')', ']', '}'])
        .split(['?', '#'])
        .next()?;
    if !candidate.starts_with("https://") || candidate.len() > 512 {
        return None;
    }
    let lower = candidate.to_ascii_lowercase();
    let is_known_host = lower.starts_with("https://github.com/")
        || lower.starts_with("https://gitlab.com/")
        || lower.starts_with("https://bitbucket.org/")
        || lower.starts_with("https://codeberg.org/");
    let is_artifact = [
        "/pull/",
        "/pulls/",
        "/issues/",
        "/merge_requests/",
        "/commits/",
    ]
    .iter()
    .any(|segment| lower.contains(segment));
    (is_known_host && is_artifact).then(|| candidate.to_owned())
}

pub(crate) fn provider_output_event_is_failure(body: &Value) -> bool {
    match body {
        Value::Object(object) => {
            provider_output_object_indicates_failure(object)
                || object.values().any(provider_output_event_is_failure)
        }
        Value::Array(items) => items.iter().any(provider_output_event_is_failure),
        _ => false,
    }
}

fn provider_output_object_indicates_failure(object: &serde_json::Map<String, Value>) -> bool {
    object
        .get("timed_out")
        .or_else(|| object.get("timedOut"))
        .or_else(|| object.get("timeout"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || object
            .get("success")
            .and_then(Value::as_bool)
            .is_some_and(|success| !success)
        || object
            .get("isError")
            .or_else(|| object.get("is_error"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || ["exit_code", "exitCode"].iter().any(|key| {
            object
                .get(*key)
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
        })
        || ["status_code", "statusCode"].iter().any(|key| {
            object
                .get(*key)
                .and_then(Value::as_i64)
                .is_some_and(|code| code >= 400)
        })
        || ["status", "state", "outcome"].iter().any(|key| {
            object
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(provider_status_text_is_failure)
        })
        || object
            .get("error")
            .is_some_and(provider_error_value_indicates_failure)
}

fn provider_status_text_is_failure(status: &str) -> bool {
    let status = status.trim().to_ascii_lowercase();
    matches!(
        status.as_str(),
        "failed"
            | "failure"
            | "error"
            | "errored"
            | "timeout"
            | "timed_out"
            | "timedout"
            | "cancelled"
            | "canceled"
    )
}

fn provider_status_text_is_success(status: &str) -> bool {
    let status = status.trim().to_ascii_lowercase();
    matches!(
        status.as_str(),
        "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
    )
}

fn provider_error_value_indicates_failure(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Number(value) => value.as_i64().is_some_and(|number| number != 0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

fn provider_normalized_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

const MAX_RESULT_OUTCOME_NODES: usize = 4_096;

#[derive(Default)]
struct ProviderResultOutcome {
    success: bool,
    failure: bool,
    exhausted: bool,
}

pub(crate) fn provider_result_outcome_evidence(event_type: EventType, body: &Value) -> Value {
    if !matches!(
        event_type,
        EventType::ToolOutput | EventType::CommandOutput | EventType::CommandFinished
    ) {
        return Value::Null;
    }
    let mut outcome = ProviderResultOutcome::default();
    let mut remaining = MAX_RESULT_OUTCOME_NODES;
    provider_collect_result_outcome(body, &mut remaining, &mut outcome);
    match (outcome.success, outcome.failure, outcome.exhausted) {
        (true, false, false) => Value::String("success".to_owned()),
        (false, true, false) => Value::String("failure".to_owned()),
        _ => Value::Null,
    }
}

fn provider_collect_result_outcome(
    value: &Value,
    remaining: &mut usize,
    outcome: &mut ProviderResultOutcome,
) {
    if *remaining == 0 {
        outcome.exhausted = true;
        return;
    }
    *remaining -= 1;
    match value {
        Value::Array(values) => {
            for value in values {
                provider_collect_result_outcome(value, remaining, outcome);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                let normalized = provider_normalized_key(key);
                if normalized == "exitcode" {
                    if let Some(code) = value.as_i64() {
                        outcome.success |= code == 0;
                        outcome.failure |= code != 0;
                    }
                } else if normalized == "statuscode" {
                    if let Some(code) = value.as_i64() {
                        outcome.success |= (200..400).contains(&code);
                        outcome.failure |= code >= 400;
                    }
                } else if matches!(normalized.as_str(), "success" | "ok") {
                    if let Some(success) = value.as_bool() {
                        outcome.success |= success;
                        outcome.failure |= !success;
                    }
                } else if matches!(normalized.as_str(), "iserror" | "timedout" | "timeout") {
                    outcome.failure |= value.as_bool().unwrap_or(false);
                } else if matches!(normalized.as_str(), "status" | "state" | "outcome") {
                    if let Some(status) = value.as_str() {
                        outcome.success |= provider_status_text_is_success(status);
                        outcome.failure |= provider_status_text_is_failure(status);
                    }
                } else if normalized == "error" {
                    outcome.failure |= provider_error_value_indicates_failure(value);
                }
                provider_collect_result_outcome(value, remaining, outcome);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

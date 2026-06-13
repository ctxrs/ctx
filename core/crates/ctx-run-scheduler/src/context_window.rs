use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub fn compute_context_window_metrics(
    provider_id: &str,
    model_id: &str,
    prompt: &str,
) -> Option<serde_json::Value> {
    let context_window_tokens = model_context_window(provider_id, model_id)?;
    let context_tokens_estimate = estimate_tokens(prompt);
    let remaining_tokens_estimate = context_window_tokens.saturating_sub(context_tokens_estimate);
    let remaining_fraction = remaining_tokens_estimate as f64 / context_window_tokens as f64;
    Some(json!({
        "context_tokens_estimate": context_tokens_estimate,
        "context_window_tokens": context_window_tokens,
        "remaining_tokens_estimate": remaining_tokens_estimate,
        "remaining_fraction": remaining_fraction,
    }))
}

pub fn read_codex_context_window_metrics(
    codex_home: &Path,
    session_ref: &str,
) -> Option<serde_json::Value> {
    let path = find_codex_session_log(codex_home, session_ref)?;
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut latest_info: Option<Value> = None;

    for line in reader.lines().map_while(std::result::Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let payload: Value = serde_json::from_str(trimmed).ok()?;
        if payload.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let event_payload = payload.get("payload")?;
        if event_payload.get("type").and_then(Value::as_str) != Some("token_count") {
            continue;
        }
        if let Some(info) = event_payload.get("info") {
            latest_info = Some(info.clone());
        }
    }

    let info = latest_info?;
    let context_window_tokens = info.get("model_context_window").and_then(Value::as_u64)?;
    let last_usage = info.get("last_token_usage").and_then(Value::as_object);
    let input_tokens = last_usage
        .and_then(|m| m.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = last_usage
        .and_then(|m| m.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning_tokens = last_usage
        .and_then(|m| m.get("reasoning_output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = last_usage
        .and_then(|m| m.get("total_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(
            input_tokens
                .saturating_add(output_tokens)
                .saturating_add(reasoning_tokens),
        );

    if context_window_tokens == 0 {
        return None;
    }
    let remaining_tokens_estimate = context_window_tokens.saturating_sub(total_tokens);
    let remaining_fraction = remaining_tokens_estimate as f64 / context_window_tokens as f64;

    Some(json!({
        "context_tokens_estimate": total_tokens,
        "context_window_tokens": context_window_tokens,
        "remaining_tokens_estimate": remaining_tokens_estimate,
        "remaining_fraction": remaining_fraction,
        "total_input_tokens": input_tokens,
        "total_output_tokens": output_tokens.saturating_add(reasoning_tokens),
    }))
}

fn find_codex_session_log(codex_home: &Path, session_ref: &str) -> Option<PathBuf> {
    let root = codex_home.join("sessions");
    if !root.exists() {
        return None;
    }
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".jsonl") && name.contains(session_ref) {
                return Some(path);
            }
        }
    }
    None
}

fn model_context_window(provider_id: &str, model_id: &str) -> Option<usize> {
    match (provider_id, model_id) {
        ("fake", "fake-model") => Some(100),
        _ => None,
    }
}

fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    chars.div_ceil(4)
}

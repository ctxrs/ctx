use super::*;
use serde_json::{Map, Value};

const REDACTED_ABSOLUTE_PATH: &str = "[redacted:absolute_path]";
const REDACTED_PROVIDER_REF: &str = "[redacted:provider_ref]";
const REDACTED_PTY_STREAM: &str = "[redacted:pty_stream]";
const REDACTED_SECRET: &str = "[redacted:secret]";

pub fn normalize_archive_json(value: &Value) -> NormalizedArchivePayload {
    let mut stats = RunArchiveNormalizationStats::default();
    let value = normalize_value(value, &mut stats);
    NormalizedArchivePayload { value, stats }
}

pub fn normalize_archive_text(value: &str) -> NormalizedArchiveText {
    let mut stats = RunArchiveNormalizationStats::default();
    let text = normalize_string(value, &mut stats);
    NormalizedArchiveText { text, stats }
}

pub fn normalize_session_event_for_archive(
    event: &SessionEvent,
    scope: RunArchiveIngestScope,
) -> Option<(RunArchiveIngestSessionEvent, RunArchiveNormalizationStats)> {
    let run_id = event.run_id?;
    if event.transient
        || should_drop_session_event_for_archive(&event.event_type, &event.payload_json)
    {
        let stats = RunArchiveNormalizationStats {
            dropped_transient_events: 1,
            ..RunArchiveNormalizationStats::default()
        };
        return Some((
            RunArchiveIngestSessionEvent {
                seq: event.seq,
                id: event.id,
                session_id: event.session_id,
                run_id,
                turn_id: event.turn_id,
                event_type: session_event_type_key(&event.event_type).to_string(),
                payload_json: None,
                payload_omitted_reason: Some("transient_or_stream_payload".to_string()),
                created_at: event.created_at,
            },
            stats,
        ));
    }

    let mut stats = RunArchiveNormalizationStats::default();
    let (payload_json, payload_omitted_reason) = if scope.includes_evidence_payloads() {
        let normalized = normalize_archive_json(&event.payload_json);
        stats.merge(normalized.stats);
        (Some(normalized.value), None)
    } else {
        stats.omitted_content_payloads += 1;
        (None, Some("scope_omits_event_payload".to_string()))
    };

    Some((
        RunArchiveIngestSessionEvent {
            seq: event.seq,
            id: event.id,
            session_id: event.session_id,
            run_id,
            turn_id: event.turn_id,
            event_type: session_event_type_key(&event.event_type).to_string(),
            payload_json,
            payload_omitted_reason,
            created_at: event.created_at,
        },
        stats,
    ))
}

pub fn session_event_type_key(event_type: &SessionEventType) -> &'static str {
    match event_type {
        SessionEventType::Init => "init",
        SessionEventType::UserMessage => "user_message",
        SessionEventType::InputQueued => "input_queued",
        SessionEventType::TurnQueued => "turn_queued",
        SessionEventType::TurnStarted => "turn_started",
        SessionEventType::ContextWindowUpdate => "context_window_update",
        SessionEventType::TurnFinished => "turn_finished",
        SessionEventType::AuthRequired => "auth_required",
        SessionEventType::Notice => "notice",
        SessionEventType::AssistantChunk => "assistant_chunk",
        SessionEventType::ThoughtChunk => "thought_chunk",
        SessionEventType::AssistantComplete => "assistant_complete",
        SessionEventType::AssistantMessageInserted => "assistant_message_inserted",
        SessionEventType::ToolCall => "tool_call",
        SessionEventType::ToolCallUpdate => "tool_call_update",
        SessionEventType::ToolResult => "tool_result",
        SessionEventType::Plan => "plan",
        SessionEventType::ArtifactsSet => "artifacts_set",
        SessionEventType::Done => "done",
        SessionEventType::InterruptRequested => "interrupt_requested",
        SessionEventType::TurnInterrupted => "turn_interrupted",
        SessionEventType::MessageQueueAdded => "message_queue_added",
        SessionEventType::MessageQueueUpdated => "message_queue_updated",
        SessionEventType::MessageQueueRemoved => "message_queue_removed",
        SessionEventType::MessageQueuePromoted => "message_queue_promoted",
        SessionEventType::Error => "error",
    }
}

fn normalize_value(value: &Value, stats: &mut RunArchiveNormalizationStats) -> Value {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(text) => Value::String(normalize_string(text, stats)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| normalize_value(item, stats))
                .collect(),
        ),
        Value::Object(object) => normalize_object(object, stats),
    }
}

fn normalize_object(
    object: &Map<String, Value>,
    stats: &mut RunArchiveNormalizationStats,
) -> Value {
    let mut normalized = Map::with_capacity(object.len());
    for (key, value) in object {
        if is_provider_ref_key(key) {
            stats.redacted_provider_refs += 1;
            normalized.insert(
                key.clone(),
                Value::String(REDACTED_PROVIDER_REF.to_string()),
            );
            continue;
        }
        if is_pty_stream_key(key) {
            stats.redacted_pty_streams += 1;
            normalized.insert(key.clone(), Value::String(REDACTED_PTY_STREAM.to_string()));
            continue;
        }
        if is_sensitive_key(key) {
            stats.redacted_secret_fields += 1;
            normalized.insert(key.clone(), Value::String(REDACTED_SECRET.to_string()));
            continue;
        }
        normalized.insert(key.clone(), normalize_value(value, stats));
    }
    Value::Object(normalized)
}

fn normalize_string(value: &str, stats: &mut RunArchiveNormalizationStats) -> String {
    let redacted_paths = redact_absolute_paths(value, stats);
    redact_secret_values(&redacted_paths, stats)
}

fn should_drop_session_event_for_archive(
    event_type: &SessionEventType,
    payload_json: &Value,
) -> bool {
    if matches!(
        event_type,
        SessionEventType::AssistantChunk
            | SessionEventType::ThoughtChunk
            | SessionEventType::ToolCallUpdate
    ) {
        return true;
    }
    if payload_json
        .get("crp_channel")
        .or_else(|| payload_json.get("crpChannel"))
        .and_then(Value::as_str)
        .is_some_and(|channel| channel == "data")
    {
        return true;
    }
    if payload_json
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| {
            let normalized = normalize_key(kind);
            normalized.contains("pty") || normalized.contains("terminalstream")
        })
    {
        return true;
    }
    false
}

fn is_provider_ref_key(key: &str) -> bool {
    matches!(
        normalize_key(key).as_str(),
        "providersessionref"
            | "providersessionid"
            | "providerref"
            | "providerresponseid"
            | "providerthreadid"
            | "nativeconversationid"
            | "nativeproviderref"
    )
}

fn is_pty_stream_key(key: &str) -> bool {
    let normalized = normalize_key(key);
    normalized.contains("pty")
        && (normalized.contains("bytes")
            || normalized.contains("stream")
            || normalized.contains("data")
            || normalized.contains("output"))
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = normalize_key(key);
    normalized == "token"
        || normalized.ends_with("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("apikey")
        || normalized.contains("authorization")
        || normalized.contains("credential")
        || normalized.contains("privatekey")
        || normalized == "cookie"
        || normalized.ends_with("cookie")
}

fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn redact_absolute_paths(value: &str, stats: &mut RunArchiveNormalizationStats) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if is_unix_absolute_path(bytes, index)
            || is_windows_absolute_path(bytes, index)
            || is_unc_absolute_path(bytes, index)
        {
            let end = consume_path(bytes, index);
            out.push_str(REDACTED_ABSOLUTE_PATH);
            stats.redacted_absolute_paths += 1;
            index = end;
            continue;
        }

        let Some(ch) = value[index..].chars().next() else {
            break;
        };
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn is_unix_absolute_path(bytes: &[u8], index: usize) -> bool {
    if bytes.get(index) != Some(&b'/') || bytes.get(index + 1) == Some(&b'/') {
        return false;
    }
    if index > 0 && matches!(bytes[index - 1], b':' | b'/' | b'\\') {
        return false;
    }
    let Some(next) = bytes.get(index + 1) else {
        return false;
    };
    next.is_ascii_alphanumeric() || matches!(next, b'.' | b'_' | b'-')
}

fn is_windows_absolute_path(bytes: &[u8], index: usize) -> bool {
    let Some(letter) = bytes.get(index) else {
        return false;
    };
    letter.is_ascii_alphabetic()
        && bytes.get(index + 1) == Some(&b':')
        && matches!(bytes.get(index + 2), Some(b'\\' | b'/'))
}

fn is_unc_absolute_path(bytes: &[u8], index: usize) -> bool {
    matches!(
        (bytes.get(index), bytes.get(index + 1), bytes.get(index + 2)),
        (Some(b'\\'), Some(b'\\'), Some(next)) if next.is_ascii_alphanumeric()
    )
}

fn consume_path(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    while let Some(byte) = bytes.get(index) {
        if is_path_delimiter(*byte) {
            break;
        }
        index += 1;
    }
    index
}

fn is_path_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'"' | b'\'' | b'`' | b'<' | b'>' | b'{' | b'}' | b'[' | b']' | b'(' | b')'
        )
}

fn redact_secret_values(value: &str, stats: &mut RunArchiveNormalizationStats) -> String {
    let mut out = String::with_capacity(value.len());
    let mut token = String::new();

    for ch in value.chars() {
        if ch.is_whitespace() {
            flush_secret_token(&mut token, &mut out, stats);
            out.push(ch);
        } else {
            token.push(ch);
        }
    }
    flush_secret_token(&mut token, &mut out, stats);
    out
}

fn flush_secret_token(
    token: &mut String,
    out: &mut String,
    stats: &mut RunArchiveNormalizationStats,
) {
    if token.is_empty() {
        return;
    }
    let trimmed = token.trim_matches(|ch: char| {
        matches!(
            ch,
            '"' | '\'' | '`' | ',' | ';' | ':' | ')' | '(' | '[' | ']' | '{' | '}' | '<' | '>'
        )
    });
    if looks_like_secret_value(trimmed) {
        out.push_str(REDACTED_SECRET);
        stats.redacted_secret_values += 1;
    } else {
        out.push_str(token);
    }
    token.clear();
}

fn looks_like_secret_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    (lower.starts_with("sk-") && value.len() >= 18)
        || (lower.starts_with("ghp_") && value.len() >= 20)
        || (lower.starts_with("github_pat_") && value.len() >= 24)
        || (lower.starts_with("xoxb-") && value.len() >= 18)
        || (lower.starts_with("bearer") && value.len() >= 20)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn visibility_maps_to_org_ingest_scope() {
        assert_eq!(
            RunArchiveIngestScope::from_visibility(ArchiveVisibility::LocalOnly),
            RunArchiveIngestScope::None
        );
        assert_eq!(
            RunArchiveIngestScope::from_visibility(ArchiveVisibility::OrgSummary),
            RunArchiveIngestScope::Summary
        );
        assert_eq!(
            RunArchiveIngestScope::from_visibility(ArchiveVisibility::OrgEvidence),
            RunArchiveIngestScope::Evidence
        );
    }

    #[test]
    fn normalizer_removes_paths_provider_refs_pty_streams_and_secrets() {
        let fake_github_token = format!("{}{}", "ghp_", "123456789012345678901234");
        let payload = json!({
            "path": "/home/fixture/src/ctx/file.rs",
            "windows": "C:\\Users\\admin\\ctx\\secret.txt",
            "provider_session_ref": "thread-secret",
            "api_key": "sk-12345678901234567890",
            "pty_byte_stream": "raw terminal bytes",
            "nested": {
                "message": format!("open /tmp/project/.env with {fake_github_token}")
            },
            "token_usage": {
                "input": 12
            },
            "provider_id": "codex"
        });

        let normalized = normalize_archive_json(&payload);
        assert_eq!(normalized.value["path"], REDACTED_ABSOLUTE_PATH);
        assert_eq!(normalized.value["windows"], REDACTED_ABSOLUTE_PATH);
        assert_eq!(
            normalized.value["provider_session_ref"],
            REDACTED_PROVIDER_REF
        );
        assert_eq!(normalized.value["api_key"], REDACTED_SECRET);
        assert_eq!(normalized.value["pty_byte_stream"], REDACTED_PTY_STREAM);
        assert_eq!(normalized.value["provider_id"], "codex");
        assert_eq!(normalized.value["token_usage"]["input"], 12);
        assert!(normalized.value["nested"]["message"]
            .as_str()
            .unwrap()
            .contains(REDACTED_ABSOLUTE_PATH));
        assert!(normalized.stats.redacted_absolute_paths >= 3);
        assert_eq!(normalized.stats.redacted_provider_refs, 1);
        assert_eq!(normalized.stats.redacted_secret_fields, 1);
        assert_eq!(normalized.stats.redacted_pty_streams, 1);
        assert_eq!(normalized.stats.redacted_secret_values, 1);
    }

    #[test]
    fn transcript_text_normalization_redacts_inline_paths_and_tokens() {
        let normalized = normalize_archive_text(
            "Read /home/dev/project/main.rs then used sk-12345678901234567890.",
        );
        assert_eq!(
            normalized.text,
            format!("Read {REDACTED_ABSOLUTE_PATH} then used {REDACTED_SECRET}")
        );
        assert_eq!(normalized.stats.redacted_absolute_paths, 1);
        assert_eq!(normalized.stats.redacted_secret_values, 1);
    }

    #[test]
    fn transient_thought_events_are_omitted_even_for_evidence_scope() {
        let event = SessionEvent {
            seq: 44,
            id: SessionEventId::new(),
            session_id: SessionId::new(),
            run_id: Some(RunId::new()),
            turn_id: Some(TurnId::new()),
            event_type: SessionEventType::ThoughtChunk,
            payload_json: json!({"text": "secret chain of thought"}),
            transient: false,
            created_at: Utc.with_ymd_and_hms(2026, 4, 28, 12, 0, 0).unwrap(),
        };

        let (normalized, stats) =
            normalize_session_event_for_archive(&event, RunArchiveIngestScope::Evidence).unwrap();
        assert!(normalized.payload_json.is_none());
        assert_eq!(
            normalized.payload_omitted_reason.as_deref(),
            Some("transient_or_stream_payload")
        );
        assert_eq!(stats.dropped_transient_events, 1);
    }

    #[test]
    fn summary_scope_omits_payloads() {
        let event = SessionEvent {
            seq: 45,
            id: SessionEventId::new(),
            session_id: SessionId::new(),
            run_id: Some(RunId::new()),
            turn_id: None,
            event_type: SessionEventType::ToolResult,
            payload_json: json!({"path": "/home/fixture/src/ctx/file.rs"}),
            transient: false,
            created_at: Utc.with_ymd_and_hms(2026, 4, 28, 12, 0, 0).unwrap(),
        };

        let (normalized, stats) =
            normalize_session_event_for_archive(&event, RunArchiveIngestScope::Summary).unwrap();
        assert!(normalized.payload_json.is_none());
        assert_eq!(
            normalized.payload_omitted_reason.as_deref(),
            Some("scope_omits_event_payload")
        );
        assert_eq!(stats.omitted_content_payloads, 1);
        assert_eq!(stats.redacted_absolute_paths, 0);
    }
}

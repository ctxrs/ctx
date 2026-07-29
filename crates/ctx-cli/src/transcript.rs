use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use serde_json::{json, Value};
use uuid::Uuid;

use ctx_history_core::{CaptureProvider, Event, Session};
use ctx_history_store::Store;

use ctx_history_capture::complete_content::{
    VerifiedContentLocatorsV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};

use crate::output::compact_json;

mod artifact;
use artifact::atomic_write_output;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum TranscriptMode {
    Full,
    Lite,
    Log,
}

impl TranscriptMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Lite => "lite",
            Self::Log => "log",
        }
    }
}

pub(crate) fn resolve_session(
    store: &Store,
    id: Option<String>,
    provider: Option<CaptureProvider>,
    provider_session: Option<&str>,
) -> Result<Session> {
    if let Some(id) = id {
        return resolve_session_by_id_text(store, &id);
    }
    let provider = provider.ok_or_else(|| {
        anyhow!(
            "session lookup requires either a ctx session id or --provider with --provider-session"
        )
    })?;
    let provider_session = match provider_session {
        Some(value) => value.trim(),
        None => {
            return Err(anyhow!(
                "session lookup requires --provider-session when no ctx session id is provided"
            ));
        }
    };
    if provider_session.is_empty() {
        return Err(anyhow!("--provider-session cannot be empty"));
    }
    let matches = store.sessions_by_external_session_limited(provider, provider_session, 2)?;
    match matches.as_slice() {
        [session] => Ok(session.clone()),
        [] => Err(anyhow!(
            "no {provider} session with provider_session_id {provider_session:?} is indexed"
        )),
        _ => Err(anyhow!(
            "multiple {provider} sessions with provider_session_id {provider_session:?} are indexed; use ctx_session_id"
        )),
    }
}

pub(crate) fn write_output(body: String, out: Option<PathBuf>) -> Result<()> {
    if let Some(out) = out {
        if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        atomic_write_output(&out, body.as_bytes())?;
    } else {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

pub(crate) fn resolve_session_by_id_text(store: &Store, value: &str) -> Result<Session> {
    if let Ok(id) = Uuid::parse_str(value.trim()) {
        return store.get_session(id).with_context(|| {
            format!("session {id} was not found; rerun the search that found it with `--verbose` to get ctx_session_id")
        });
    }
    let prefix = normalize_uuid_prefix(value, "session")?;
    match store.sessions_by_id_prefix(&prefix)?.as_slice() {
        [session] => Ok(session.clone()),
        [] => Err(anyhow!(
            "session id prefix {prefix:?} was not found; rerun the search that found it with `--verbose` to get ctx_session_id"
        )),
        matches => Err(anyhow!(
            "session id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_session_id",
            matches[0].id,
            matches[1].id
        )),
    }
}

pub(crate) fn resolve_session_id(store: &Store, value: &str) -> Result<Uuid> {
    Ok(resolve_session_by_id_text(store, value)?.id)
}

pub(crate) fn resolve_event(store: &Store, value: &str) -> Result<Event> {
    if let Ok(id) = Uuid::parse_str(value.trim()) {
        return store.get_event(id).with_context(|| {
            format!(
                "event {id} was not found; rerun the event search with `--events --verbose` to get ctx_event_id"
            )
        });
    }
    let prefix = normalize_uuid_prefix(value, "event")?;
    match store.events_by_id_prefix(&prefix)?.as_slice() {
        [event] => Ok(event.clone()),
        [] => Err(anyhow!(
            "event id prefix {prefix:?} was not found; rerun the event search with `--events --verbose` to get ctx_event_id"
        )),
        matches => Err(anyhow!(
            "event id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_event_id",
            matches[0].id,
            matches[1].id
        )),
    }
}

pub(crate) fn normalize_uuid_prefix(value: &str, kind: &str) -> Result<String> {
    let prefix = value.trim();
    if prefix.len() < 8 {
        return Err(anyhow!(
            "{kind} id prefix must be at least 8 hex characters, or pass a full ctx UUID"
        ));
    }
    if prefix.contains('-') || !prefix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "{kind} id must be a full ctx UUID or an unambiguous hex prefix from verbose search output"
        ));
    }
    Ok(prefix.to_ascii_lowercase())
}

pub(crate) fn locate_session_json(store: &Store, session: &Session) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "payload_type": "session_location",
        "ctx_session_id": session.id,
        "provider": session.provider,
        "provider_session_id": session.external_session_id,
        "parent_ctx_session_id": session.parent_session_id,
        "root_ctx_session_id": session.root_session_id,
        "agent_type": session.agent_type,
        "role": session.role_hint,
        "status": session.status,
        "started_at": session.started_at,
        "ended_at": session.ended_at,
        "source": source_json_for(store, session.capture_source_id),
        "resume": provider_resume_json(session.provider, session.external_session_id.as_deref()),
    }))
}

pub(crate) fn locate_event_json(store: &Store, event: &Event) -> Value {
    let session = event.session_id.and_then(|id| store.get_session(id).ok());
    let locator = event
        .sync
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .and_then(VerifiedContentLocatorsV1::from_metadata_value)
        .and_then(|locators| locators.locator(VerifiedContentRole::MessageBody).cloned());
    compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_location",
        "ctx_event_id": event.id,
        "ctx_session_id": event.session_id,
        "provider": session.as_ref().map(|session| session.provider),
        "provider_session_id": session
            .as_ref()
            .and_then(|session| session.external_session_id.clone()),
        "sequence": event.seq,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": event.occurred_at,
        "source": source_json_for(store, event.capture_source_id),
        "source_record": {
            "ordinal": event.sync.metadata.get("source_record_ordinal"),
            "subrecord_index": event.sync.metadata.get("source_record_subrecord_index"),
            "fixture_line": event.sync.metadata.get("fixture_line"),
            "provider_event_hash": event.sync.metadata.get("provider_event_hash"),
            "provider_event_hash_authority": event.sync.metadata.get("provider_event_hash_authority"),
        },
        "complete_content": locator.as_ref().map(|locator| json!({
            "available": true,
            "source_family": locator.family(),
            "locator_kind": locator.kind(),
        })).unwrap_or_else(|| json!({"available": false})),
        "cursor": event_cursor(event),
        "resume": session
            .as_ref()
            .map(|session| provider_resume_json(session.provider, session.external_session_id.as_deref())),
    }))
}

pub(crate) fn source_json_for(store: &Store, source_id: Option<Uuid>) -> Option<Value> {
    let source = source_id.and_then(|source_id| store.get_capture_source(source_id).ok())?;
    let path = source.descriptor.raw_source_path.clone();
    let source_format = source
        .descriptor
        .source_format
        .clone()
        .or_else(|| source_format(&source.sync.metadata));
    Some(compact_json(json!({
        "source_id": source.id,
        "provider": source.descriptor.provider,
        "provider_session_id": source.descriptor.external_session_id,
        "path": path,
        "exists": source_path_exists(path.as_deref()),
        "cwd": source.descriptor.cwd,
        "started_at": source.started_at,
        "ended_at": source.ended_at,
        "source_format": source_format,
        "cursor": source_cursor(&source.sync.metadata),
        "verification_snapshot": {
            "size_bytes": source.sync.metadata.get("last_imported_size_bytes"),
            "modified_at_ms": source.sync.metadata.get("last_imported_modified_at_ms"),
            "sha256": source.sync.metadata.get("last_imported_sha256"),
        },
    })))
}

pub(crate) fn source_path_exists(source_path: Option<&str>) -> Option<bool> {
    source_path.map(|path| Path::new(path).exists())
}

pub(crate) fn source_format(metadata: &Value) -> Option<String> {
    for pointer in [
        "/source_format",
        "/format",
        "/provider/source_format",
        "/source/source_format",
    ] {
        if let Some(value) = metadata.pointer(pointer).and_then(|value| value.as_str()) {
            return Some(value.to_owned());
        }
    }
    None
}

pub(crate) fn source_cursor(metadata: &Value) -> Option<String> {
    metadata
        .pointer("/cursor/after/cursor")
        .and_then(|value| value.as_str())
        .or_else(|| metadata.pointer("/cursor").and_then(|value| value.as_str()))
        .map(str::to_owned)
}

pub(crate) fn event_cursor(event: &Event) -> Option<String> {
    if let Some(cursor) = event.payload.get("cursor").and_then(|value| value.as_str()) {
        return Some(cursor.to_owned());
    }
    event
        .payload
        .get("body")
        .and_then(|body| body.get("cursor"))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

pub(crate) fn provider_resume_json(
    provider: CaptureProvider,
    provider_session_id: Option<&str>,
) -> Value {
    let (command, argv) = match (provider, provider_session_id) {
        (CaptureProvider::Codex, Some(session_id)) => (
            Some(format!("codex resume {}", shell_quote_arg(session_id))),
            Some(vec![
                "codex".to_owned(),
                "resume".to_owned(),
                session_id.to_owned(),
            ]),
        ),
        _ => (None, None),
    };
    compact_json(json!({
        "available": command.is_some(),
        "command": command,
        "argv": argv,
    }))
}

pub(crate) fn shell_quote_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@'))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn print_locate_session_text(value: &Value) -> Result<()> {
    println!(
        "ctx_session_id: {}",
        value["ctx_session_id"].as_str().unwrap_or("")
    );
    print_optional_json_str(value, "provider");
    print_optional_json_str(value, "provider_session_id");
    if let Some(source) = value.get("source") {
        print_optional_json_str(source, "path");
        print_optional_json_str(source, "source_format");
        if let Some(exists) = source.get("exists").and_then(|value| value.as_bool()) {
            println!("source_exists: {exists}");
        }
    }
    if let Some(command) = value
        .get("resume")
        .and_then(|resume| resume.get("command"))
        .and_then(|value| value.as_str())
    {
        println!("resume_command: {command}");
    }
    Ok(())
}

pub(crate) fn print_locate_event_text(value: &Value) -> Result<()> {
    println!(
        "ctx_event_id: {}",
        value["ctx_event_id"].as_str().unwrap_or("")
    );
    print_optional_json_str(value, "ctx_session_id");
    print_optional_json_str(value, "provider");
    print_optional_json_str(value, "provider_session_id");
    print_optional_json_str(value, "event_type");
    print_optional_json_str(value, "role");
    print_optional_json_str(value, "cursor");
    if let Some(source) = value.get("source") {
        print_optional_json_str(source, "path");
    }
    if let Some(source_record) = value.get("source_record") {
        if let Some(ordinal) = source_record.get("ordinal").and_then(Value::as_u64) {
            println!("source_record_ordinal: {ordinal}");
        }
        if let Some(index) = source_record.get("subrecord_index").and_then(Value::as_u64) {
            println!("source_record_subrecord_index: {index}");
        }
    }
    Ok(())
}

pub(crate) fn print_optional_json_str(value: &Value, key: &str) {
    if let Some(text) = value.get(key).and_then(|value| value.as_str()) {
        println!("{key}: {text}");
    }
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use serde_json::{json, Value};
use uuid::Uuid;

use ctx_history_core::{CaptureProvider, Event, EventRole, EventType, RedactionState, Session};
use ctx_history_store::Store;

use crate::output::{compact_json, OutputFormat};

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
pub(crate) struct ShowDto;
pub(crate) fn event_preview(event: &Event) -> String {
    let preview = ctx_history_search::event_preview_text(event);
    if preview.trim().is_empty() {
        format!("{} event", event.event_type.as_str())
    } else {
        ctx_history_search::display_snippet(&preview, 120)
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

pub(crate) fn event_window(
    store: &Store,
    event: &Event,
    before: usize,
    after: usize,
    window: Option<usize>,
) -> Result<Vec<Event>> {
    let (before, after) = window
        .map(|window| (window, window))
        .unwrap_or((before, after));
    Ok(store.events_for_session_window(event, before, after)?)
}

pub(crate) fn write_rendered_session(
    store: &Store,
    session: &Session,
    events: &[Event],
    mode: TranscriptMode,
    format: OutputFormat,
    out: Option<PathBuf>,
) -> Result<()> {
    let body = match format {
        OutputFormat::Text => render_session_text(store, session, events, mode),
        OutputFormat::Markdown => render_session_markdown(store, session, events, mode),
        OutputFormat::Json => serde_json::to_string_pretty(&session_transcript_json(
            store, session, events, mode, format,
        ))?,
        OutputFormat::Jsonl => render_session_jsonl(store, session, events, mode)?,
    };
    write_output(body, out)
}

pub(crate) fn write_rendered_events(
    store: &Store,
    selected: &Event,
    events: &[Event],
    format: OutputFormat,
    out: Option<PathBuf>,
) -> Result<()> {
    let body = match format {
        OutputFormat::Text => render_events_text(store, selected, events),
        OutputFormat::Markdown => render_events_markdown(store, selected, events),
        OutputFormat::Json => {
            serde_json::to_string_pretty(&event_window_json(store, selected, events, format))?
        }
        OutputFormat::Jsonl => render_events_jsonl(store, events)?,
    };
    write_output(body, out)
}

pub(crate) fn write_output(body: String, out: Option<PathBuf>) -> Result<()> {
    if let Some(out) = out {
        if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(&out, body).with_context(|| format!("write {}", out.display()))?;
    } else {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

pub(crate) fn selected_transcript_events(events: &[Event], mode: TranscriptMode) -> Vec<&Event> {
    match mode {
        TranscriptMode::Log => events.iter().collect(),
        TranscriptMode::Full => events.iter().filter(|event| is_message(event)).collect(),
        TranscriptMode::Lite => lite_transcript_events(events),
    }
}

pub(crate) fn lite_transcript_events(events: &[Event]) -> Vec<&Event> {
    let mut selected = Vec::new();
    let mut pending_assistant: Option<&Event> = None;
    for event in events {
        if is_user_message(event) {
            if let Some(assistant) = pending_assistant.take() {
                selected.push(assistant);
            }
            selected.push(event);
        } else if is_assistant_message(event) {
            pending_assistant = Some(event);
        }
    }
    if let Some(assistant) = pending_assistant {
        selected.push(assistant);
    }
    selected
}

pub(crate) fn is_message(event: &Event) -> bool {
    event.event_type == EventType::Message
        && matches!(
            event.role,
            Some(EventRole::User | EventRole::Assistant | EventRole::System)
        )
}

pub(crate) fn is_user_message(event: &Event) -> bool {
    event.event_type == EventType::Message && event.role == Some(EventRole::User)
}

pub(crate) fn is_assistant_message(event: &Event) -> bool {
    event.event_type == EventType::Message && event.role == Some(EventRole::Assistant)
}

pub(crate) fn event_content(event: &Event) -> String {
    if matches!(
        event.redaction_state,
        RedactionState::Raw | RedactionState::Withheld
    ) {
        return "raw event payload withheld".to_owned();
    }
    if let Some(value) = event.payload.get("body").and_then(event_value_text) {
        return ctx_history_search::display_snippet(&value, 16_000);
    }
    if let Some(value) = event_value_text(&event.payload) {
        return ctx_history_search::display_snippet(&value, 16_000);
    }
    let preview = ctx_history_search::event_preview_text(event);
    if preview.trim().is_empty() {
        format!("{} event", event.event_type.as_str())
    } else {
        ctx_history_search::display_snippet(&preview, 16_000)
    }
}

pub(crate) fn event_value_text(value: &Value) -> Option<String> {
    if let Some(value) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(value.to_owned());
    }
    let object = value.as_object()?;
    for key in [
        "text",
        "preview",
        "summary",
        "command",
        "output_preview",
        "output",
        "message",
    ] {
        if let Some(value) = object
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_owned());
        }
    }
    let structured = ["tool", "name", "arguments_preview", "status"]
        .into_iter()
        .filter_map(|key| object.get(key).and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if structured.is_empty() {
        None
    } else {
        Some(structured.join(" "))
    }
}

pub(crate) fn render_session_text(
    store: &Store,
    session: &Session,
    events: &[Event],
    mode: TranscriptMode,
) -> String {
    let mut out = String::new();
    push_session_header(&mut out, store, session, mode, OutputFormat::Text);
    for event in selected_transcript_events(events, mode) {
        push_event_text_block(&mut out, event);
    }
    out
}

pub(crate) fn render_session_markdown(
    store: &Store,
    session: &Session,
    events: &[Event],
    mode: TranscriptMode,
) -> String {
    let mut out = String::new();
    let label = session
        .external_session_id
        .clone()
        .unwrap_or_else(|| session.id.to_string());
    out.push_str(&format!("# {} session {}\n\n", session.provider, label));
    push_session_metadata_markdown(&mut out, store, session, mode, OutputFormat::Markdown);
    for event in selected_transcript_events(events, mode) {
        let heading = event
            .role
            .map(|role| role.as_str())
            .unwrap_or(event.event_type.as_str());
        out.push_str(&format!(
            "\n## {} - {} - {}\n\n",
            heading,
            event.event_type.as_str(),
            event.occurred_at
        ));
        out.push_str(&format!("ctx_event_id: `{}`\n\n", event.id));
        out.push_str(&event_content(event));
        out.push('\n');
    }
    out
}

pub(crate) fn push_session_header(
    out: &mut String,
    store: &Store,
    session: &Session,
    mode: TranscriptMode,
    format: OutputFormat,
) {
    out.push_str(&format!("ctx_session_id: {}\n", session.id));
    out.push_str(&format!("provider: {}\n", session.provider));
    if let Some(provider_session_id) = &session.external_session_id {
        out.push_str(&format!("provider_session_id: {provider_session_id}\n"));
    }
    out.push_str(&format!("mode: {}\n", mode.as_str()));
    out.push_str(&format!("format: {}\n", format.as_str()));
    if let Some(source) = source_json_for(store, session.capture_source_id) {
        if let Some(path) = source.get("path").and_then(|value| value.as_str()) {
            out.push_str(&format!("source_path: {path}\n"));
        }
    }
    out.push('\n');
}

pub(crate) fn push_session_metadata_markdown(
    out: &mut String,
    store: &Store,
    session: &Session,
    mode: TranscriptMode,
    format: OutputFormat,
) {
    out.push_str(&format!("- ctx_session_id: `{}`\n", session.id));
    out.push_str(&format!("- provider: `{}`\n", session.provider));
    if let Some(provider_session_id) = &session.external_session_id {
        out.push_str(&format!("- provider_session_id: `{provider_session_id}`\n"));
    }
    out.push_str(&format!("- mode: `{}`\n", mode.as_str()));
    out.push_str(&format!("- format: `{}`\n", format.as_str()));
    if let Some(source) = source_json_for(store, session.capture_source_id) {
        if let Some(path) = source.get("path").and_then(|value| value.as_str()) {
            out.push_str(&format!("- source_path: `{path}`\n"));
        }
    }
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

pub(crate) fn push_event_text_block(out: &mut String, event: &Event) {
    let role = event.role.map(|role| role.as_str()).unwrap_or("-");
    out.push_str(&format!(
        "[{}] {} {} {}\n",
        event.occurred_at,
        role,
        event.event_type.as_str(),
        event.id
    ));
    out.push_str(&event_content(event));
    out.push_str("\n\n");
}

pub(crate) fn render_events_text(store: &Store, selected: &Event, events: &[Event]) -> String {
    let mut out = String::new();
    out.push_str(&format!("ctx_event_id: {}\n", selected.id));
    if let Some(session_id) = selected.session_id {
        out.push_str(&format!("ctx_session_id: {session_id}\n"));
        if let Ok(session) = store.get_session(session_id) {
            out.push_str(&format!("provider: {}\n", session.provider));
            if let Some(provider_session_id) = session.external_session_id {
                out.push_str(&format!("provider_session_id: {provider_session_id}\n"));
            }
        }
    }
    out.push('\n');
    for event in events {
        push_event_text_block(&mut out, event);
    }
    out
}

pub(crate) fn render_events_markdown(store: &Store, selected: &Event, events: &[Event]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Event {}\n\n", selected.id));
    if let Some(session_id) = selected.session_id {
        out.push_str(&format!("- ctx_session_id: `{session_id}`\n"));
        if let Ok(session) = store.get_session(session_id) {
            out.push_str(&format!("- provider: `{}`\n", session.provider));
            if let Some(provider_session_id) = session.external_session_id {
                out.push_str(&format!("- provider_session_id: `{provider_session_id}`\n"));
            }
        }
    }
    for event in events {
        let role = event.role.map(|role| role.as_str()).unwrap_or("-");
        out.push_str(&format!(
            "\n## {} - {} - {}\n\n",
            role,
            event.event_type.as_str(),
            event.occurred_at
        ));
        out.push_str(&format!("ctx_event_id: `{}`\n\n", event.id));
        out.push_str(&event_content(event));
        out.push('\n');
    }
    out
}

pub(crate) fn session_transcript_json(
    store: &Store,
    session: &Session,
    events: &[Event],
    mode: TranscriptMode,
    format: OutputFormat,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "item_type": "session_transcript",
        "ctx_session_id": session.id,
        "provider": session.provider,
        "provider_session_id": session.external_session_id,
        "mode": mode.as_str(),
        "format": format.as_str(),
        "session": ShowDto::session(store, session),
        "source": source_json_for(store, session.capture_source_id),
        "events": selected_transcript_events(events, mode)
            .into_iter()
            .map(|event| transcript_event_json(store, event))
            .collect::<Vec<_>>(),
    }))
}

pub(crate) fn event_window_json(
    store: &Store,
    selected: &Event,
    events: &[Event],
    format: OutputFormat,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "item_type": "event_window",
        "ctx_event_id": selected.id,
        "ctx_session_id": selected.session_id,
        "format": format.as_str(),
        "event": transcript_event_json(store, selected),
        "events": events
            .iter()
            .map(|event| transcript_event_json(store, event))
            .collect::<Vec<_>>(),
    }))
}

pub(crate) fn transcript_event_json(store: &Store, event: &Event) -> Value {
    let session = event.session_id.and_then(|id| store.get_session(id).ok());
    compact_json(json!({
        "ctx_event_id": event.id,
        "item_id": event.id,
        "item_type": "event",
        "ctx_session_id": event.session_id,
        "provider": session.as_ref().map(|session| session.provider),
        "provider_session_id": session
            .as_ref()
            .and_then(|session| session.external_session_id.clone()),
        "sequence": event.seq,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": event.occurred_at,
        "source_id": event.capture_source_id,
        "source_path": source_path_for(store, event.capture_source_id),
        "source_exists": source_path_exists(source_path_for(store, event.capture_source_id).as_deref()),
        "source": source_json_for(store, event.capture_source_id),
        "cursor": event_cursor(event),
        "preview": event_preview(event),
        "text": event_content(event),
        "redaction_state": event.redaction_state,
    }))
}

pub(crate) fn render_session_jsonl(
    store: &Store,
    session: &Session,
    events: &[Event],
    mode: TranscriptMode,
) -> Result<String> {
    let mut lines = Vec::new();
    for event in selected_transcript_events(events, mode) {
        lines.push(serde_json::to_string(&compact_json(json!({
            "schema_version": 1,
            "item_type": "session_transcript_event",
            "mode": mode.as_str(),
            "ctx_session_id": session.id,
            "provider": session.provider,
            "provider_session_id": session.external_session_id,
            "event": transcript_event_json(store, event),
        })))?);
    }
    Ok(lines.join("\n") + "\n")
}

pub(crate) fn render_events_jsonl(store: &Store, events: &[Event]) -> Result<String> {
    let mut lines = Vec::new();
    for event in events {
        lines.push(serde_json::to_string(&transcript_event_json(store, event))?);
    }
    Ok(lines.join("\n") + "\n")
}

pub(crate) fn locate_session_json(store: &Store, session: &Session) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "item_type": "session_location",
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
    compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "item_type": "event_location",
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
        "cursor": event_cursor(event),
        "resume": session
            .as_ref()
            .map(|session| provider_resume_json(session.provider, session.external_session_id.as_deref())),
    }))
}

pub(crate) fn source_json_for(store: &Store, source_id: Option<Uuid>) -> Option<Value> {
    let source = source_id.and_then(|source_id| store.get_capture_source(source_id).ok())?;
    let path = source.descriptor.raw_source_path.clone();
    Some(compact_json(json!({
        "source_id": source.id,
        "provider": source.descriptor.provider,
        "provider_session_id": source.descriptor.external_session_id,
        "path": path,
        "exists": source_path_exists(path.as_deref()),
        "cwd": source.descriptor.cwd,
        "started_at": source.started_at,
        "ended_at": source.ended_at,
        "source_format": source_format(&source.sync.metadata),
        "cursor": source_cursor(&source.sync.metadata),
    })))
}

pub(crate) fn source_path_for(store: &Store, source_id: Option<Uuid>) -> Option<String> {
    source_id
        .and_then(|source_id| store.get_capture_source(source_id).ok())
        .and_then(|source| source.descriptor.raw_source_path)
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
    Ok(())
}

pub(crate) fn print_optional_json_str(value: &Value, key: &str) {
    if let Some(text) = value.get(key).and_then(|value| value.as_str()) {
        println!("{key}: {text}");
    }
}
impl ShowDto {
    pub(crate) fn session(store: &Store, session: &Session) -> Value {
        let source_path = source_path_for(store, session.capture_source_id);
        compact_json(json!({
            "id": session.id,
            "item_id": session.id,
            "item_type": "session",
            "provider": session.provider,
            "external_session_id": session.external_session_id,
            "agent_type": session.agent_type,
            "role": session.role_hint,
            "is_primary": session.is_primary,
            "status": session.status,
            "started_at": session.started_at,
            "ended_at": session.ended_at,
            "source_id": session.capture_source_id,
            "source_path": source_path,
            "source_exists": source_path_exists(source_path.as_deref()),
        }))
    }
}

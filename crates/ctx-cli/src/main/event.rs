#[allow(unused_imports)]
use super::*;

pub(crate) const MAX_EVENT_WINDOW: usize = 50;

#[derive(Debug, Args)]
pub(crate) struct ShowEventArgs {
    #[arg(help = "ctx event id or unambiguous id prefix")]
    pub(crate) id: String,
    #[arg(long, default_value_t = 0, value_parser = parse_event_window_limit)]
    pub(crate) before: usize,
    #[arg(long, default_value_t = 0, value_parser = parse_event_window_limit)]
    pub(crate) after: usize,
    #[arg(long, value_parser = parse_event_window_limit)]
    pub(crate) window: Option<usize>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) format: OutputFormat,
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct LocateEventArgs {
    #[arg(help = "ctx event id or unambiguous id prefix")]
    pub(crate) id: String,
    #[arg(long, value_enum, default_value_t = LocateFormat::Text)]
    pub(crate) format: LocateFormat,
    #[arg(long)]
    pub(crate) json: bool,
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

pub(crate) fn render_events_jsonl(store: &Store, events: &[Event]) -> Result<String> {
    let mut lines = Vec::new();
    for event in events {
        lines.push(serde_json::to_string(&transcript_event_json(store, event))?);
    }
    Ok(lines.join("\n") + "\n")
}

pub(crate) fn public_citations(citations: &[ContextCitation]) -> Vec<Value> {
    citations
        .iter()
        .map(|citation| {
            let ctx_event_id = if citation.citation_type == ContextCitationType::Event {
                Some(citation.id)
            } else {
                None
            };
            let ctx_session_id = if citation.citation_type == ContextCitationType::Session {
                Some(citation.id)
            } else {
                citation.session_id
            };
            compact_json(json!({
                "item_id": citation.id,
                "item_type": public_citation_item_type(citation.citation_type),
                "ctx_event_id": ctx_event_id,
                "ctx_session_id": ctx_session_id,
                "label": citation.label,
                "time": citation.time,
                "provider": citation.provider,
                "session_id": citation.session_id,
                "event_seq": citation.event_seq,
                "source_path": citation.raw_source_path,
                "source_exists": citation.raw_source_exists,
                "cursor": citation.cursor,
            }))
        })
        .collect()
}

pub(crate) fn parse_event_window_limit(value: &str) -> std::result::Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|err| format!("invalid event window: {err}"))?;
    if limit > MAX_EVENT_WINDOW {
        return Err(format!(
            "event window must be between 0 and {MAX_EVENT_WINDOW}"
        ));
    }
    Ok(limit)
}

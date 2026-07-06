#[allow(unused_imports)]
use super::*;

#[derive(Debug, Subcommand)]
pub(crate) enum ShowTarget {
    #[command(about = "Show a session transcript")]
    Session(ShowSessionArgs),
    #[command(about = "Show one event or a surrounding event window")]
    Event(ShowEventArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ShowSessionArgs {
    #[arg(help = "ctx session id or unambiguous id prefix")]
    pub(crate) id: Option<String>,
    #[arg(long, value_parser = parse_provider_arg)]
    #[arg(hide_possible_values = true)]
    pub(crate) provider: Option<ProviderArg>,
    #[arg(long = "provider-session")]
    pub(crate) provider_session: Option<String>,
    #[arg(long, value_enum, default_value_t = TranscriptMode::Lite)]
    pub(crate) mode: TranscriptMode,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) format: OutputFormat,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct LocateSessionArgs {
    #[arg(help = "ctx session id or unambiguous id prefix")]
    pub(crate) id: Option<String>,
    #[arg(long, value_parser = parse_provider_arg)]
    #[arg(hide_possible_values = true)]
    pub(crate) provider: Option<ProviderArg>,
    #[arg(long = "provider-session")]
    pub(crate) provider_session: Option<String>,
    #[arg(long, value_enum, default_value_t = LocateFormat::Text)]
    pub(crate) format: LocateFormat,
    #[arg(long)]
    pub(crate) json: bool,
}

impl ShowArgs {
    pub(crate) fn json_output(&self) -> bool {
        match &self.target {
            ShowTarget::Session(args) => args.json || args.format == OutputFormat::Json,
            ShowTarget::Event(args) => args.json || args.format == OutputFormat::Json,
        }
    }
}

impl LocateArgs {
    pub(crate) fn json_output(&self) -> bool {
        match &self.target {
            LocateTarget::Session(args) => args.json || args.format == LocateFormat::Json,
            LocateTarget::Event(args) => args.json || args.format == LocateFormat::Json,
        }
    }
}

pub(crate) fn run_show(
    args: ShowArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let db_path = database_path(data_root);
    let store = open_existing_store_read_only(&db_path, "ctx show")?;
    match args.target {
        ShowTarget::Session(args) => {
            let session = resolve_session(
                &store,
                args.id,
                args.provider.map(ProviderArg::capture_provider),
                args.provider_session.as_deref(),
            )?;
            let events = store.events_for_session(session.id)?;
            analytics::insert_count_bucket(
                analytics_properties,
                "events_returned_bucket",
                events.len() as u64,
            );
            let format = effective_format(args.format, args.json);
            write_rendered_session(&store, &session, &events, args.mode, format, args.out)?;
        }
        ShowTarget::Event(args) => {
            let event = resolve_event(&store, &args.id)?;
            let events = event_window(&store, &event, args.before, args.after, args.window)?;
            analytics::insert_count_bucket(
                analytics_properties,
                "events_returned_bucket",
                events.len() as u64,
            );
            let format = effective_format(args.format, args.json);
            write_rendered_events(&store, &event, &events, format, None)?;
        }
    }
    Ok(())
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
    let provider_session = provider_session
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!("session lookup requires --provider-session when no ctx session id is provided")
        })?;
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

pub(crate) fn resolve_session_id(store: &Store, value: &str) -> Result<Uuid> {
    Ok(resolve_session_by_id_text(store, value)?.id)
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

pub(crate) fn public_citation_item_type(citation_type: ContextCitationType) -> &'static str {
    match citation_type {
        ContextCitationType::HistoryRecord => "indexed_item",
        ContextCitationType::Session => "session",
        ContextCitationType::Run => "run",
        ContextCitationType::Event => "event",
        ContextCitationType::VcsChange => "vcs_change",
        ContextCitationType::Artifact => "artifact",
        ContextCitationType::Summary => "summary",
        ContextCitationType::File => "file",
    }
}

pub(crate) fn item_type_for_id(store: &Store, item_id: Uuid) -> String {
    if let Ok(record) = store.get_record(item_id) {
        return public_record_item_type(&record);
    }
    if store.get_event(item_id).is_ok() {
        return "event".to_owned();
    }
    if store.get_session(item_id).is_ok() {
        return "session".to_owned();
    }
    if store.get_run(item_id).is_ok() {
        return "run".to_owned();
    }
    "indexed_item".to_owned()
}

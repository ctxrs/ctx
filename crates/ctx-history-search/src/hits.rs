use crate::*;

pub(crate) fn record_context_display_hit(
    context: &RecordContext,
    filters: &SearchFilters,
    time: chrono::DateTime<Utc>,
) -> HitMetadata {
    context
        .sessions
        .iter()
        .find(|session| {
            session_matches_agent_scope(session, filters)
                && filters
                    .provider
                    .map_or(true, |provider| session.provider == provider)
                && filters.session.map_or(true, |id| session.id == id)
                && hit_matches_history_source_filter(&session_hit(session, context), filters)
        })
        .or_else(|| {
            context
                .sessions
                .iter()
                .find(|session| session_matches_agent_scope(session, filters))
        })
        .map(|session| session_hit(session, context))
        .unwrap_or_else(|| empty_hit(time))
}

pub(crate) fn empty_hit(time: chrono::DateTime<Utc>) -> HitMetadata {
    HitMetadata {
        time,
        provider: None,
        provider_session_id: None,
        history_source: None,
        history_source_plugin: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        session_id: None,
        parent_session_id: None,
        root_session_id: None,
        event_id: None,
        event_seq: None,
        cwd: None,
        raw_source_path: None,
        raw_source_exists: None,
        cursor: None,
    }
}

pub(crate) fn session_hit(session: &Session, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(session.capture_source_id, session.started_at, context);
    hit.provider = Some(session.provider);
    hit.provider_session_id = session.external_session_id.clone();
    hit.session_id = Some(session.id);
    hit.parent_session_id = session.parent_session_id;
    hit.root_session_id = session.root_session_id;
    if hit.cwd.is_none() {
        hit.cwd = source_for_id(session.capture_source_id, context)
            .and_then(|source| source.descriptor.cwd.clone());
    }
    hit
}

pub(crate) fn run_hit(run: &Run, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(run.source_id, run.started_at, context);
    hit.session_id = run.session_id;
    if let Some(session) = run
        .session_id
        .and_then(|id| context.sessions.iter().find(|session| session.id == id))
    {
        if hit.provider.is_none() {
            hit.provider = Some(session.provider);
        }
        if hit.provider_session_id.is_none() {
            hit.provider_session_id = session.external_session_id.clone();
        }
        hit.parent_session_id = session.parent_session_id;
        hit.root_session_id = session.root_session_id;
    }
    if hit.cwd.is_none() {
        hit.cwd = run.cwd.clone();
    }
    hit
}

pub(crate) fn event_hit(event: &Event, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(event.capture_source_id, event.occurred_at, context);
    hit.session_id = event.session_id;
    hit.event_id = Some(event.id);
    hit.event_seq = Some(event.seq);
    hit.cursor = event_cursor(event).or(hit.cursor);
    if hit.provider.is_none() {
        if let Some(session) = event
            .session_id
            .and_then(|id| context.sessions.iter().find(|session| session.id == id))
        {
            hit.provider = Some(session.provider);
            if hit.provider_session_id.is_none() {
                hit.provider_session_id = session.external_session_id.clone();
            }
            hit.parent_session_id = session.parent_session_id;
            hit.root_session_id = session.root_session_id;
        }
    }
    hit
}

pub(crate) fn artifact_hit(artifact: &Artifact, context: &RecordContext) -> HitMetadata {
    source_hit(artifact.source_id, artifact.timestamps.updated_at, context)
}

pub(crate) fn file_hit(file: &FileTouched, context: &RecordContext) -> HitMetadata {
    let mut hit = source_hit(file.source_id, file.timestamps.updated_at, context);
    hit.event_id = file.event_id;
    hit.session_id = file.event_id.and_then(|id| {
        context
            .events
            .iter()
            .find(|event| event.id == id)
            .and_then(|event| event.session_id)
    });
    if let Some(session) = hit
        .session_id
        .and_then(|id| context.sessions.iter().find(|session| session.id == id))
    {
        hit.provider = Some(session.provider);
        hit.provider_session_id = session.external_session_id.clone();
        hit.parent_session_id = session.parent_session_id;
        hit.root_session_id = session.root_session_id;
    }
    hit
}

pub(crate) fn source_hit(
    source_id: Option<Uuid>,
    time: chrono::DateTime<Utc>,
    context: &RecordContext,
) -> HitMetadata {
    let Some(source) = source_for_id(source_id, context) else {
        return empty_hit(time);
    };
    let raw_source_path = source.descriptor.raw_source_path.clone();
    let identity = source_history_identity(source);
    let mut hit = HitMetadata {
        time,
        provider: Some(source.descriptor.provider),
        provider_session_id: source.descriptor.external_session_id.clone(),
        history_source: identity.history_source,
        history_source_plugin: identity.history_source_plugin,
        provider_key: identity.provider_key,
        source_id: identity.source_id,
        source_format: identity.source_format,
        session_id: None,
        parent_session_id: None,
        root_session_id: None,
        event_id: None,
        event_seq: None,
        cwd: source.descriptor.cwd.clone(),
        raw_source_exists: raw_source_path
            .as_deref()
            .map(|path| Path::new(path).exists()),
        raw_source_path,
        cursor: source_cursor(source),
    };
    if let Some(session) = associated_session_for_source(source.id, context) {
        hit.provider = Some(session.provider);
        hit.provider_session_id = session.external_session_id.clone();
        hit.session_id = Some(session.id);
        hit.parent_session_id = session.parent_session_id;
        hit.root_session_id = session.root_session_id;
    }
    hit
}

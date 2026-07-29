use super::*;

pub(super) fn discover_sessions(path: &Path) -> Result<Vec<MuxSessionSource>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let mut sessions = Vec::new();
    visit_mux_session_sources(path, &mut |source| {
        sessions.push(source);
        Ok(())
    })?;
    Ok(sessions)
}

pub(super) fn plan_source(
    store: &Store,
    configured_root: &Path,
    source: MuxSessionSource,
    path: PathBuf,
    kind: MuxStreamKind,
    context: &ProviderAdapterContext,
) -> Result<MuxSourcePlan> {
    let observation = MuxFileObservation::read(&path, source.metadata_path.as_deref())?;
    let path_identity = provider_path_identity(&observation.canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        &path_identity,
    );
    let canonical_source_identity = mux_canonical_source_identity(configured_root, &path_identity);
    let source_revision = observation.source_revision(kind.label());
    let metadata_revision = observation.metadata_revision();
    let prior = load_source_cursor(store, &context.machine_id, &cursor_stream)?;
    let mut generation = 0;
    let mut initial_frontier = MuxFrontier::initial();
    let mut accepted_events = 0;
    let mut rejected_records = 0;
    let mut first_failure = None;
    if let Some(loaded) = prior.as_ref() {
        let wire = &loaded.wire;
        if wire.version != MUX_CURSOR_VERSION
            || wire.capture_revision != MUX_CAPTURE_REVISION
            || wire.policy_revision != MUX_POLICY_REVISION
            || wire.kind != kind
            || wire.canonical_path != observation.canonical_path
            || wire.frontier.version != MUX_FRONTIER_VERSION
        {
            return Err(CaptureError::InvalidPayload(
                "Mux NativePath cursor identity is inconsistent".to_owned(),
            ));
        }
        generation = wire.generation;
        let can_resume = (!wire.retired
            && wire.source_revision == source_revision
            && prefix_matches(&path, &observation, &wire.frontier)?)
            || (!wire.retired
                && kind == MuxStreamKind::Chat
                && wire.metadata_revision == metadata_revision
                && prefix_matches(&path, &observation, &wire.frontier)?);
        if can_resume {
            initial_frontier = wire.frontier.clone();
            accepted_events = wire.accepted_events;
            rejected_records = wire.rejected_records;
            first_failure.clone_from(&wire.first_failure);
        } else {
            generation = generation
                .checked_add(1)
                .ok_or(CaptureError::InvalidPayload(
                    "Mux NativePath source generation is exhausted".to_owned(),
                ))?;
        }
    }
    Ok(MuxSourcePlan {
        source,
        path,
        kind,
        observation,
        path_identity,
        cursor_stream,
        canonical_source_identity,
        source_revision,
        metadata_revision,
        prior,
        generation,
        initial_frontier,
        accepted_events,
        rejected_records,
        first_failure,
    })
}

pub(super) fn load_source_cursor(
    store: &Store,
    machine_id: &str,
    stream: &str,
) -> Result<Option<MuxLoadedCursor>> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(None);
    };
    let committed =
        decode_native_path_committed_cursor(&stored.cursor).map_err(CaptureError::Store)?;
    let wire = serde_json::from_str::<MuxCursorWire>(committed.provider_cursor())
        .map_err(|_| CaptureError::InvalidPayload("Mux NativePath cursor is corrupt".to_owned()))?;
    Ok(Some(MuxLoadedCursor { stored, wire }))
}

pub(super) fn prefix_matches(
    path: &Path,
    observation: &MuxFileObservation,
    frontier: &MuxFrontier,
) -> Result<bool> {
    let content_identity = observation.content_identity();
    if frontier.file_identity.as_deref() != Some(content_identity.as_str()) {
        return Ok(false);
    }
    let metadata = fs::metadata(path)?;
    if metadata.len() < frontier.next_offset {
        return Ok(false);
    }
    let mut file = File::open(path)?;
    let mut remaining = frontier.next_offset;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("Mux prefix size exceeds usize"))?;
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            return Ok(false);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(<[u8; 32]>::from(hasher.finalize()) == frontier.prefix_sha256)
}

pub(super) fn mux_canonical_source_identity(configured_root: &Path, path_identity: &str) -> String {
    let key = format!(
        "{}\0{}\0{}",
        CaptureProvider::Mux.as_str(),
        configured_root.display(),
        path_identity
    );
    format!(
        "mux-nativepath:{}",
        stable_capture_uuid(&key, "canonical-source")
    )
}

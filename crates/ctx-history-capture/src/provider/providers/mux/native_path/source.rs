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

pub(super) fn mux_legacy_bridge(
    store: &Store,
    context: &ProviderAdapterContext,
    source: &MuxSessionSource,
) -> Result<Option<MuxLegacyBridge>> {
    let Some(primary_path) = source.chat_path.as_ref().or(source.partial_path.as_ref()) else {
        return Ok(None);
    };
    let primary_path_display = primary_path.display().to_string();
    let legacy_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        &primary_path_display,
    );
    let Some(cursor) = store.get_sync_cursor(None, &context.machine_id, &legacy_stream)? else {
        return Ok(None);
    };
    if !released_mux_cursor_matches_source(&cursor.cursor, source) {
        return Err(CaptureError::InvalidPayload(
            "Mux released cursor does not address its merged session source".to_owned(),
        ));
    }

    let layout = scan_released_mux_layout(source)?;
    let provider_session_id = bounded_mux_id(
        layout
            .provider_session_id
            .unwrap_or_else(|| source.provider_session_id.clone()),
        &source.session_dir,
        "workspace id",
    )?;
    let primary_source_id = provider_scoped_source_uuid(
        CaptureProvider::Mux,
        &provider_session_id,
        MUX_SOURCE_FORMAT,
        Some(&primary_path_display),
    );
    let primary_source_identity = provider_source_identity(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        None,
        Some(&primary_path_display),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Mux released primary source identity is unavailable",
    ))?;
    Ok(Some(MuxLegacyBridge {
        primary_path: primary_path.clone(),
        primary_source_id,
        primary_source_identity,
        provider_session_id,
        partial_disposition: layout.partial_disposition,
    }))
}

pub(super) fn released_mux_cursor_matches_source(cursor: &str, source: &MuxSessionSource) -> bool {
    if cursor.trim_start().starts_with('{') || decode_native_path_committed_cursor(cursor).is_ok() {
        return false;
    }
    let Some((path, line)) = cursor.rsplit_once(":line:") else {
        return false;
    };
    if line.parse::<usize>().ok().is_none_or(|line| line == 0) {
        return false;
    }
    [source.chat_path.as_ref(), source.partial_path.as_ref()]
        .into_iter()
        .flatten()
        .any(|candidate| candidate.display().to_string() == path)
}

struct MuxReleasedLayout {
    provider_session_id: Option<String>,
    partial_disposition: MuxLegacyPartialDisposition,
}

fn scan_released_mux_layout(source: &MuxSessionSource) -> Result<MuxReleasedLayout> {
    let partial = source
        .partial_path
        .as_deref()
        .map(read_released_partial_value)
        .transpose()?
        .flatten();
    let partial_sequence = partial.as_ref().and_then(mux_history_sequence);
    let partial_parts = partial.as_ref().map_or(0, mux_legacy_parts_len);
    let partial_workspace = partial.as_ref().and_then(mux_workspace_id);

    let mut chat_rows = 0_u64;
    let mut matching_chat = None;
    let mut insert_at = None;
    let mut workspace_candidates = Vec::with_capacity(2);
    if let Some(chat_path) = source.chat_path.as_deref() {
        let mut reader = BufReader::new(File::open(chat_path)?);
        let mut hasher = Sha256::new();
        let mut offset = 0_u64;
        while let Some(record) = read_bounded_record(&mut reader, &mut hasher, offset)? {
            offset = record.end;
            if record.oversized || record.payload.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<Value>(&record.payload) else {
                continue;
            };
            if !value.is_object() {
                continue;
            }
            let rank = chat_rows;
            if workspace_candidates.len() < 2 {
                if let Some(workspace) = mux_workspace_id(&value) {
                    workspace_candidates.push((rank, workspace));
                }
            }
            if let Some(sequence) = partial_sequence {
                let chat_sequence = mux_history_sequence(&value);
                if matching_chat.is_none() && chat_sequence == Some(sequence) {
                    matching_chat = Some((rank, mux_legacy_parts_len(&value)));
                }
                if insert_at.is_none() && chat_sequence.is_some_and(|value| value > sequence) {
                    insert_at = Some(rank);
                }
            }
            chat_rows = chat_rows
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Mux released merged row count overflowed",
                ))?;
        }
    }

    let partial_disposition = match (partial.as_ref(), partial_sequence, matching_chat) {
        (None, _, _) => MuxLegacyPartialDisposition::None,
        (Some(_), Some(_), Some((chat_rank, chat_parts))) if partial_parts > chat_parts => {
            MuxLegacyPartialDisposition::Replace { chat_rank }
        }
        (Some(_), Some(_), Some(_)) => MuxLegacyPartialDisposition::Ignored,
        (Some(_), Some(_), None) => MuxLegacyPartialDisposition::Insert {
            merged_index: insert_at.unwrap_or(chat_rows),
        },
        (Some(_), None, _) => MuxLegacyPartialDisposition::Insert {
            merged_index: chat_rows,
        },
    };
    let replaced_rank = match partial_disposition {
        MuxLegacyPartialDisposition::Replace { chat_rank } => Some(chat_rank),
        _ => None,
    };
    let first_chat_workspace = workspace_candidates
        .into_iter()
        .find(|(rank, _)| Some(*rank) != replaced_rank);
    let partial_workspace = match partial_disposition {
        MuxLegacyPartialDisposition::Replace { chat_rank } => {
            partial_workspace.map(|workspace| (chat_rank, workspace))
        }
        MuxLegacyPartialDisposition::Insert { merged_index } => {
            partial_workspace.map(|workspace| (merged_index, workspace))
        }
        MuxLegacyPartialDisposition::None | MuxLegacyPartialDisposition::Ignored => None,
    };
    let provider_session_id = match (partial_workspace, first_chat_workspace) {
        (Some(partial), Some(chat)) if partial.0 <= chat.0 => Some(partial.1),
        (_, Some(chat)) => Some(chat.1),
        (Some(partial), None) => Some(partial.1),
        (None, None) => None,
    };
    Ok(MuxReleasedLayout {
        provider_session_id,
        partial_disposition,
    })
}

pub(super) fn read_released_partial_value(path: &Path) -> Result<Option<Value>> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let Some(record) = read_bounded_whole_record(&mut reader, &mut hasher, 0)? else {
        return Ok(None);
    };
    if record.oversized || record.payload.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    Ok(serde_json::from_slice::<Value>(&record.payload)
        .ok()
        .filter(Value::is_object))
}

pub(super) fn mux_workspace_id(value: &Value) -> Option<String> {
    value
        .get("workspaceId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn mux_legacy_parts_len(value: &Value) -> usize {
    value
        .get("parts")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

pub(super) fn legacy_valid_rows_before(path: &Path, byte_offset: u64) -> Result<u64> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut offset = 0_u64;
    let mut valid_rows = 0_u64;
    while offset < byte_offset {
        let record = read_bounded_record(&mut reader, &mut hasher, offset)?.ok_or(
            CaptureError::InvalidPayload(
                "Mux legacy cursor frontier exceeds its source".to_owned(),
            ),
        )?;
        if record.end > byte_offset {
            return Err(CaptureError::InvalidPayload(
                "Mux legacy cursor frontier splits a physical record".to_owned(),
            ));
        }
        offset = record.end;
        if !record.oversized
            && !record.payload.iter().all(u8::is_ascii_whitespace)
            && serde_json::from_slice::<Value>(&record.payload).is_ok_and(|value| value.is_object())
        {
            valid_rows = valid_rows
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Mux legacy merged row count overflowed",
                ))?;
        }
    }
    Ok(valid_rows)
}

pub(super) fn plan_source(
    store: &Store,
    configured_root: &Path,
    source: MuxSessionSource,
    path: PathBuf,
    kind: MuxStreamKind,
    context: &ProviderAdapterContext,
    legacy_bridge: Option<MuxLegacyBridge>,
) -> Result<MuxSourcePlan> {
    let observation = MuxFileObservation::read(&path, source.metadata_path.as_deref())?;
    let path_identity = provider_path_identity(&observation.canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        &path_identity,
    );
    let canonical_source_identity = legacy_bridge
        .as_ref()
        .filter(|bridge| bridge.primary_path == path)
        .map(|bridge| bridge.primary_source_identity.clone())
        .unwrap_or_else(|| mux_canonical_source_identity(configured_root, &path_identity));
    let source_revision = observation.source_revision(kind.label());
    let metadata_revision = observation.metadata_revision();
    let prior = load_source_cursor(store, &context.machine_id, &cursor_stream)?;
    let mut generation = 0;
    let mut initial_frontier = MuxFrontier::initial();
    let mut accepted_events = 0;
    let mut rejected_records = 0;
    let mut first_failure = None;
    if let Some(loaded) = prior.as_ref() {
        if let Some(wire) = loaded.wire.as_ref() {
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
    }
    if kind == MuxStreamKind::Chat && legacy_bridge.is_some() {
        initial_frontier.legacy_valid_rows = Some(match initial_frontier.legacy_valid_rows {
            Some(rows) => rows,
            None => legacy_valid_rows_before(&path, initial_frontier.next_offset)?,
        });
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
        legacy_bridge,
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
    let wire = match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => Some(
            serde_json::from_str::<MuxCursorWire>(committed.provider_cursor()).map_err(|_| {
                CaptureError::InvalidPayload("Mux NativePath cursor is corrupt".to_owned())
            })?,
        ),
        Err(_) => {
            // Released pre-NativePath cursors are accepted only as a migration
            // signal. Their parser position is never resumed by NativePath.
            match crate::provider::importer::CertifiedProviderCursor::decode_if_certified(
                &stored.cursor,
            )? {
                Some(_) => None,
                None => {
                    return Err(CaptureError::InvalidPayload(
                        "Mux cursor is neither NativePath nor a released migration cursor"
                            .to_owned(),
                    ));
                }
            }
        }
    };
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

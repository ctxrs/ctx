use super::*;

pub(super) fn encode_cursor(checkpoint: &Checkpoint) -> Result<String> {
    Ok(serde_json::to_string(&CursorWire {
        version: CURSOR_VERSION,
        kind: "openclaw-nativepath-jsonl".to_owned(),
        checkpoint: checkpoint.clone(),
    })?)
}

pub(super) fn decode_cursor(
    encoded_store_cursor: &str,
    path: &Path,
    observation: &OpenClawSessionObservation,
) -> Result<CursorDecode> {
    let encoded = decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    if let Ok(wire) = serde_json::from_str::<CursorWire>(&encoded) {
        if wire.version == CURSOR_VERSION
            && wire.kind == "openclaw-nativepath-jsonl"
            && wire.checkpoint.supported()
        {
            return Ok(CursorDecode::Native(wire.checkpoint));
        }
        return Ok(CursorDecode::Reset);
    }
    migrate_released_cursor(&encoded, path, observation)
}

pub(super) fn committed_replay_authority(
    store: &Store,
    machine_id: &str,
    path: &Path,
) -> Result<Checkpoint> {
    let observation = OpenClawSessionObservation::read(path)?;
    let canonical_path = observation.canonical_path.clone();
    let locator_identity = provider_path_identity(&canonical_path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store
        .get_sync_cursor(None, machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "OpenClaw output replay requires committed terminal NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenClaw output replay requires a Store-committed NativePath Core cursor".to_owned(),
        )
    })?;
    let wire: CursorWire = serde_json::from_str(committed.provider_cursor()).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenClaw output replay requires committed OpenClaw Core authority".to_owned(),
        )
    })?;
    if wire.version != CURSOR_VERSION
        || wire.kind != "openclaw-nativepath-jsonl"
        || !wire.checkpoint.supported()
        || !wire.checkpoint.terminal
        || wire.checkpoint.source_path != canonical_path
        || !wire
            .checkpoint
            .source_observation
            .matches_live(&observation)?
        || wire.checkpoint.complete_prefix_end != observation.transcript.length
    {
        return Err(CaptureError::InvalidPayload(
            "OpenClaw output replay source does not exactly match committed terminal Core authority"
                .to_owned(),
        ));
    }
    let live_prefix_sha256 = prefix_sha256(path, wire.checkpoint.complete_prefix_end)?;
    let revalidated = OpenClawSessionObservation::read(path)?;
    if live_prefix_sha256 != wire.checkpoint.complete_prefix_sha256
        || !wire
            .checkpoint
            .source_observation
            .matches_live(&revalidated)?
    {
        return Err(CaptureError::InvalidPayload(
            "OpenClaw output replay content does not match committed terminal Core authority"
                .to_owned(),
        ));
    }
    Ok(wire.checkpoint)
}

pub(super) fn replay_checkpoint_is_covered_by(
    authority: &Checkpoint,
    candidate: &Checkpoint,
) -> bool {
    authority.version == candidate.version
        && authority.parser_revision == candidate.parser_revision
        && authority.policy_revision == candidate.policy_revision
        && authority.source_path == candidate.source_path
        && authority.source_observation == candidate.source_observation
        && authority.complete_prefix_end >= candidate.complete_prefix_end
        && authority.next_raw_ordinal >= candidate.next_raw_ordinal
        && authority.accepted_events >= candidate.accepted_events
        && authority.accepted_file_touches >= candidate.accepted_file_touches
        && authority.rejected_records >= candidate.rejected_records
        && (authority.complete_prefix_end != candidate.complete_prefix_end
            || authority.complete_prefix_sha256 == candidate.complete_prefix_sha256)
        && (!candidate.terminal || authority.terminal)
}

pub(super) fn migrate_released_cursor(
    encoded: &str,
    path: &Path,
    observation: &OpenClawSessionObservation,
) -> Result<CursorDecode> {
    let Some(released) = CertifiedProviderCursor::decode_if_certified(encoded)? else {
        return Ok(CursorDecode::Reset);
    };
    if released.parser_revision() != OPENCLAW_RELEASED_CAPTURE_REVISION
        || released.policy_revision() != OPENCLAW_RELEASED_POLICY_REVISION
        || !released
            .source_revision()
            .starts_with("openclaw-jsonl-metadata-v1:")
    {
        return Ok(CursorDecode::Reset);
    }
    let complete_prefix_end = released_jsonl_position_offset(released.native_position())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if complete_prefix_end > observation.transcript.length
        || !released_jsonl_boundary_matches(path, released.native_position())?
    {
        return Ok(CursorDecode::Reset);
    }
    let legacy: ReleasedParserCheckpoint = released.parser_checkpoint().deserialize()?;
    if legacy.session.index_revision != observation.index_revision {
        return Ok(CursorDecode::Reset);
    }
    let agent_id = legacy.session.agent_id;
    let parent_provider_session_id = related_session_id(
        &observation.index,
        agent_id.as_deref(),
        &["parentSessionId", "parent_session_id"],
    );
    let root_provider_session_id = related_session_id(
        &observation.index,
        agent_id.as_deref(),
        &["rootSessionId", "root_session_id"],
    )
    .or_else(|| parent_provider_session_id.clone());
    Ok(CursorDecode::Migrated(Checkpoint {
        version: CURSOR_VERSION,
        parser_revision: PARSER_REVISION,
        policy_revision: POLICY_REVISION,
        generation: 0,
        source_path: fs::canonicalize(path)?,
        source_observation: SourceObservation::from_live(observation),
        route_source_revision: observation.source_revision(),
        complete_prefix_end,
        complete_prefix_sha256: prefix_sha256(path, complete_prefix_end)?,
        next_raw_ordinal: legacy.next_ordinal,
        accepted_events: legacy.accepted_events,
        accepted_file_touches: 0,
        rejected_records: released.rejected_records(),
        session: SessionCursor {
            provider_session_id: legacy.session.provider_session_id,
            agent_id,
            parent_provider_session_id,
            root_provider_session_id,
            started_at: legacy.session.started_at,
            cwd: legacy.session.cwd,
            header_anchor: legacy.header_anchor,
        },
        terminal: complete_prefix_end == observation.transcript.length,
    }))
}

pub(super) fn released_jsonl_boundary_matches(
    path: &Path,
    position: &crate::native_source::NativePosition,
) -> Result<bool> {
    // Released OpenClaw cursors certified the final 64 KiB before their
    // frontier. The path-scoped cursor stream supplies exact-path authority;
    // this proof is what allows an ordinary metadata revision change caused
    // by append without trusting mtime.
    let offset = released_jsonl_position_offset(position)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let value = position.value();
    if value.len() != RELEASED_POSITION_ENCODED_BYTES {
        return Ok(false);
    }
    let proof_length = u32::from_be_bytes(
        value[RELEASED_POSITION_PROOF_LENGTH_START..RELEASED_POSITION_DIGEST_START]
            .try_into()
            .map_err(|_| {
                CaptureError::InvalidPayload(
                    "released OpenClaw JSONL proof length is malformed".to_owned(),
                )
            })?,
    );
    if u64::from(proof_length) != offset.min(RELEASED_BOUNDARY_MAX_BYTES) {
        return Ok(false);
    }
    let proof_start =
        offset
            .checked_sub(u64::from(proof_length))
            .ok_or(CaptureError::SystemInvariant(
                "released OpenClaw JSONL proof starts before the source",
            ))?;
    let mut proof = vec![
        0_u8;
        usize::try_from(proof_length).map_err(|_| {
            CaptureError::SystemInvariant("released OpenClaw JSONL proof length exceeds usize")
        })?
    ];
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(proof_start))?;
    if let Err(error) = file.read_exact(&mut proof) {
        return match error.kind() {
            io::ErrorKind::UnexpectedEof => Ok(false),
            _ => Err(error.into()),
        };
    }
    let mut hasher = Sha256::new();
    hasher.update(RELEASED_BOUNDARY_HASH_DOMAIN);
    hasher.update(offset.to_be_bytes());
    hasher.update(proof_length.to_be_bytes());
    hasher.update(&proof);
    let digest: [u8; 32] = hasher.finalize().into();
    Ok(digest.as_slice() == &value[RELEASED_POSITION_DIGEST_START..])
}

use super::*;

pub(super) fn event_identity_raw_source_path(
    store: &Store,
    authority: &SourceAuthority,
    canonical_source_identity: &str,
) -> Result<String> {
    let source_id = authority.shared_source_id(canonical_source_identity);
    match store.get_capture_source(source_id) {
        Ok(source) => Ok(source
            .descriptor
            .raw_source_path
            .or(source.descriptor.source_root)
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| authority.raw_source_path.clone())),
        Err(StoreError::NotFound(_)) => Ok(authority.raw_source_path.clone()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn plan_cursor(
    authority: &SourceAuthority,
    stored: StoredCursor,
    digest: &SourceDigest,
    event_identity_raw_source_path: String,
) -> Result<PromptHistoryCursor> {
    let (prior, migration) = match stored {
        StoredCursor::None => (None, false),
        StoredCursor::Released => (None, true),
        StoredCursor::Native { cursor } => (Some(cursor), false),
    };
    if let Some(prior) = &prior {
        prior.validate_route(authority)?;
    }
    let lifecycle = match prior.as_ref() {
        None if migration => Lifecycle::Migration,
        None => Lifecycle::Fresh,
        Some(previous) if !previous.observation.same_file(&digest.observation) => {
            Lifecycle::Replacement
        }
        Some(previous) if digest.observation.len < previous.observation.len => {
            Lifecycle::Truncation
        }
        Some(previous)
            if digest.observation.len > previous.observation.len
                && revision_inventory_authority(&digest.revision)
                    == revision_inventory_authority(&previous.source_revision)
                && digest.prefix_at_prior_len
                    == Some(revision_bytes(&previous.source_revision)?) =>
        {
            Lifecycle::Append
        }
        Some(_) => Lifecycle::Rewrite,
    };
    let resume = prior.as_ref().is_some_and(|previous| {
        previous.source_revision == digest.revision
            && matches!(
                previous.phase,
                CursorPhase::Core { .. } | CursorPhase::Retiring { missing: false, .. }
            )
    });
    if resume {
        return prior.ok_or(CaptureError::SystemInvariant(
            "Codex prompt-history resume cursor disappeared",
        ));
    }
    let generation = match prior.as_ref() {
        Some(previous) => {
            previous
                .generation
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Codex prompt-history generation exhausted",
                ))?
        }
        None => 0,
    };
    let canonical_source_identity = prior
        .as_ref()
        .map(|previous| previous.canonical_source_identity.clone())
        .unwrap_or_else(|| authority.canonical_source_identity.clone());
    let capture_source_id = authority.shared_source_id(&canonical_source_identity);
    Ok(PromptHistoryCursor {
        version: CURSOR_VERSION,
        parser_revision: PARSER_REVISION.to_owned(),
        policy_revision: POLICY_REVISION.to_owned(),
        route_identity: authority.route_identity.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        canonical_source_identity,
        capture_source_id,
        event_identity_raw_source_path: prior
            .as_ref()
            .and_then(|previous| previous.event_identity_raw_source_path.clone())
            .or(Some(event_identity_raw_source_path)),
        source_revision: digest.revision.clone(),
        generation,
        generation_id: generation_id(generation, &digest.revision, false),
        observation: digest.observation.clone(),
        lifecycle,
        accepted_events: 0,
        session_runs: 0,
        rejected_records: 0,
        ignored_records: 0,
        last_session_hash: None,
        phase: CursorPhase::Core {
            next_offset: 0,
            next_ordinal: 0,
            prefix_sha256: Sha256::digest([]).into(),
        },
    })
}

pub(super) fn load_cursor(store: &Store, machine_id: &str, stream: &str) -> Result<StoredCursor> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(StoredCursor::None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        return Ok(StoredCursor::Native {
            cursor: PromptHistoryCursor::decode(committed.provider_cursor())?,
        });
    }
    if crate::provider::importer::CertifiedProviderCursor::decode_if_certified(&stored.cursor)?
        .is_some()
    {
        return Ok(StoredCursor::Released);
    }
    Err(CaptureError::InvalidPayload(
        "Codex prompt-history cursor is neither NativePath nor a released migration cursor"
            .to_owned(),
    ))
}

pub(super) fn digest_source(
    authority: &SourceAuthority,
    prior_len: Option<u64>,
    inventory_observation_token: Option<&str>,
) -> Result<SourceDigest> {
    let source = authority.opened()?;
    let observation = FileObservation::from_metadata(source.metadata())?;
    let file = open_prompt_history_source(source)?;
    if FileObservation::from_metadata(&file.metadata()?)? != observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut reader = BufReader::new(file);
    let mut full = Sha256::new();
    let mut prefix = Sha256::new();
    let mut read = 0_u64;
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        full.update(&bytes[..count]);
        if let Some(target) = prior_len {
            let remaining = target.saturating_sub(read);
            let take = count.min(usize::try_from(remaining).unwrap_or(usize::MAX));
            prefix.update(&bytes[..take]);
        }
        read = read
            .checked_add(u64::try_from(count).map_err(|_| {
                CaptureError::SystemInvariant("Codex prompt-history source length exceeds u64")
            })?)
            .ok_or(CaptureError::SystemInvariant(
                "Codex prompt-history source length overflowed",
            ))?;
    }
    if read != observation.len || !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let hash: [u8; 32] = full.finalize().into();
    Ok(SourceDigest {
        observation,
        revision: revision_string(&hash, inventory_observation_token),
        prefix_at_prior_len: prior_len.map(|_| prefix.finalize().into()),
    })
}

pub(super) fn read_record(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
) -> Result<Option<RawRecord>> {
    let mut bytes = Vec::new();
    let mut observed = 0_usize;
    let mut saw_any = false;
    let mut terminated = false;
    while !terminated {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        saw_any = true;
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        observed = observed.saturating_add(chunk.len());
        if bytes.len() <= MAX_PROVIDER_JSONL_LINE_BYTES {
            let remaining = MAX_PROVIDER_JSONL_LINE_BYTES
                .saturating_add(1)
                .saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
        terminated = chunk.last() == Some(&b'\n');
        reader.consume(take);
    }
    if !saw_any {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    Ok(Some(RawRecord {
        bytes,
        observed_bytes: observed,
        terminated,
    }))
}

pub(super) fn hash_prefix_and_seek(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    target: u64,
) -> Result<()> {
    let mut remaining = target;
    let mut bytes = [0_u8; 64 * 1024];
    while remaining > 0 {
        let take = bytes
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let count = reader.read(&mut bytes[..take])?;
        if count == 0 {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history cursor exceeds its source".to_owned(),
            ));
        }
        hasher.update(&bytes[..count]);
        remaining = remaining.saturating_sub(u64::try_from(count).unwrap_or(u64::MAX));
    }
    Ok(())
}

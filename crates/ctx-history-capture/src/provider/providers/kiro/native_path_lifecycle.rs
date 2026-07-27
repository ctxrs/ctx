use super::*;

pub(super) fn retire_pending_generation(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &KiroSource,
    context: &ProviderAdapterContext,
    work_limit: CaptureWorkLimit,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    let mut actual_groups = 0_usize;
    let mut next_request: Option<Option<KiroRetirementFrontier>> = None;
    loop {
        source.revalidate()?;
        let stored = store
            .get_sync_cursor(None, &context.machine_id, &source.cursor_stream)?
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Kiro generation retirement has no committed Core cursor".to_owned(),
                )
            })?;
        let committed = decode_native_path_committed_cursor(&stored.cursor)?;
        let cursor = KiroStoreCursor::decode(committed.provider_cursor())?;
        if cursor.locator_identity != source.locator_identity
            || cursor.source_revision != source.source_revision
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let (request_after, recovery) = match next_request.take() {
            Some(after) => (after, false),
            None => {
                let Some(request) = cursor.retirement.clone() else {
                    if cursor.terminal {
                        break;
                    }
                    return Err(CaptureError::InvalidPayload(
                        "Kiro cursor is neither scanning, retiring, nor terminal".to_owned(),
                    ));
                };
                (request.after, request.committed)
            }
        };
        let page = publish_retirement_request(
            store,
            bulk_guard,
            source,
            context,
            &stored,
            &cursor,
            request_after.clone(),
            recovery,
        )?;
        if !recovery {
            actual_groups = actual_groups.saturating_add(1);
        }
        summary.set_work_result(ProviderImportWorkResult::Changed);
        if page.done {
            publish_terminal_generation_cursor(store, bulk_guard, source, context)?;
            summary.work_remaining = false;
            break;
        }
        let after = page.next_after.ok_or(CaptureError::SystemInvariant(
            "Kiro nonterminal retirement page has no next frontier",
        ))?;
        if work_limit == CaptureWorkLimit::OneSafeGroup && actual_groups != 0 {
            summary.work_remaining = true;
            break;
        }
        next_request = Some(Some(KiroRetirementFrontier::from_store(after)));
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn publish_retirement_request(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &KiroSource,
    context: &ProviderAdapterContext,
    stored: &SyncCursor,
    cursor: &KiroStoreCursor,
    request_after: Option<KiroRetirementFrontier>,
    recovery: bool,
) -> Result<NativePathSourceRetirementPage> {
    let mut next_cursor = cursor.clone();
    next_cursor.retirement = Some(KiroRetirementRequest {
        after: request_after.clone(),
        committed: true,
    });
    next_cursor.terminal = false;
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor.clone()),
        SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: context.machine_id.clone(),
            stream: source.cursor_stream.clone(),
            cursor: next_cursor.encode()?,
            last_synced_at: Some(context.imported_at),
            timestamps: timestamps(context.imported_at),
        },
    );
    let publication_id =
        retirement_request_publication_id(source, cursor, &request_after, recovery, stored);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(0, 1, KIRO_PAGE_BASE_BYTES)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Err(CaptureError::SystemInvariant(
            "Kiro retirement recovery publication did not advance",
        ));
    }
    let resolution =
        group.reconcile_provider_source_locator(&kiro_locator_observation(source, context)?)?;
    if resolution.canonical_source_identity != cursor.canonical_source_identity {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let generation_key = kiro_generation_key(
        source,
        context,
        &resolution.canonical_source_identity,
        cursor.generation,
    );
    let after = request_after
        .as_ref()
        .map(KiroRetirementFrontier::to_store)
        .transpose()?;
    let page = group.retire_source_generation_page(
        &generation_key,
        after.as_ref(),
        KIRO_RETIREMENT_PAGE_ENTITIES,
        context.imported_at.timestamp_millis(),
    )?;
    source.revalidate()?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok(page)
}

fn publish_terminal_generation_cursor(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &KiroSource,
    context: &ProviderAdapterContext,
) -> Result<()> {
    source.revalidate()?;
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &source.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Kiro terminal generation publication has no Core cursor".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let mut cursor = KiroStoreCursor::decode(committed.provider_cursor())?;
    if cursor.locator_identity != source.locator_identity
        || cursor.source_revision != source.source_revision
        || cursor.retirement.is_none()
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    cursor.retirement = None;
    cursor.terminal = true;
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor.clone()),
        SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: context.machine_id.clone(),
            stream: source.cursor_stream.clone(),
            cursor: cursor.encode()?,
            last_synced_at: Some(context.imported_at),
            timestamps: timestamps(context.imported_at),
        },
    );
    let mut digest = Sha256::new();
    digest.update(b"ctx-kiro-nativepath-generation-terminal-v1\0");
    hash_field(&mut digest, source.locator_identity.as_bytes());
    hash_field(&mut digest, source.source_revision.as_bytes());
    hash_field(&mut digest, stored.cursor.as_bytes());
    let publication_id = format!("kiro-generation-terminal-v1:{}", hex(&digest.finalize()));
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(0, 1, KIRO_PAGE_BASE_BYTES)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(());
    }
    source.revalidate()?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok(())
}

fn retirement_request_publication_id(
    source: &KiroSource,
    cursor: &KiroStoreCursor,
    after: &Option<KiroRetirementFrontier>,
    recovery: bool,
    stored: &SyncCursor,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-kiro-nativepath-generation-retirement-v1\0");
    hash_field(&mut digest, source.locator_identity.as_bytes());
    hash_field(&mut digest, source.source_revision.as_bytes());
    digest.update(cursor.generation.to_be_bytes());
    hash_field(
        &mut digest,
        serde_json::to_string(after).unwrap_or_default().as_bytes(),
    );
    digest.update([u8::from(recovery)]);
    if recovery {
        hash_field(&mut digest, stored.cursor.as_bytes());
    }
    format!("kiro-generation-retirement-v1:{}", hex(&digest.finalize()))
}

pub(super) fn kiro_locator_observation(
    source: &KiroSource,
    context: &ProviderAdapterContext,
) -> Result<ProviderSourceLocatorObservation> {
    let raw_source_path = source.canonical_path.display().to_string();
    let source_root = source.configured_source_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Kiro NativePath source has no canonical identity",
    ))?;
    Ok(ProviderSourceLocatorObservation {
        provider: CaptureProvider::KiroCli,
        source_format: KIRO_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: source.locator_identity.clone(),
        cursor_stream: source.cursor_stream.clone(),
        proposed_source_identity,
        raw_source_path: Some(raw_source_path),
        source_revision: source.source_revision.clone(),
        observed_at_ms: context.imported_at.timestamp_millis(),
    })
}

pub(super) fn retire_missing_kiro_route(
    path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> Result<ProviderImportSummary> {
    let locator_identity = provider_path_identity(path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Kiro SQLite source does not exist",
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = KiroStoreCursor::decode(committed.provider_cursor())?;
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::KiroCli,
        source_format: KIRO_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity,
        cursor_stream: stream.clone(),
        expected_canonical_source_identity: cursor.canonical_source_identity,
        expected_source_revision: cursor.source_revision,
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::SourceMissing,
    };
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream,
        cursor: committed.provider_cursor().to_owned(),
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let publication_id = retirement_publication_id(&retirement);
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, 0)?,
        )?;
        let changed =
            match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
                NativePathCursorSetClassification::AllExpected => {
                    let disposition = group.retire_provider_source_route(&retirement)?;
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    matches!(
                        disposition,
                        ProviderSourceRouteRetirementDisposition::Retired
                    )
                }
                NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
            };
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(if changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        });
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(KIRO_RETIREMENT_DOMAIN);
    hash_field(&mut digest, retirement.locator_identity.as_bytes());
    hash_field(&mut digest, retirement.cursor_stream.as_bytes());
    hash_field(
        &mut digest,
        retirement.expected_canonical_source_identity.as_bytes(),
    );
    hash_field(&mut digest, retirement.expected_source_revision.as_bytes());
    format!("kiro-retirement-v1:{}", hex(&digest.finalize()))
}

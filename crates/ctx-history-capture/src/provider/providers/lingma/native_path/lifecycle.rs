use super::{
    records::{hash_bytes, hash_optional_bytes},
    *,
};

#[derive(Clone)]
pub(super) struct KnownRoute {
    raw_source_path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    cursor: SyncCursor,
}

pub(super) fn retire_missing_source(
    requested_path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> Result<ProviderImportSummary> {
    let requested = requested_path.display().to_string();
    let requested_absolute = std::path::absolute(requested_path)?;
    let mut known = Vec::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Lingma
            || source.descriptor.machine_id != context.machine_id
            || source.descriptor.source_format.as_deref() != Some(LINGMA_SQLITE_SOURCE_FORMAT)
        {
            continue;
        }
        let Some(raw_source_path) = source.descriptor.raw_source_path.as_deref() else {
            continue;
        };
        let display_source_path = source
            .sync
            .metadata
            .get("display_source_path")
            .and_then(Value::as_str);
        if Path::new(raw_source_path) != requested_absolute
            && display_source_path != Some(requested.as_str())
        {
            continue;
        }
        let Some(canonical_source_identity) = source.descriptor.source_identity.as_deref() else {
            continue;
        };
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let raw_source_path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&raw_source_path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Lingma,
            LINGMA_SQLITE_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(cursor) = store.get_sync_cursor(None, &context.machine_id, &stream)? else {
            continue;
        };
        known.push(KnownRoute {
            raw_source_path,
            locator_identity,
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            cursor,
        });
    }
    if known.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: requested_path.to_path_buf(),
            reason: "Lingma SQLite source does not exist",
        });
    }
    if known.len() != 1 {
        return Err(CaptureError::SystemInvariant(
            "Lingma NativePath found ambiguous current routes for one source",
        ));
    }
    let route = known.pop().ok_or(CaptureError::SystemInvariant(
        "Lingma NativePath missing-route inventory changed",
    ))?;
    let committed = decode_native_path_committed_cursor(&route.cursor.cursor)?;
    let mut checkpoint: CoreCheckpoint = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| {
            CaptureError::InvalidPayload(
                "Lingma retirement requires a committed Lingma Core cursor".to_owned(),
            )
        })?;
    checkpoint.validate(&route.locator_identity)?;
    checkpoint.terminal = false;
    let transition = NativePathCursorTransition::new(
        Some(route.cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            route.cursor.stream.clone(),
            checkpoint.encode()?,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Lingma,
        source_format: LINGMA_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity,
        cursor_stream: route.cursor.stream,
        expected_canonical_source_identity: route.canonical_source_identity,
        expected_source_revision: route.source_revision,
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: if context
            .source_root
            .as_deref()
            .is_some_and(|root| !root.exists())
        {
            ProviderSourceRouteRetirementReason::RootMissing
        } else {
            ProviderSourceRouteRetirementReason::SourceMissing
        },
    };
    let publication_id = retirement_publication_id(&retirement, &route.raw_source_path);
    if committed.publication_id() == publication_id {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
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
        let mut summary = ProviderImportSummary {
            skipped: usize::from(changed),
            skipped_sessions: usize::from(changed),
            ..ProviderImportSummary::default()
        };
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

pub(super) fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Lingma.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

pub(super) fn publication_id(
    authority: &SourceAuthority,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(PUBLICATION_DOMAIN);
    hash_bytes(&mut digest, authority.locator_identity.as_bytes());
    hash_bytes(&mut digest, authority.source_revision.as_bytes());
    hash_bytes(&mut digest, transition.key().stream().as_bytes());
    hash_optional_bytes(&mut digest, transition.expected_cursor().map(str::as_bytes));
    hash_bytes(&mut digest, transition.next().cursor.as_bytes());
    format!("lingma-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
    path: &Path,
) -> String {
    let mut digest = Sha256::new();
    digest.update(RETIREMENT_DOMAIN);
    hash_bytes(&mut digest, retirement.machine_id.as_bytes());
    hash_bytes(&mut digest, retirement.locator_identity.as_bytes());
    hash_bytes(
        &mut digest,
        retirement.expected_canonical_source_identity.as_bytes(),
    );
    hash_bytes(&mut digest, retirement.expected_source_revision.as_bytes());
    hash_bytes(&mut digest, path.as_os_str().as_encoded_bytes());
    format!("lingma-nativepath-retirement-v1:{:x}", digest.finalize())
}

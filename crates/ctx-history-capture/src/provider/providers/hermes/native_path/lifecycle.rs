use super::{cursor::*, *};

pub(super) fn retire_missing_source(
    store: &mut Store,
    path: &Path,
    adapter: &ProviderAdapterContext,
) -> Result<()> {
    let locator_identity = provider_path_identity(path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &adapter.machine_id, &stream)? else {
        return Ok(());
    };
    let committed = match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => committed,
        Err(_) => return Ok(()),
    };
    let mut cursor: HermesStoreCursor = serde_json::from_str(committed.provider_cursor())?;
    validate_cursor(&cursor)?;
    if cursor.retired {
        return Ok(());
    }
    cursor.retired = true;
    cursor.terminal = true;
    let next = SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Hermes.as_str(),
                adapter.machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: adapter.machine_id.clone(),
        stream: stream.clone(),
        cursor: serde_json::to_string(&cursor)?,
        last_synced_at: Some(adapter.imported_at),
        timestamps: timestamps(adapter.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor.clone()), next);
    let publication_id = publication_id(&transition, &cursor);
    let guard = store.begin_event_search_bulk_mode()?;
    let operation: Result<()> = (|| {
        let admission = store.admit_event_search_bulk_group(&guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(1, 1, 0)?,
        )?;
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => {}
            NativePathCursorSetClassification::AllExpected => {
                group.retire_provider_source_route(&ProviderSourceRouteRetirement {
                    provider: CaptureProvider::Hermes,
                    source_format: HERMES_SQLITE_SOURCE_FORMAT.to_owned(),
                    machine_id: adapter.machine_id.clone(),
                    locator_identity: cursor.locator_identity.clone(),
                    cursor_stream: stream,
                    expected_canonical_source_identity: cursor.canonical_source_identity.clone(),
                    expected_source_revision: cursor.source_revision.clone(),
                    retired_at_ms: adapter.imported_at.timestamp_millis(),
                    reason: if adapter
                        .source_root
                        .as_ref()
                        .is_some_and(|root| !root.exists())
                    {
                        ProviderSourceRouteRetirementReason::RootMissing
                    } else {
                        ProviderSourceRouteRetirementReason::SourceMissing
                    },
                })?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
            }
        }
        group.commit()?;
        Ok(())
    })();
    let finish = store
        .finish_event_search_bulk_mode(&guard)
        .map_err(CaptureError::from);
    operation?;
    finish
}

pub(super) fn absolute_path(path: &Path) -> Result<std::path::PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(super) fn canonical_source_path(path: &Path) -> Result<std::path::PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Hermes SQLite source has no parent directory",
        })?;
    let file_name =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Hermes SQLite source has no file name",
            })?;
    Ok(fs::canonicalize(parent)?.join(file_name))
}

use super::*;

#[derive(Debug)]
pub(in crate::provider::codex::nativepath) struct CodexNativeRootPublication {
    pub(super) published_cursors: Vec<SyncCursor>,
    pub(in super::super) imported_events: usize,
    pub(in super::super) skipped_events: usize,
}

/// Publishes one already bounded Codex root group.
///
/// Group membership is decided entirely by the Store's admission bounds, which
/// the producer respects when it merges windows. The Store no longer rejects an
/// admitted group for derived projection-journal volume, so there is no
/// retry-by-splitting ladder here: a group that is admitted commits.
pub(super) fn publish_root_group_bounded(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    chunks: Vec<CodexNativeRootChunk>,
) -> VerticalResult<CodexNativeRootPublication> {
    publish_root_group_once(store, bulk_guard, &chunks)
}

pub(super) fn cursor_key_matches(left: &SyncCursor, right: &SyncCursor) -> bool {
    left.team_id == right.team_id
        && left.device_id == right.device_id
        && left.stream == right.stream
}

pub(super) fn exact_cursor_for_key<'a>(
    cursors: &'a [SyncCursor],
    key: &SyncCursor,
) -> Option<&'a SyncCursor> {
    let mut matches = cursors
        .iter()
        .filter(|cursor| cursor_key_matches(cursor, key));
    let cursor = matches.next()?;
    matches.next().is_none().then_some(cursor)
}

pub(super) fn publish_root_group_once(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    chunks: &[CodexNativeRootChunk],
) -> VerticalResult<CodexNativeRootPublication> {
    let pages = chunks.iter().map(|chunk| chunk.pages.len()).sum::<usize>();
    let serialized_bytes = chunks
        .iter()
        .map(|chunk| chunk.serialized_bytes)
        .sum::<usize>();
    let accounting = NativePathGroupAccounting::new(pages, chunks.len(), serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut publication = store.begin_native_path_publication_group(admission, accounting)?;
    let transitions = chunks
        .iter()
        .map(|chunk| {
            NativePathCursorTransition::new(
                chunk
                    .expected_store_cursor
                    .as_ref()
                    .map(|cursor| cursor.cursor.clone()),
                chunk.next_store_cursor.clone(),
            )
        })
        .collect::<Vec<_>>();
    let publication_id = root_publication_id(chunks);
    let classification = publication.classify_cursor_set(&publication_id, &transitions)?;
    let mut write = CodexNativeCoreWrite::default();
    match classification {
        NativePathCursorSetClassification::AllExpected => {
            for chunk in chunks {
                let chunk_write =
                    write_raw_core(store, &mut publication, &chunk.context, &chunk.pages)?;
                if chunk.expected_store_cursor.is_none()
                    && chunk.pages.iter().all(|page| page.core_rows.is_empty())
                {
                    return Err(CodexNativeVerticalError::CorruptFrontier(
                        "fresh Codex source cannot publish cursor-only authority",
                    ));
                }
                write.imported_events = write
                    .imported_events
                    .saturating_add(chunk_write.imported_events);
                write.skipped_events = write
                    .skipped_events
                    .saturating_add(chunk_write.skipped_events);
            }
            publication.prepare_journal_checkpoint()?;
            for chunk in chunks {
                revalidate_codex_source_observation(
                    &chunk.context.source,
                    &chunk.context.certified_observation,
                )?;
            }
            publication.publish_cursor_set()?;
        }
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            write.skipped_events = chunks
                .iter()
                .flat_map(|chunk| &chunk.pages)
                .map(|page| page.core_rows.len())
                .sum();
            for chunk in chunks {
                revalidate_codex_source_observation(
                    &chunk.context.source,
                    &chunk.context.certified_observation,
                )?;
            }
        }
    }
    let receipt = publication.commit()?;
    #[cfg(codex_nativepath_qualification)]
    super::super::qualification::observe_store_receipt(&receipt);
    let checkpoint = receipt.checkpoint().cloned();
    if checkpoint.is_none() && !store.native_cold_load_active() {
        return Err(CodexNativeVerticalError::CanonicalJournalInactive);
    }
    let published_cursors = receipt.published_cursors().to_vec();
    if published_cursors.len() != chunks.len()
        || chunks.iter().any(|chunk| {
            exact_cursor_for_key(&published_cursors, &chunk.next_store_cursor)
                .and_then(|cursor| decode_native_path_committed_cursor(&cursor.cursor).ok())
                .is_none_or(|committed| {
                    committed.publication_id() != publication_id
                        || committed.provider_cursor() != chunk.next_store_cursor.cursor
                        || committed.journal_checkpoint() != checkpoint.as_ref()
                })
        })
    {
        return Err(ctx_history_store::StoreError::NativePathCursorConflict.into());
    }
    Ok(CodexNativeRootPublication {
        published_cursors,
        imported_events: write.imported_events,
        skipped_events: write.skipped_events,
    })
}

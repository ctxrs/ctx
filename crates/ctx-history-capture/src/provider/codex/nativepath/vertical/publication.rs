use super::*;

#[derive(Debug)]
pub(in crate::provider::codex::nativepath) struct CodexNativeRootPublication {
    pub(super) published_cursors: Vec<SyncCursor>,
    pub(in super::super) imported_events: usize,
    pub(in super::super) skipped_events: usize,
}

impl CodexNativeRootPublication {
    fn merge(mut self, later: Self) -> Self {
        for cursor in later.published_cursors {
            self.published_cursors
                .retain(|current| !cursor_key_matches(current, &cursor));
            self.published_cursors.push(cursor);
        }
        self.imported_events = self.imported_events.saturating_add(later.imported_events);
        self.skipped_events = self.skipped_events.saturating_add(later.skipped_events);
        self
    }
}

pub(super) fn publish_root_group_bounded(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    mut chunks: Vec<CodexNativeRootChunk>,
) -> VerticalResult<CodexNativeRootPublication> {
    let mut journal_overflow_at = None;
    match publish_root_group_once(store, bulk_guard, &chunks, &mut journal_overflow_at) {
        Ok(checkpoint) => Ok(checkpoint),
        Err(error) => match journal_overflow_at {
            Some(split_at) if split_at > 0 => {
                let remainder = chunks.split_off(split_at);
                // Store rolled the overflowing transaction back. Commit the
                // accepted source prefix, then retry from the rejected source.
                let prefix = publish_root_group_bounded(store, bulk_guard, chunks)?;
                let remainder = publish_root_group_bounded(store, bulk_guard, remainder)?;
                Ok(prefix.merge(remainder))
            }
            Some(0) => {
                let chunk = chunks.remove(0);
                let Some((left, mut right)) = chunk.split_at_page_boundary()? else {
                    return Err(error);
                };
                let left = publish_root_group_bounded(store, bulk_guard, vec![left])?;
                right.bind_exact_expected_cursor(&left)?;
                chunks.insert(0, right);
                let remainder = publish_root_group_bounded(store, bulk_guard, chunks)?;
                Ok(left.merge(remainder))
            }
            _ => Err(error),
        },
    }
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
    journal_overflow_at: &mut Option<usize>,
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
            for (index, chunk) in chunks.iter().enumerate() {
                match write_raw_core(store, &mut publication, &chunk.context, &chunk.pages) {
                    Ok(chunk_write) => {
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
                    Err(error) => {
                        *journal_overflow_at =
                            is_exact_journal_group_overflow(&error).then_some(index);
                        return Err(error);
                    }
                }
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

pub(super) fn is_exact_journal_group_overflow(error: &CodexNativeVerticalError) -> bool {
    matches!(
        error,
        CodexNativeVerticalError::Store(
            ctx_history_store::StoreError::NativePathGroupLimitExceeded {
                limit: "actual journal records" | "uncompressed journal encoding bytes",
                ..
            }
        )
    )
}

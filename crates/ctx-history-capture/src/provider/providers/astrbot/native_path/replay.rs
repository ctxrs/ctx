use super::*;

pub(super) fn replay_outputs(
    path: &Path,
    store: &Store,
    conn: &Connection,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    authority: &SourceAuthority,
    context: &ProviderAdapterContext,
    profile: &ImportProfile,
) -> Result<()> {
    let sink = profile.sink().ok_or(CaptureError::SystemInvariant(
        "AstrBot output replay has no output sink",
    ))?;
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "AstrBot output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let core = AstrBotStoreCursor::decode(committed.provider_cursor())?;
    if core.retired
        || !core.terminal
        || core.source_revision != authority.source_revision
        || core.schema_authority != authority.schema_authority
    {
        return Err(CaptureError::InvalidPayload(
            "AstrBot output replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::AstrBot.as_str().to_owned(),
        namespace_id: core.source_identity.clone(),
        source_id: core.source_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let parser_revision = format!(
        "{OUTPUT_PARSER_REVISION}:capture={ASTRBOT_CAPTURE_REVISION}:policy={ASTRBOT_POLICY_REVISION}"
    );
    let progress_frontier = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .and_then(|cursor| decode_output_frontier(cursor).ok());
    let compatible_progress = progress.as_ref().is_some_and(|progress| {
        progress.observed_revision == authority.source_revision
            && progress.parser_revision == parser_revision
            && progress.materializer_revision == sink.materializer_revision()
            && progress_frontier.is_some()
    });
    if compatible_progress
        && progress.as_ref().is_some_and(|progress| progress.terminal)
        && progress_frontier
            .as_ref()
            .is_some_and(|frontier| frontier == &core.frontier)
    {
        return Ok(());
    }
    let resumable_frontier =
        if compatible_progress && progress.as_ref().is_some_and(|progress| !progress.terminal) {
            match progress_frontier {
                Some(frontier)
                    if frontier.next_native_ordinal <= core.frontier.next_native_ordinal
                        && validate_frontier(conn, &AstrBotSql::new(conn)?, &frontier)? =>
                {
                    Some(frontier)
                }
                _ => None,
            }
        } else {
            None
        };
    let (source_epoch, expected_epoch, expected_cursor, disposition, reader_start) =
        match (&progress, resumable_frontier) {
            (Some(progress), Some(frontier)) => (
                progress.source_epoch,
                Some(progress.source_epoch),
                progress.cursor.clone(),
                ProOutputSourceDisposition::AppendOrResume,
                frontier,
            ),
            (Some(progress), None) => (
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "AstrBot output source epoch exhausted",
                    ))?,
                Some(progress.source_epoch),
                progress.cursor.clone(),
                ProOutputSourceDisposition::Rewrite,
                AstrBotFrontier::initial(),
            ),
            (None, None) => (
                0,
                None,
                None,
                ProOutputSourceDisposition::NewSource,
                AstrBotFrontier::initial(),
            ),
            (None, Some(_)) => {
                return Err(CaptureError::SystemInvariant(
                    "AstrBot output replay derived progress without a sink source",
                ));
            }
        };
    let mut expected_epoch = expected_epoch;
    let mut expected_cursor = expected_cursor;
    let mut disposition = disposition;
    let mut reader = AstrBotReader::new(conn, AstrBotSql::new(conn)?, reader_start);
    while let Some(page) = reader.next_page(true)? {
        if !snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        if let Some(rejection) = page.rejections.first() {
            sink.mark_behind(crate::ProOutputSinkError::new(
                "astrbot_output_incomplete",
                rejection.detail.clone(),
            ));
            return Ok(());
        }
        let next_cursor = encode_output_frontier(&page.next_frontier)?;
        let observations = page
            .outputs
            .into_iter()
            .map(|output| output.observation)
            .collect::<Vec<_>>();
        let materialization = ProOutputMaterializationPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch,
            observed_revision: authority.source_revision.clone(),
            parser_revision: parser_revision.clone(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition,
            expected_prior_source_epoch: expected_epoch,
            expected_prior_cursor: expected_cursor.clone(),
            next_safe_cursor: next_cursor.clone(),
            terminal: page.terminal,
            observations,
        };
        match sink.materialize_page(materialization) {
            Ok(result)
                if result.source_epoch == source_epoch
                    && result.committed_cursor == next_cursor =>
            {
                expected_epoch = Some(source_epoch);
                expected_cursor = Some(next_cursor);
                disposition = ProOutputSourceDisposition::AppendOrResume;
            }
            Ok(_) => {
                sink.mark_behind(crate::ProOutputSinkError::new(
                    "astrbot_output_receipt",
                    "AstrBot output sink returned a mismatched receipt",
                ));
                return Ok(());
            }
            Err(error) => {
                sink.mark_behind(error);
                return Ok(());
            }
        }
    }
    if reader.frontier != core.frontier {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

pub(super) fn retire_missing_source(
    path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> Result<ProviderImportSummary> {
    let locator_identity = provider_path_identity(path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "AstrBot data_v4.db does not exist",
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let mut prior = AstrBotStoreCursor::decode(committed.provider_cursor())?;
    if prior.retired {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::AstrBot,
        source_format: ASTRBOT_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: prior.locator_identity.clone(),
        cursor_stream: cursor_stream.clone(),
        expected_canonical_source_identity: prior.source_identity.clone(),
        expected_source_revision: prior.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::SourceMissing,
    };
    prior.retired = true;
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: cursor_stream,
        cursor: prior.encode()?,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let publication_id = retirement_publication_id(&retirement, transition.next().cursor.as_str());
    let accounting = NativePathGroupAccounting::new(0, 1, transition.next().cursor.len())?;
    let bulk = store.begin_event_search_bulk_mode()?;
    let admission = store.admit_event_search_bulk_group(&bulk)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let disposition =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => {
                group.commit()?;
                ProviderSourceRouteRetirementDisposition::AlreadyRetired
            }
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                group.commit()?;
                disposition
            }
        };
    store.finish_event_search_bulk_mode(&bulk)?;
    let mut summary = ProviderImportSummary::default();
    match disposition {
        ProviderSourceRouteRetirementDisposition::Retired => {
            summary.skipped_sessions = 1;
            summary.skipped = 1;
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        ProviderSourceRouteRetirementDisposition::AlreadyRetired => {
            summary.set_work_result(ProviderImportWorkResult::NoOp);
        }
    }
    Ok(summary)
}

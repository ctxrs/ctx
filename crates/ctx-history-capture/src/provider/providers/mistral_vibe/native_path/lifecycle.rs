use super::*;

pub(super) fn configured_source_root(
    path: &Path,
    context: &ProviderAdapterContext,
    root_missing: bool,
) -> Result<PathBuf> {
    let direct_messages_file = path.file_name().and_then(|name| name.to_str())
        == Some("messages.jsonl")
        && (root_missing || fs::symlink_metadata(path)?.is_file());
    if direct_messages_file {
        let parent = path
            .parent()
            .ok_or(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Mistral Vibe messages.jsonl has no session directory",
            })?;
        return match fs::canonicalize(parent) {
            Ok(parent) => Ok(parent),
            Err(error) if root_missing && error.kind() == std::io::ErrorKind::NotFound => {
                Ok(parent.to_path_buf())
            }
            Err(error) => Err(error.into()),
        };
    }
    Ok(context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf()))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn open_source(
    source: MistralVibeSessionSource,
    observation: SourceObservation,
    machine_id: &str,
    source_revision: String,
    canonical_source_identity: String,
    mut session: SessionFact,
    metadata_failure: Option<String>,
    stored: Option<&SyncCursor>,
) -> Result<OpenedSource> {
    let mut generation = 0_u64;
    let mut prior = None;
    let mut locator_policy_upgrade = false;
    let mut rejection_detail_rebuild = false;
    let mut lifecycle = SourceLifecycle::Fresh;
    let mut force_publication = stored.is_none();
    if let Some(stored) = stored {
        match decode_native_checkpoint(&stored.cursor)? {
            Some(checkpoint) => {
                generation = checkpoint.generation;
                rejection_detail_rebuild =
                    checkpoint.rejected_records != 0 && checkpoint.rejection_details.is_empty();
                if rejection_detail_rebuild {
                    force_publication = true;
                    lifecycle = SourceLifecycle::Migrated;
                }
                prior = Some(checkpoint);
            }
            None => {
                let previous_policy = decode_native_checkpoint_at_policy(
                    &stored.cursor,
                    LOCATOR_REPAIR_PREVIOUS_POLICY_REVISION,
                )?;
                if let Some(previous_policy) = previous_policy {
                    force_publication = true;
                    if previous_policy.machine_id == machine_id
                        && previous_policy.canonical_source_identity == canonical_source_identity
                        && previous_policy.session.provider_session_id
                            == session.provider_session_id
                    {
                        generation = previous_policy.generation;
                        prior = Some(previous_policy);
                        locator_policy_upgrade = true;
                        lifecycle = SourceLifecycle::Migrated;
                    } else {
                        generation = previous_policy.generation.checked_add(1).ok_or(
                            CaptureError::SystemInvariant(
                                "Mistral Vibe source generation overflowed",
                            ),
                        )?;
                        lifecycle = SourceLifecycle::Replace;
                    }
                } else if let Some(migrated) = migrate_released_cursor(
                    &stored.cursor,
                    &source,
                    &observation,
                    &session,
                    machine_id,
                    &canonical_source_identity,
                    &source_revision,
                )? {
                    force_publication = true;
                    generation = migrated.generation;
                    prior = Some(migrated);
                    lifecycle = SourceLifecycle::Migrated;
                } else {
                    force_publication = true;
                    generation = generation.saturating_add(1);
                    lifecycle = SourceLifecycle::Replace;
                }
            }
        }
    }

    let mut checkpoint = Checkpoint::fresh(
        &observation,
        machine_id,
        source_revision.clone(),
        canonical_source_identity.clone(),
        session.clone(),
        generation,
    );
    let mut hasher = initial_prefix_hasher();
    if let Some(previous) = prior {
        session.started_at = session.started_at.min(previous.session.started_at);
        let migration_required = lifecycle == SourceLifecycle::Migrated;
        let same_paths = previous.canonical_metadata_path == observation.canonical_metadata_path
            && previous.canonical_messages_path == observation.canonical_messages_path;
        let same_metadata = previous.metadata_sha256 == observation.metadata_sha256;
        let same_physical = previous
            .messages_stamp
            .same_physical_file(&observation.messages);
        let enough_bytes = observation.messages.length >= previous.complete_prefix_end;
        let prefix_valid = same_paths
            && same_metadata
            && same_physical
            && enough_bytes
            && hash_file_prefix(
                &observation.canonical_messages_path,
                previous.complete_prefix_end,
            )? == previous.complete_prefix_sha256;
        if prefix_valid {
            hasher = hash_prefix(
                &observation.canonical_messages_path,
                previous.complete_prefix_end,
                initial_prefix_hasher(),
            )?;
            if locator_policy_upgrade || rejection_detail_rebuild {
                // Locator policy upgrades and old detail-free rejection checkpoints
                // replay a validated prefix under the same logical source generation.
                hasher = initial_prefix_hasher();
                checkpoint = Checkpoint::fresh(
                    &observation,
                    machine_id,
                    source_revision.clone(),
                    canonical_source_identity.clone(),
                    session.clone(),
                    previous.generation,
                );
                lifecycle = SourceLifecycle::Migrated;
            } else {
                checkpoint = previous;
                let fully_consumed = checkpoint.complete_prefix_end == observation.messages.length;
                let unchanged = fully_consumed
                    && checkpoint.terminal
                    && checkpoint.metadata_stamp == observation.metadata
                    && checkpoint.messages_stamp == observation.messages
                    && checkpoint.metadata_sha256 == observation.metadata_sha256
                    && checkpoint.source_revision == source_revision
                    && checkpoint.generation_identity == observation.generation_identity()
                    && checkpoint.canonical_source_identity == canonical_source_identity
                    && checkpoint.session == session;
                lifecycle = if unchanged && !force_publication && !migration_required {
                    SourceLifecycle::NoOp
                } else if migration_required {
                    SourceLifecycle::Migrated
                } else {
                    SourceLifecycle::Append
                };
            }
        } else {
            force_publication = true;
            lifecycle = if observation.messages.length < previous.complete_prefix_end {
                SourceLifecycle::Truncate
            } else if same_physical {
                SourceLifecycle::Rewrite
            } else {
                SourceLifecycle::Replace
            };
            checkpoint.generation =
                previous
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Mistral Vibe source generation overflowed",
                    ))?;
        }
    }

    let mut file = File::open(&observation.canonical_messages_path)?;
    if FileStamp::from_metadata(&file.metadata()?)? != observation.messages {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(checkpoint.complete_prefix_end))?;
    Ok(OpenedSource {
        source,
        observation,
        lifecycle,
        checkpoint,
        target_source_revision: source_revision,
        target_source_identity: canonical_source_identity,
        target_session: session,
        force_publication,
        metadata_failure,
        reader: BufReader::new(file),
        hasher,
    })
}

pub(super) fn retire_missing_source(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    entry: &KnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &entry.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "Mistral Vibe retirement lost its source cursor",
        ))?;
    let committed = decode_native_path_committed_cursor(&stored.cursor).ok();
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::MistralVibe,
        source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: entry.locator_identity.clone(),
        cursor_stream: entry.cursor_stream.clone(),
        expected_canonical_source_identity: entry.canonical_source_identity.clone(),
        expected_source_revision: entry.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if committed
        .as_ref()
        .is_some_and(|committed| committed.publication_id() == publication_id)
    {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let provider_cursor = committed
        .as_ref()
        .map(|committed| committed.provider_cursor().to_owned())
        .unwrap_or_else(|| stored.cursor.clone());
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor),
        provider_sync_cursor(
            &context.machine_id,
            entry.cursor_stream.clone(),
            provider_cursor,
            context.imported_at,
        ),
    );
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let disposition = group.retire_provider_source_route(&retirement)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
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

pub(super) fn load_known_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::MistralVibe
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(MISTRAL_VIBE_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
        ) else {
            continue;
        };
        let canonical_messages_path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&canonical_messages_path)?;
        let cursor_stream = provider_source_cursor_stream_for_path(
            CaptureProvider::MistralVibe,
            MISTRAL_VIBE_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(cursor) = store.get_sync_cursor(None, machine_id, &cursor_stream)? else {
            continue;
        };
        let checkpoint = decode_native_checkpoint(&cursor.cursor)?;
        if checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.canonical_messages_path != canonical_messages_path
                || source.descriptor.external_session_id.as_deref()
                    != Some(checkpoint.session.provider_session_id.as_str())
        }) {
            continue;
        }
        let source_revision = checkpoint
            .map(|checkpoint| checkpoint.source_revision)
            .or_else(|| {
                source
                    .sync
                    .metadata
                    .get("source_revision")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        let Some(source_revision) = source_revision else {
            continue;
        };
        let route = KnownRoute {
            locator_identity,
            cursor_stream: cursor_stream.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision,
        };
        if let Some(previous) = routes.insert(cursor_stream, route.clone()) {
            if previous != route {
                return Err(CaptureError::SystemInvariant(
                    "Mistral Vibe persisted conflicting routes for one transcript",
                ));
            }
        }
    }
    Ok(routes.into_values().collect())
}

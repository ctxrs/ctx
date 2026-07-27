use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn import_source(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &RovoDevSessionSource,
    configured_source_root: &Path,
    root_stream: &str,
    manifest: &mut RovoDevRootManifest,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<PublishedSource> {
    let observation = RovoDevSessionObservation::read(source)?;
    let context_oversized = observation.context_length() > MAX_PROVIDER_JSONL_LINE_BYTES as u64;
    let context_bytes = if context_oversized {
        None
    } else {
        Some(fs::read(&source.context_path)?)
    };
    let metadata_oversized = observation
        .metadata_length()
        .is_some_and(|length| length > MAX_PROVIDER_JSONL_LINE_BYTES as u64);
    let metadata_bytes = match source.metadata_path.as_deref() {
        Some(path) if !metadata_oversized => Some(fs::read(path)?),
        Some(_) | None => None,
    };
    let metadata_source = source.metadata_path.as_deref().map(|_| {
        (
            metadata_bytes.as_deref(),
            observation.metadata_length().unwrap_or(0),
        )
    });
    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    let path_identity = provider_path_identity(observation.canonical_path())?;
    let source_identity = format!("rovodev-session:{path_identity}");
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        &path_identity,
    );
    let source_revision = source_revision(
        context_bytes.as_deref(),
        observation.context_length(),
        metadata_source,
        observation.revision_authority(),
        options.inventory_observation_token.as_deref(),
    );
    let physical_identity = observation.physical_identity();
    let document = if context_oversized {
        Err(failure(
            1,
            format!(
                "Rovo Dev session_context.json exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
            ),
        ))
    } else {
        prepare_document(
            source,
            context,
            context_bytes.as_deref().unwrap_or_default(),
            metadata_bytes.as_deref(),
            metadata_oversized.then(|| {
                failure(
                    1,
                    format!(
                        "Rovo Dev metadata.json exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
                    ),
                )
            }),
        )
    };
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let plan = classify_cursor(
        stored.as_ref(),
        &source_identity,
        &source_revision,
        &physical_identity,
        document.as_ref().ok(),
    )?;

    let replay_only = options.import_profile.is_replay_only();
    let mut summary = ProviderImportSummary::default();
    let mut groups_changed = 0_usize;
    let final_cursor = match plan {
        CursorPlan::AlreadyCommitted(cursor) => {
            replay_cursor_summary(&cursor, &mut summary);
            cursor
        }
        CursorPlan::Publish {
            mut expected,
            prior,
            generation,
            start,
            replacement,
        } => {
            if replay_only {
                return Err(CaptureError::InvalidPayload(
                    "RovoDev output replay requires matching committed NativePath Core".to_owned(),
                ));
            }
            match document.as_ref() {
                Ok(document) => {
                    let mut next = start;
                    let mut prior_cursor = prior;
                    loop {
                        let page = prepare_page(source, context, document, next)?;
                        let cursor = publish_core_page(
                            store,
                            committed_store,
                            bulk_guard,
                            source,
                            configured_source_root,
                            root_stream,
                            manifest,
                            context,
                            options,
                            &observation,
                            &source_identity,
                            &source_revision,
                            &physical_identity,
                            &stream,
                            expected,
                            prior_cursor.as_ref(),
                            generation,
                            replacement && next == start,
                            document,
                            page,
                            &mut summary,
                        )?;
                        groups_changed = groups_changed.saturating_add(1);
                        expected = store
                            .get_sync_cursor(None, &context.machine_id, &stream)?
                            .map(|cursor| cursor.cursor);
                        next =
                            usize::try_from(cursor.frontier.next_message_index).map_err(|_| {
                                CaptureError::InvalidPayload(
                                    "RovoDev NativePath frontier exceeds usize".to_owned(),
                                )
                            })?;
                        let terminal = cursor.terminal;
                        prior_cursor = Some(cursor);
                        if terminal || options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                        {
                            break prior_cursor.ok_or(CaptureError::SystemInvariant(
                                "RovoDev NativePath lost its committed cursor",
                            ))?;
                        }
                    }
                }
                Err(failure) => {
                    let cursor = publish_rejection_cursor(
                        store,
                        committed_store,
                        bulk_guard,
                        source,
                        root_stream,
                        manifest,
                        context,
                        &observation,
                        &source_identity,
                        &source_revision,
                        &physical_identity,
                        &stream,
                        expected,
                        prior.as_ref(),
                        generation,
                        replacement,
                        failure.clone(),
                    )?;
                    groups_changed = groups_changed.saturating_add(1);
                    replay_cursor_summary(&cursor, &mut summary);
                    summary.set_work_result(ProviderImportWorkResult::Changed);
                    cursor
                }
            }
        }
    };

    if let Some(sink) = options.import_profile.sink() {
        if let Ok(document) = document.as_ref() {
            if final_cursor.terminal && final_cursor.source_revision == source_revision {
                if let Err(error) = replay_outputs(
                    source,
                    document,
                    &source_identity,
                    &final_cursor,
                    sink.as_ref(),
                ) {
                    sink.mark_behind(error.clone());
                    summary.record_failure(ProviderImportFailure {
                        line: 0,
                        error: format!("RovoDev output replay is behind: {error}"),
                    });
                }
            }
        }
    }

    Ok(PublishedSource {
        cursor: final_cursor,
        summary,
        groups_changed,
    })
}

pub(super) fn prepare_document(
    source: &RovoDevSessionSource,
    context: &ProviderAdapterContext,
    context_bytes: &[u8],
    metadata_bytes: Option<&[u8]>,
    metadata_acquisition_failure: Option<RovoDevFailure>,
) -> std::result::Result<PreparedDocument, RovoDevFailure> {
    let context_json = serde_json::from_slice::<Value>(context_bytes)
        .map_err(|error| failure(1, format!("invalid Rovo Dev session_context.json: {error}")))?;
    validate_json_bounds(&context_json)
        .map_err(|error| failure(1, format!("Rovo Dev session_context.json {error}")))?;
    let messages = message_history(&context_json).cloned().ok_or_else(|| {
        failure(
            1,
            "Rovo Dev session_context.json is missing message_history array",
        )
    })?;
    let context_metadata = metadata_without_transcripts(&context_json);

    let mut initial_failures = metadata_acquisition_failure.into_iter().collect::<Vec<_>>();
    let metadata = match metadata_bytes {
        Some(bytes) => match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => match validate_json_bounds(&value) {
                Ok(()) => value,
                Err(error) => {
                    initial_failures.push(failure(1, format!("Rovo Dev metadata.json {error}")));
                    Value::Null
                }
            },
            Err(error) => {
                initial_failures.push(failure(
                    1,
                    format!("invalid Rovo Dev metadata.json: {error}"),
                ));
                Value::Null
            }
        },
        None => Value::Null,
    };
    let metadata_preview = metadata_without_transcripts(&metadata);
    let provider_session_id = provider_string_field(&metadata, &["session_id", "sessionId"])
        .or_else(|| provider_string_field(&context_json, &["session_id", "sessionId"]))
        .unwrap_or_else(|| source.provider_session_id.clone());
    let parent_provider_session_id = provider_string_field(
        &metadata,
        &[
            "parent_session_id",
            "parentSessionId",
            "forked_from_session_id",
            "forkedFromSessionId",
            "fork_parent_id",
        ],
    );
    let started_at = provider_timestamp_from_fields(
        &metadata,
        &["created_at", "createdAt", "started_at", "startedAt"],
    )
    .or_else(|| messages.iter().find_map(message_timestamp))
    .unwrap_or(context.imported_at);
    let ended_at = provider_timestamp_from_fields(
        &metadata,
        &["updated_at", "updatedAt", "last_updated", "lastUpdated"],
    )
    .or_else(|| messages.iter().rev().find_map(message_timestamp));
    let cwd = provider_string_field(
        &metadata,
        &[
            "workspace_path",
            "workspacePath",
            "working_directory",
            "workingDirectory",
            "cwd",
        ],
    );
    Ok(PreparedDocument {
        context_record: context_bytes.to_vec(),
        context_metadata,
        metadata,
        metadata_preview,
        messages,
        provider_session_id,
        parent_provider_session_id,
        started_at,
        ended_at,
        cwd,
        initial_failures,
    })
}

pub(super) fn revalidate_discovery(path: &Path, discovery: &RovoDevDiscovery) -> Result<()> {
    let current = discover_rovodev_session_sources(path)?;
    if current.root_exists() != discovery.root_exists()
        || current.canonical_context_paths()? != discovery.canonical_context_paths()?
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

pub(super) fn capture_source(
    id: Uuid,
    source: &RovoDevSessionSource,
    context: &ProviderAdapterContext,
    configured_source_root: &Path,
    source_revision: &str,
    canonical_source_identity: &str,
    document: &PreparedDocument,
) -> CaptureSource {
    let raw_source_path = source.context_path.display().to_string();
    let source_root = configured_source_root.display().to_string();
    CaptureSource {
        id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::RovoDev,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: document.cwd.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_format: Some(ROVODEV_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(document.provider_session_id.clone()),
        },
        started_at: document.started_at,
        ended_at: document.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": document.provider_session_id,
                "source_format": ROVODEV_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::RovoDev,
                    &document.provider_session_id,
                    ROVODEV_SOURCE_FORMAT,
                    Some(&raw_source_path),
                ),
                "nativepath_parser": ROVODEV_NATIVE_PARSER_REVISION,
                "nativepath_policy_revision": ROVODEV_NATIVE_POLICY_REVISION,
            }),
        ),
    }
}

pub(super) fn source_revision(
    context_bytes: Option<&[u8]>,
    context_length: u64,
    metadata_source: Option<(Option<&[u8]>, u64)>,
    frozen_revision_authority: [u8; 32],
    inventory_token: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_SOURCE_REVISION_DOMAIN);
    digest.update(ROVODEV_NATIVE_PARSER_REVISION.as_bytes());
    digest.update(ROVODEV_NATIVE_POLICY_REVISION.to_be_bytes());
    digest.update(frozen_revision_authority);
    digest.update(context_length.to_be_bytes());
    match context_bytes {
        Some(context) => {
            digest.update([1]);
            digest.update(context);
        }
        None => digest.update([0]),
    }
    if let Some((metadata, metadata_length)) = metadata_source {
        digest.update([1]);
        digest.update(metadata_length.to_be_bytes());
        match metadata {
            Some(metadata) => {
                digest.update([1]);
                digest.update(metadata);
            }
            None => digest.update([0]),
        }
    } else {
        digest.update([0]);
    }
    if let Some(token) = inventory_token {
        digest.update([1]);
        digest.update((token.len() as u64).to_be_bytes());
        digest.update(token.as_bytes());
    } else {
        digest.update([0]);
    }
    format!("rovodev-nativepath-sha256:{:x}", digest.finalize())
}

pub(super) fn root_identity(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    let mut digest = Sha256::new();
    digest.update(b"ctx-rovodev-root-path-v1\0");
    digest.update(format!("{:?}", normalized.as_os_str()).as_bytes());
    Ok(format!("sha256:{:x}", digest.finalize()))
}

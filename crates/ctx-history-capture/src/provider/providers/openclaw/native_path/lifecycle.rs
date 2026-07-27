use super::*;

pub(crate) fn import_openclaw_nativepath_tree(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let source_root = context
        .source_root
        .clone()
        .or(context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let inventory = discover_inventory(path)?;
    let known_routes = known_routes(store, &context.machine_id, &source_root)?;
    let sink = options.import_profile.sink().cloned();

    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            &context.machine_id,
            &inventory.paths,
            &source_root,
            context.imported_at,
            sink.as_deref(),
        );
        return Ok(ProviderImportSummary::default());
    }

    if inventory.paths.is_empty() {
        if known_routes.is_empty() {
            if has_source_history(store, &context.machine_id, &source_root)? {
                return Ok(ProviderImportSummary::default());
            }
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "no OpenClaw session JSONL files found",
            });
        }
        return retire_missing_routes(
            store,
            &context.machine_id,
            context.imported_at,
            &known_routes,
            &inventory.paths,
            if inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        );
    }

    let mut summary = import_core(store, &inventory.paths, &source_root, &context, &options)?;
    if summary.work_remaining {
        return Ok(summary);
    }
    summary.merge_from(retire_missing_routes(
        store,
        &context.machine_id,
        context.imported_at,
        &known_routes,
        &inventory.paths,
        ProviderSourceRouteRetirementReason::SourceMissing,
    )?);
    replay_outputs_or_mark_behind(
        store,
        &context.machine_id,
        &inventory.paths,
        &source_root,
        context.imported_at,
        sink.as_deref(),
    );
    Ok(summary)
}

pub(super) fn has_source_history(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<bool> {
    let source_root = source_root.display().to_string();
    Ok(store.list_capture_sources()?.into_iter().any(|source| {
        source.descriptor.provider == CaptureProvider::OpenClaw
            && source.descriptor.machine_id == machine_id
            && source.descriptor.source_format.as_deref() == Some(OPENCLAW_SOURCE_FORMAT)
            && source.descriptor.source_root.as_deref() == Some(source_root.as_str())
    }))
}

pub(super) struct Inventory {
    pub(super) paths: BTreeSet<PathBuf>,
    pub(super) root_missing: bool,
}

pub(super) fn discover_inventory(root: &Path) -> Result<Inventory> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Inventory {
                paths: BTreeSet::new(),
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    }
    let restrict_to_sessions = root.is_dir();
    let mut paths = BTreeSet::new();
    crate::provider::providers::native_jsonl::visit_native_jsonl_files(
        root,
        CaptureProvider::OpenClaw,
        &mut |candidate| {
            if restrict_to_sessions && !path_has_component(candidate, "sessions") {
                return Ok(());
            }
            paths.insert(fs::canonicalize(candidate)?);
            Ok(())
        },
    )?;
    Ok(Inventory {
        paths,
        root_missing: false,
    })
}

pub(super) fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == expected)
}

pub(super) fn import_core(
    store: &mut Store,
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let publication_context = PublicationContext {
        machine_id: &context.machine_id,
        source_root,
        imported_at: context.imported_at,
        history_record_id: options.history_record_id,
        inventory_observation_token: options.inventory_observation_token.as_deref(),
    };
    let mut accumulator = GroupAccumulator::new(
        store,
        &committed_store,
        &bulk_guard,
        publication_context,
        options.capture_work_limit,
    );
    let operation = (|| {
        for path in paths {
            if accumulator.stopped() {
                break;
            }
            if let Some(token) = options.inventory_observation_token.as_deref() {
                if crate::observe_ordinary_file(path)?.token_hex() != token {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
            }
            let observation = OpenClawSessionObservation::read(path)?;
            let locator = provider_path_identity(&observation.canonical_path)?;
            let stream = provider_source_cursor_stream_for_path(
                CaptureProvider::OpenClaw,
                OPENCLAW_SOURCE_FORMAT,
                &locator,
            );
            let stored = accumulator
                .store()
                .get_sync_cursor(None, &context.machine_id, &stream)?;
            let reactivate_retired_route = stored
                .as_ref()
                .is_some_and(|cursor| cursor_was_retired(&cursor.cursor));
            let decoded = stored
                .as_ref()
                .map(|cursor| decode_cursor(&cursor.cursor, path, &observation))
                .transpose()?;
            let migrated = matches!(decoded, Some(CursorDecode::Migrated(_)));
            let previous = decoded.and_then(|decoded| match decoded {
                CursorDecode::Native(checkpoint) | CursorDecode::Migrated(checkpoint) => {
                    Some(checkpoint)
                }
                CursorDecode::Reset => None,
            });
            let had_previous = previous.is_some();
            let mut reader = open_pages(
                path,
                context.imported_at,
                false,
                options.inventory_observation_token.as_deref(),
                reactivate_retired_route,
                previous.as_ref(),
            )?;
            let mut emitted = false;
            while let Some(page) = reader.next_page()? {
                emitted = true;
                accumulator.push(PendingPage {
                    path: path.clone(),
                    page,
                })?;
                if accumulator.stopped() {
                    break;
                }
            }
            if !accumulator.stopped() && !emitted {
                if let Some(outcome) = reader.outcome.as_ref() {
                    if !had_previous
                        && outcome.source_change == SourceChange::Fresh
                        && observation.transcript.length == 0
                    {
                        continue;
                    } else if outcome.source_change == SourceChange::Unchanged && !migrated {
                        accumulator.record_unchanged(outcome);
                    } else {
                        accumulator.push(PendingPage {
                            path: path.clone(),
                            page: observation_page(
                                outcome.checkpoint.clone(),
                                reader.session.clone(),
                                outcome.source_change,
                            ),
                        })?;
                    }
                }
            }
        }
        accumulator.finish()
    })();
    let stopped = accumulator.stopped();
    drop(accumulator);
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(mut summary), Ok(())) => {
            if stopped {
                summary.work_remaining = true;
            }
            Ok(summary)
        }
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

pub(super) fn observation_page(
    checkpoint: Checkpoint,
    session: SessionFact,
    source_change: SourceChange,
) -> Page {
    Page {
        expected_checkpoint: checkpoint.clone(),
        next_checkpoint: checkpoint,
        source_change,
        session,
        events: Vec::new(),
        touches: Vec::new(),
        outputs: Vec::new(),
        rejections: Vec::new(),
        logical_units: 1,
        conservative_serialized_bytes: PAGE_ENVELOPE_BYTES,
        terminal: true,
    }
}

pub(super) struct PublicationContext<'a> {
    pub(super) machine_id: &'a str,
    pub(super) source_root: &'a Path,
    pub(super) imported_at: DateTime<Utc>,
    pub(super) history_record_id: Option<Uuid>,
    pub(super) inventory_observation_token: Option<&'a str>,
}

use super::core::provider_sync_cursor;
use super::*;

pub(super) fn encode_warp_cursor(
    state: &WarpNativePersistedState,
    replacement_prior_source_identity: Option<&str>,
    released_migration: Option<&ReleasedWarpMigrationState>,
) -> Result<String> {
    Ok(serde_json::to_string(&WarpCursorWire {
        version: WARP_CURSOR_VERSION,
        kind: WARP_CURSOR_KIND.to_owned(),
        state: Some(state.clone()),
        replacement_prior_source_identity: replacement_prior_source_identity.map(str::to_owned),
        released_migration: released_migration.cloned(),
    })?)
}

fn encode_released_warp_migration_cursor(migration: &ReleasedWarpMigrationState) -> Result<String> {
    Ok(serde_json::to_string(&WarpCursorWire {
        version: WARP_CURSOR_VERSION,
        kind: WARP_CURSOR_KIND.to_owned(),
        state: None,
        replacement_prior_source_identity: None,
        released_migration: Some(migration.clone()),
    })?)
}

pub(super) fn provider_cursor(encoded: &str) -> Option<String> {
    ctx_history_store::decode_native_path_committed_cursor(encoded)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .ok()
        .or_else(|| Some(encoded.to_owned()))
}

pub(super) fn decode_warp_cursor(encoded: &str) -> Result<Option<DecodedWarpCursor>> {
    let committed = ctx_history_store::decode_native_path_committed_cursor(encoded);
    let provider = match &committed {
        Ok(committed) => committed.provider_cursor(),
        Err(_) => {
            // Released certified pre-NativePath cursors are migration input
            // only. They grant no NativePath resume authority and are replaced
            // after one complete NativePath scan.
            return match CertifiedProviderCursor::decode_if_certified(encoded)? {
                Some(certified)
                    if certified.native_position().kind()
                        == "warp-conversation-task-keyset-v4"
                        && certified.parser_revision() == 5
                        && certified.policy_revision() == 7
                        && certified.source_revision().starts_with(
                            "warp-sqlite-snapshot-v1:capture=5;policy=7;schema=",
                        ) =>
                {
                    Ok(Some(DecodedWarpCursor {
                        state: None,
                        replacement_prior_source_identity: None,
                        released_migration: None,
                        released_source_revision: Some(
                            certified.source_revision().to_owned(),
                        ),
                    }))
                }
                Some(_) => Err(CaptureError::InvalidPayload(
                    "Warp migration cursor does not match the released Warp cursor authority"
                        .to_owned(),
                )),
                None => Err(CaptureError::InvalidPayload(
                    "Warp cursor is neither a committed NativePath cursor nor a released certified migration cursor"
                        .to_owned(),
                )),
            };
        }
    };
    let wire: WarpCursorWire = serde_json::from_str(provider).map_err(|_| {
        CaptureError::InvalidPayload("Warp committed NativePath cursor is malformed".to_owned())
    })?;
    if wire.version != WARP_CURSOR_VERSION
        || wire.kind != WARP_CURSOR_KIND
        || wire.state.as_ref().is_some_and(|state| {
            state.parser_revision != WARP_NATIVE_PARSER_REVISION
                || state.policy_revision != WARP_NATIVE_POLICY_REVISION
        })
        || (wire.state.is_none() && wire.released_migration.is_none())
    {
        return Err(CaptureError::InvalidPayload(
            "Warp committed NativePath cursor has unsupported authority revisions".to_owned(),
        ));
    }
    Ok(Some(DecodedWarpCursor {
        state: wire.state,
        replacement_prior_source_identity: wire.replacement_prior_source_identity,
        released_migration: wire.released_migration,
        released_source_revision: None,
    }))
}

pub(super) fn known_warp_route(
    store: &Store,
    query: KnownWarpRouteQuery<'_>,
) -> Result<Option<KnownWarpRoute>> {
    let KnownWarpRouteQuery {
        machine_id,
        path,
        configured_root,
        path_identity,
        cursor_stream,
        cursor,
        decoded,
    } = query;
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let Some(decoded) = decoded else {
        return Ok(None);
    };
    let expected_path = match path.canonicalize() {
        Ok(path) => path,
        Err(_) if path.is_absolute() => path.to_path_buf(),
        Err(_) => std::env::current_dir()?.join(path),
    }
    .display()
    .to_string();
    let sources = store.list_capture_sources()?;

    if decoded.released_source_revision.is_some() || decoded.released_migration.is_some() {
        let persisted = decoded.released_migration.as_ref();
        let mut canonical_source_identities = BTreeSet::new();
        let mut released_source_ids = BTreeMap::new();
        for source in &sources {
            if source.descriptor.provider != CaptureProvider::Warp
                || source.descriptor.machine_id != machine_id
                || source.descriptor.source_format.as_deref() != Some(WARP_SQLITE_SOURCE_FORMAT)
                || source.descriptor.raw_source_path.as_deref() != Some(expected_path.as_str())
            {
                continue;
            }
            let Some(external_session_id) = source.descriptor.external_session_id.as_deref() else {
                continue;
            };
            let Some(canonical_source_identity) = source.descriptor.source_identity.as_deref()
            else {
                continue;
            };
            if persisted.is_some_and(|migration| {
                migration.canonical_source_identity != canonical_source_identity
            }) {
                continue;
            }
            canonical_source_identities.insert(canonical_source_identity.to_owned());
            if released_source_ids
                .insert(external_session_id.to_owned(), source.id)
                .is_some()
            {
                return Err(CaptureError::SystemInvariant(
                    "Warp released migration found duplicate session sources for one SQLite path",
                ));
            }
        }
        let canonical_source_identity = if let Some(migration) = persisted {
            migration.canonical_source_identity.clone()
        } else {
            match canonical_source_identities.len() {
                0 => {
                    let configured_root = configured_root.display().to_string();
                    provider_source_identity(
                        CaptureProvider::Warp,
                        WARP_SQLITE_SOURCE_FORMAT,
                        Some(&configured_root),
                        Some(&expected_path),
                        None,
                        &Value::Null,
                    )
                    .ok_or(CaptureError::SystemInvariant(
                        "Warp released cursor could not reconstruct its canonical source identity",
                    ))?
                }
                1 => canonical_source_identities.into_iter().next().ok_or(
                    CaptureError::SystemInvariant(
                        "Warp released source identity disappeared during resolution",
                    ),
                )?,
                _ => {
                    return Err(CaptureError::SystemInvariant(
                        "Warp released cursor matched multiple canonical source identities",
                    ));
                }
            }
        };
        let locator_identity = persisted
            .map(|migration| migration.locator_identity.clone())
            .unwrap_or_else(|| format!("warp-sqlite:{path_identity}"));
        let source_revision = persisted
            .map(|migration| migration.source_revision.clone())
            .or_else(|| decoded.released_source_revision.clone())
            .ok_or(CaptureError::SystemInvariant(
                "Warp released migration has no source revision",
            ))?;
        let released_migration = ReleasedWarpMigrationState {
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.clone(),
            source_revision: source_revision.clone(),
        };
        return Ok(Some(KnownWarpRoute {
            locator_identity,
            canonical_source_identity,
            source_revision,
            cursor: cursor.clone(),
            released_migration: Some(released_migration),
            released_source_ids,
        }));
    }

    let expected_locator = decoded
        .state
        .as_ref()
        .map(|state| {
            warp_canonical_source_identity(machine_id, &state.source_identity)
                .map(|identity| format!("{cursor_stream}#{identity}"))
        })
        .transpose()?;
    let mut found = None;
    for source in sources {
        if source.descriptor.provider != CaptureProvider::Warp
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(WARP_SQLITE_SOURCE_FORMAT)
            || source.descriptor.raw_source_path.as_deref() != Some(expected_path.as_str())
        {
            continue;
        }
        let Some(locator_identity) = source
            .sync
            .metadata
            .get("warp_native_locator_identity")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(stored_stream) = source
            .sync
            .metadata
            .get("warp_native_cursor_stream")
            .and_then(Value::as_str)
        else {
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
        let Some(canonical_source_identity) = source.descriptor.source_identity.as_deref() else {
            continue;
        };
        if stored_stream != cursor_stream {
            continue;
        }
        if expected_locator
            .as_deref()
            .is_some_and(|expected| expected != locator_identity)
        {
            continue;
        }
        let route = KnownWarpRoute {
            locator_identity: locator_identity.to_owned(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            cursor: cursor.clone(),
            released_migration: None,
            released_source_ids: BTreeMap::new(),
        };
        if found.replace(route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Warp persisted duplicate current routes for one SQLite source",
            ));
        }
    }
    Ok(found)
}

pub(super) fn retire_known_route(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    route: &KnownWarpRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let provider_cursor = if let Some(migration) = route.released_migration.as_ref() {
        encode_released_warp_migration_cursor(migration)?
    } else {
        provider_cursor(&route.cursor.cursor).ok_or(CaptureError::SystemInvariant(
            "Warp cursor could not be decoded",
        ))?
    };
    let context = WarpPublicationContext {
        machine_id: machine_id.to_owned(),
        raw_source_path: String::new(),
        source_root: String::new(),
        imported_at: retired_at,
        history_record_id: None,
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.cursor.stream.clone(),
        proposed_source_identity: route.canonical_source_identity.clone(),
        source_revision: route.source_revision.clone(),
        replacement_prior_source_id: None,
        replacement_prior_source_identity: None,
        released_migration: route.released_migration.clone(),
        released_source_ids: route.released_source_ids.clone(),
    };
    let transition = NativePathCursorTransition::new(
        Some(route.cursor.cursor.clone()),
        provider_sync_cursor(&context, provider_cursor),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Warp,
        source_format: WARP_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: machine_id.to_owned(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason,
    };
    let mut digest = Sha256::new();
    digest.update(WARP_RETIREMENT_DOMAIN);
    digest.update(route.locator_identity.as_bytes());
    digest.update(route.source_revision.as_bytes());
    digest.update([reason as u8]);
    let publication_id = format!("warp-nativepath-retirement-v1:{:x}", digest.finalize());
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

pub(super) fn replay_outputs_or_mark_behind(
    path: &Path,
    adapter: &ProviderAdapterContext,
    sink: Option<&dyn ProOutputSink>,
    expected_core: Option<&WarpNativePersistedState>,
) {
    let Some(sink) = sink else {
        return;
    };
    let Some(expected_core) = expected_core else {
        sink.mark_behind(ProOutputSinkError::new(
            "warp_nativepath_core_unavailable",
            "Warp output replay requires a committed NativePath Core generation",
        ));
        return;
    };
    if let Err(error) = replay_outputs(path, adapter, sink, expected_core) {
        sink.mark_behind(ProOutputSinkError::new(
            "warp_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    path: &Path,
    adapter: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
    expected_core: &WarpNativePersistedState,
) -> Result<()> {
    let mut prepared = match prepare_warp_nativepath_lifecycle(path, &[]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        WarpNativePreparationOutcome::ExactNoOp { .. } => {
            return Err(CaptureError::SystemInvariant(
                "Warp output replay unexpectedly trusted a persisted terminal checkpoint",
            ));
        }
        WarpNativePreparationOutcome::Incomplete(failure)
        | WarpNativePreparationOutcome::Failed(failure) => {
            return Err(preparation_error(failure));
        }
    };
    if prepared.inputs.source_identity != expected_core.source_identity
        || prepared.inputs.snapshot_revision != expected_core.snapshot_revision
        || prepared.inputs.capability_digest != expected_core.capability_digest
        || prepared.inputs.parser_revision != expected_core.parser_revision
        || prepared.inputs.policy_revision != expected_core.policy_revision
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Warp.as_str().to_owned(),
        namespace_id: adapter.machine_id.clone(),
        source_id: warp_canonical_source_identity(
            &adapter.machine_id,
            &prepared.inputs.source_identity,
        )?,
    };
    let observed_revision = format!(
        "warp-nativepath-output-v1:parser={};policy={};capability={};snapshot={}",
        prepared.inputs.parser_revision,
        prepared.inputs.policy_revision,
        prepared.inputs.capability_digest,
        prepared.inputs.snapshot_revision,
    );
    let progress = sink.observe_source(&source).map_err(|error| {
        CaptureError::InvalidPayload(format!("Warp output progress failed: {error}"))
    })?;
    let resume_frontier = progress
        .as_ref()
        .filter(|progress| {
            progress.observed_revision == observed_revision
                && progress.parser_revision == WARP_OUTPUT_PARSER_REVISION
                && progress.materializer_revision == sink.materializer_revision()
        })
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == WARP_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<WarpNativeFrontier>(&cursor.payload).ok())
        .filter(WarpNativeFrontier::is_persistable);
    if progress.as_ref().is_some_and(|progress| {
        progress.terminal
            && progress.observed_revision == observed_revision
            && progress.parser_revision == WARP_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && resume_frontier.is_some()
    }) {
        return Ok(());
    }
    if let Some(frontier) = resume_frontier {
        prepared.inputs.resume_frontier = Some(frontier);
        prepared.inputs.action =
            super::super::nativepath::WarpNativePreparationAction::ResumeExactSnapshot;
    }
    let mut output = WarpOutputStoreSink::new(
        sink,
        source,
        observed_revision,
        progress,
        prepared.inputs.resume_frontier.clone().unwrap_or_default(),
    )?;
    if output.terminal_noop {
        return Ok(());
    }
    let outcome =
        scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreAndPro, &mut output)?;
    if let WarpNativeScanOutcome::Complete(authority) = outcome {
        output.finish(authority.persisted_state.checkpoint_frontier().clone());
    }
    Ok(())
}

struct WarpOutputStoreSink<'a> {
    sink: &'a dyn ProOutputSink,
    source: OutputSourceIdentity,
    observed_revision: String,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_cursor: Option<OutputNativeCursor>,
    disposition: ProOutputSourceDisposition,
    failed: bool,
    terminal_noop: bool,
}

impl<'a> WarpOutputStoreSink<'a> {
    fn new(
        sink: &'a dyn ProOutputSink,
        source: OutputSourceIdentity,
        observed_revision: String,
        progress: Option<ProOutputProgress>,
        _initial_frontier: WarpNativeFrontier,
    ) -> Result<Self> {
        let exact = progress.as_ref().is_some_and(|progress| {
            progress.observed_revision == observed_revision
                && progress.parser_revision == WARP_OUTPUT_PARSER_REVISION
                && progress.materializer_revision == sink.materializer_revision()
                && progress.cursor.as_ref().is_some_and(valid_output_cursor)
        });
        let terminal_noop = exact && progress.as_ref().is_some_and(|progress| progress.terminal);
        let (source_epoch, expected_source_epoch, expected_cursor, disposition) =
            if let Some(progress) = progress {
                if exact {
                    (
                        progress.source_epoch,
                        Some(progress.source_epoch),
                        progress.cursor,
                        ProOutputSourceDisposition::AppendOrResume,
                    )
                } else {
                    (
                        progress.source_epoch.checked_add(1).ok_or(
                            CaptureError::SystemInvariant("Warp output source epoch exhausted"),
                        )?,
                        Some(progress.source_epoch),
                        progress.cursor,
                        ProOutputSourceDisposition::Rewrite,
                    )
                }
            } else {
                (0, None, None, ProOutputSourceDisposition::NewSource)
            };
        Ok(Self {
            sink,
            source,
            observed_revision,
            source_epoch,
            expected_source_epoch,
            expected_cursor,
            disposition,
            failed: false,
            terminal_noop,
        })
    }

    fn finish(&mut self, frontier: WarpNativeFrontier) {
        if self.failed || self.terminal_noop {
            return;
        }
        let next = match output_cursor(&frontier) {
            Ok(cursor) => cursor,
            Err(error) => {
                self.mark_failed("warp_output_terminal_cursor", error.to_string());
                return;
            }
        };
        self.materialize(Vec::new(), next, true);
    }

    fn materialize(
        &mut self,
        observations: Vec<crate::ProOutputObservation>,
        next: OutputNativeCursor,
        terminal: bool,
    ) {
        if self.failed {
            return;
        }
        let page = ProOutputMaterializationPage {
            inventory_generation: self.sink.inventory_generation(),
            source: self.source.clone(),
            source_epoch: self.source_epoch,
            observed_revision: self.observed_revision.clone(),
            parser_revision: WARP_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: self.sink.materializer_revision().to_owned(),
            disposition: self.disposition,
            expected_prior_source_epoch: self.expected_source_epoch,
            expected_prior_cursor: self.expected_cursor.clone(),
            next_safe_cursor: next.clone(),
            terminal,
            observations,
        };
        match self.sink.materialize_page(page) {
            Ok(result)
                if result.source_epoch == self.source_epoch && result.committed_cursor == next =>
            {
                self.expected_source_epoch = Some(self.source_epoch);
                self.expected_cursor = Some(next);
                self.disposition = ProOutputSourceDisposition::AppendOrResume;
            }
            Ok(_) => self.mark_failed(
                "warp_output_receipt_mismatch",
                "Warp output sink acknowledged a different source frontier",
            ),
            Err(error) => {
                self.sink.mark_behind(error);
                self.failed = true;
            }
        }
    }

    fn mark_failed(&mut self, code: &'static str, message: impl Into<String>) {
        self.sink
            .mark_behind(ProOutputSinkError::new(code, message.into()));
        self.failed = true;
    }
}

impl WarpNativeSink for WarpOutputStoreSink<'_> {
    fn push_page(&mut self, _page: WarpNativePage) -> Result<()> {
        Ok(())
    }

    fn push_pro_output_page(
        &mut self,
        page: WarpNativeProOutputPage,
    ) -> WarpNativeProOutputPageReceipt {
        let receipt = page.receipt();
        if !self.failed {
            match output_cursor(&page.next_safe_frontier) {
                Ok(cursor) => self.materialize(page.outputs, cursor, false),
                Err(error) => {
                    self.mark_failed("warp_output_cursor", error.to_string());
                }
            }
        }
        receipt
    }
}

fn output_cursor(frontier: &WarpNativeFrontier) -> Result<OutputNativeCursor> {
    Ok(OutputNativeCursor {
        version: WARP_OUTPUT_FRONTIER_VERSION,
        payload: serde_json::to_vec(frontier)?,
    })
}

fn valid_output_cursor(cursor: &OutputNativeCursor) -> bool {
    cursor.version == WARP_OUTPUT_FRONTIER_VERSION
        && serde_json::from_slice::<WarpNativeFrontier>(&cursor.payload)
            .is_ok_and(|frontier| frontier.is_persistable())
}

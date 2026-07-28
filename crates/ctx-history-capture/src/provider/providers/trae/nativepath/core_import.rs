use super::*;

pub(super) fn import_source_core(
    path: &Path,
    source_root: &Path,
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<TraeCoreImport> {
    let (authority, conn) = acquire_source(path, source_root, context.imported_at)?;
    let stored = load_source_cursor(store, &context.machine_id, &authority.cursor_stream)?;
    let (start, generation, rejected_records, expected_encoded, already_terminal) =
        plan_core_scan(&stored, &authority)?;
    if already_terminal {
        let cursor = match stored {
            StoredTraeCursor::Native { cursor, .. } => cursor,
            StoredTraeCursor::None | StoredTraeCursor::Legacy { .. } => {
                return Err(CaptureError::SystemInvariant(
                    "Trae terminal plan lost its NativePath cursor",
                ));
            }
        };
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(TraeCoreImport {
            summary,
            route: cursor.route_state(),
            changed_groups: 0,
            complete: true,
        });
    }

    let mut scanner = TraeScanner::new(&conn, &authority, start);
    let mut expected = expected_encoded;
    let mut rejected_total = rejected_records;
    let mut summary = ProviderImportSummary::default();
    let mut changed_groups = 0_usize;
    let mut last_route = None;
    while let Some(page) = scanner.next_page(true, false)? {
        if !authority.snapshot.revalidate(&authority.path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        rejected_total =
            rejected_total.saturating_add(u64::try_from(page.rejections.len()).unwrap_or(u64::MAX));
        let next_cursor = TraeNativeCursor {
            version: TRAE_NATIVE_CURSOR_VERSION,
            parser_revision: TRAE_NATIVE_PARSER_REVISION.to_owned(),
            policy_revision: TRAE_NATIVE_POLICY_REVISION.to_owned(),
            locator_identity: authority.locator_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            canonical_source_identity: authority.proposed_source_identity.clone(),
            raw_source_path: authority.raw_source_path.clone(),
            source_revision: authority.source_revision.clone(),
            frontier: page.next,
            terminal: page.terminal,
            generation,
            rejected_records: rejected_total,
        };
        let next = provider_sync_cursor(
            &context.machine_id,
            authority.cursor_stream.clone(),
            next_cursor.encode()?,
            context.imported_at,
        );
        let transition = NativePathCursorTransition::new(expected.clone(), next);
        let publication_id = page_publication_id(&authority, &page, generation, &transition);
        let accounting = NativePathGroupAccounting::new(1, 1, page.estimated_bytes.max(1))?;
        let admission = store.admit_event_search_bulk_group(bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(admission, accounting)?;
        let changed =
            match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
                NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
                NativePathCursorSetClassification::AllExpected => {
                    let route = publish_core_page(
                        committed_store,
                        &mut group,
                        context,
                        options,
                        &authority,
                        &page,
                        &mut summary,
                    )?;
                    last_route = Some(route);
                    if !authority.snapshot.revalidate(&authority.path)? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    true
                }
            };
        group.commit()?;
        if changed {
            changed_groups = changed_groups.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        for rejection in page.rejections {
            summary.record_failure(rejection);
        }
        expected = store
            .get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?
            .map(|cursor| cursor.cursor);
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed {
            summary.work_remaining = !page.terminal;
            let route = last_route.unwrap_or_else(|| next_cursor.route_state());
            return Ok(TraeCoreImport {
                summary,
                route,
                changed_groups,
                complete: page.terminal,
            });
        }
    }
    let route = last_route.unwrap_or_else(|| TraeRouteState {
        path: authority.path.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        canonical_source_identity: authority.proposed_source_identity.clone(),
        source_revision: authority.source_revision.clone(),
    });
    Ok(TraeCoreImport {
        summary,
        route,
        changed_groups,
        complete: true,
    })
}

pub(super) fn acquire_source(
    path: &Path,
    source_root: &Path,
    observed_at: DateTime<Utc>,
) -> Result<(TraeSourceAuthority, ReadOnlySqliteConnection)> {
    let snapshot = ProviderSqliteSourceSnapshot::read(
        path,
        "Trae SQLite source must be a regular non-symlink file",
        "Trae SQLite sidecar must be a regular non-symlink file",
    )?;
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    validate_schema(&conn, path)?;
    let schema = sqlite_schema_fingerprint(&conn)?;
    let locator_identity = provider_path_identity(path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        &locator_identity,
    );
    let source_revision = format!(
        "trae-nativepath-sqlite-v1;parser={TRAE_NATIVE_PARSER_REVISION};policy={TRAE_NATIVE_POLICY_REVISION};schema={schema};{}",
        snapshot.revision_component()
    );
    Ok((
        TraeSourceAuthority {
            path: path.to_path_buf(),
            source_root: source_root.to_path_buf(),
            raw_source_path: path.display().to_string(),
            workspace_id: trae_workspace_id(path),
            workspace_folder: trae_workspace_folder(path),
            locator_identity: locator_identity.clone(),
            cursor_stream,
            proposed_source_identity: format!("trae-sqlite:{locator_identity}"),
            source_revision,
            observed_at,
            snapshot,
        },
        conn,
    ))
}

pub(super) fn validate_schema(conn: &Connection, path: &Path) -> Result<()> {
    if !sqlite_table_exists(conn, "ItemTable")? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Trae state.vscdb is missing ItemTable",
        });
    }
    let columns = sqlite_table_columns(conn, "ItemTable")?;
    if ["key", "value"]
        .iter()
        .all(|column| columns.contains(*column))
    {
        Ok(())
    } else {
        Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Trae ItemTable is missing required key/value columns",
        })
    }
}

pub(super) fn load_source_cursor(
    store: &Store,
    machine_id: &str,
    stream: &str,
) -> Result<StoredTraeCursor> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(StoredTraeCursor::None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        return Ok(StoredTraeCursor::Native {
            encoded: stored.cursor,
            publication_id: committed.publication_id().to_owned(),
            cursor: TraeNativeCursor::decode(committed.provider_cursor())?,
        });
    }
    match CertifiedProviderCursor::decode_if_certified(&stored.cursor)? {
        Some(_) => Ok(StoredTraeCursor::Legacy {
            encoded: stored.cursor,
        }),
        None => Err(CaptureError::InvalidPayload(
            "Trae cursor is neither a released legacy cursor nor NativePath authority".into(),
        )),
    }
}

pub(super) fn plan_core_scan(
    stored: &StoredTraeCursor,
    authority: &TraeSourceAuthority,
) -> Result<(TraeFrontier, u64, u64, Option<String>, bool)> {
    match stored {
        StoredTraeCursor::None => Ok((TraeFrontier::default(), 0, 0, None, false)),
        StoredTraeCursor::Legacy { encoded } => {
            Ok((TraeFrontier::default(), 0, 0, Some(encoded.clone()), false))
        }
        StoredTraeCursor::Native {
            encoded,
            publication_id,
            cursor,
        } => {
            if cursor.locator_identity != authority.locator_identity
                || cursor.cursor_stream != authority.cursor_stream
            {
                return Err(CaptureError::InvalidPayload(
                    "Trae NativePath cursor is bound to a different route".into(),
                ));
            }
            if cursor.source_revision == authority.source_revision
                && cursor.terminal
                && publication_id.starts_with("trae-nativepath-page-v1:")
            {
                return Ok((
                    cursor.frontier,
                    cursor.generation,
                    cursor.rejected_records,
                    Some(encoded.clone()),
                    true,
                ));
            }
            if cursor.source_revision == authority.source_revision && !cursor.terminal {
                return Ok((
                    cursor.frontier,
                    cursor.generation,
                    cursor.rejected_records,
                    Some(encoded.clone()),
                    false,
                ));
            }
            Ok((
                TraeFrontier::default(),
                cursor
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Trae NativePath generation exhausted",
                    ))?,
                0,
                Some(encoded.clone()),
                false,
            ))
        }
    }
}

impl<'a> TraeScanner<'a> {
    pub(super) fn new(
        conn: &'a Connection,
        authority: &'a TraeSourceAuthority,
        frontier: TraeFrontier,
    ) -> Self {
        Self {
            conn,
            authority,
            frontier,
            active: None,
            source_content_hasher: Sha256::new(),
            certified_source_bytes: 0,
        }
    }

    pub(super) fn next_page(
        &mut self,
        collect_core: bool,
        collect_outputs: bool,
    ) -> Result<Option<TraeScanPage>> {
        if self.frontier.is_terminal() {
            return Ok(None);
        }
        let expected = self.frontier;
        let mut page = TraeScanPage {
            expected,
            next: expected,
            terminal: false,
            logical_units: 0,
            estimated_bytes: 0,
            sessions: BTreeMap::new(),
            core: Vec::new(),
            outputs: Vec::new(),
            rejections: Vec::new(),
        };
        while page.logical_units < TRAE_PAGE_UNIT_LIMIT
            && page.estimated_bytes < TRAE_PAGE_BYTE_LIMIT
            && !self.frontier.is_terminal()
        {
            if self
                .active
                .as_ref()
                .is_none_or(|active| active.key_index != self.frontier.key_index)
            {
                self.active = None;
                match self.load_key(self.frontier.key_index)? {
                    TraeLoadedKey::Missing => {
                        self.advance_key()?;
                        continue;
                    }
                    TraeLoadedKey::Rejected(error) => {
                        page.logical_units = page.logical_units.saturating_add(1);
                        page.estimated_bytes = page
                            .estimated_bytes
                            .saturating_add(error.len())
                            .saturating_add(128);
                        page.rejections.push(ProviderImportFailure {
                            line: packed_native_index(
                                self.frontier.key_index,
                                self.frontier.session_index,
                                self.frontier.message_index,
                            )
                            .unwrap_or(u64::MAX) as usize,
                            error,
                        });
                        self.advance_key()?;
                        continue;
                    }
                    TraeLoadedKey::Active(active) => self.active = Some(active),
                }
            }
            let active = self.active.as_ref().ok_or(CaptureError::SystemInvariant(
                "Trae active ItemTable key is unavailable",
            ))?;
            let session_index = usize::try_from(self.frontier.session_index).map_err(|_| {
                CaptureError::InvalidPayload("Trae session frontier exceeds platform limits".into())
            })?;
            let Some(session_plan) = active.sessions.get(session_index) else {
                self.advance_key()?;
                continue;
            };
            let message_index = usize::try_from(self.frontier.message_index).map_err(|_| {
                CaptureError::InvalidPayload("Trae message frontier exceeds platform limits".into())
            })?;
            let Some(range) = session_plan.messages.get(message_index).cloned() else {
                self.frontier.session_index = self.frontier.session_index.checked_add(1).ok_or(
                    CaptureError::SystemInvariant("Trae session frontier exhausted"),
                )?;
                self.frontier.message_index = 0;
                continue;
            };
            let message: Value = match serde_json::from_slice(&active.bytes[range.clone()]) {
                Ok(message) => message,
                Err(error) => {
                    page.rejections.push(ProviderImportFailure {
                        line: packed_native_index(
                            self.frontier.key_index,
                            self.frontier.session_index,
                            self.frontier.message_index,
                        )
                        .unwrap_or(u64::MAX) as usize,
                        error: format!(
                            "Trae ItemTable key `{}` message is invalid JSON: {error}",
                            active.chat_key
                        ),
                    });
                    page.logical_units = page.logical_units.saturating_add(1);
                    page.estimated_bytes = page.estimated_bytes.saturating_add(256);
                    self.advance_message()?;
                    continue;
                }
            };
            // Output classification owns the raw structural record. It must run
            // before message text normalization can promote `output`, `result`,
            // or `error` fields into searchable Core content.
            let output = classify_output(&message);
            let provider_session_id = format!(
                "{}/{}",
                self.authority.workspace_id, session_plan.session.native_session_id
            );
            let Some(event) = trae_event_from_owned_message(
                &provider_session_id,
                &self.authority.workspace_id,
                active.chat_key,
                message,
                message_index,
                self.authority.observed_at,
            ) else {
                page.logical_units = page.logical_units.saturating_add(1);
                self.advance_message()?;
                continue;
            };
            let fact = session_fact(
                &provider_session_id,
                active.chat_key,
                &session_plan.session,
                &event,
                !output,
            );
            page.sessions
                .entry(provider_session_id.clone())
                .and_modify(|existing| merge_session_fact(existing, &fact))
                .or_insert(fact);
            if output {
                if collect_outputs {
                    let row = output_row(
                        &provider_session_id,
                        self.frontier,
                        session_plan.raw_session_index,
                        range,
                        &event,
                        self.authority.workspace_folder.as_deref(),
                    );
                    let row_bytes = output_row_bytes(&row);
                    if row_bytes > TRAE_PAGE_BYTE_LIMIT {
                        return Err(CaptureError::InvalidPayload(
                            "Trae output record exceeds the bounded NativePath output page".into(),
                        ));
                    }
                    if !page.outputs.is_empty()
                        && page.estimated_bytes.saturating_add(row_bytes) > TRAE_PAGE_BYTE_LIMIT
                    {
                        break;
                    }
                    page.estimated_bytes = page.estimated_bytes.saturating_add(row_bytes);
                    page.outputs.push(row);
                }
                if collect_core && is_failure_or_timeout(&event.raw_message) {
                    let core_event = sparse_failure_event(
                        &provider_session_id,
                        &self.authority.workspace_id,
                        active.chat_key,
                        &event,
                    );
                    page.estimated_bytes = page
                        .estimated_bytes
                        .saturating_add(core_event_bytes(&core_event));
                    page.core.push(TraeCoreRecord {
                        provider_session_id,
                        native_session_id: session_plan.session.native_session_id.clone(),
                        native_session_id_from_provider: session_plan
                            .session
                            .native_session_id_from_provider,
                        native_message_id: event.native_message_id.clone(),
                        native_message_id_from_provider: event.native_message_id_from_provider,
                        chat_key: active.chat_key,
                        value_digest: active.value_digest,
                        key_index: self.frontier.key_index,
                        raw_session_index: session_plan.raw_session_index,
                        legacy_session_index: self.frontier.session_index,
                        message_index: self.frontier.message_index,
                        event: core_event,
                    });
                }
            } else if collect_core {
                let mut core_event = trae_core_event(
                    &provider_session_id,
                    &self.authority.workspace_id,
                    active.chat_key,
                    &event,
                );
                attach_trae_complete_content_locator(
                    &mut core_event,
                    &trae_complete_message_locator(
                        self.frontier.key_index,
                        usize::try_from(session_plan.raw_session_index).map_err(|_| {
                            CaptureError::InvalidPayload(
                                "Trae raw session ordinal exceeds platform limits".into(),
                            )
                        })?,
                        message_index,
                    )?,
                    &active.record_digest,
                    &event.text,
                )?;
                page.estimated_bytes = page
                    .estimated_bytes
                    .saturating_add(core_event_bytes(&core_event));
                page.core.push(TraeCoreRecord {
                    provider_session_id,
                    native_session_id: session_plan.session.native_session_id.clone(),
                    native_session_id_from_provider: session_plan
                        .session
                        .native_session_id_from_provider,
                    native_message_id: event.native_message_id.clone(),
                    native_message_id_from_provider: event.native_message_id_from_provider,
                    chat_key: active.chat_key,
                    value_digest: active.value_digest,
                    key_index: self.frontier.key_index,
                    raw_session_index: session_plan.raw_session_index,
                    legacy_session_index: self.frontier.session_index,
                    message_index: self.frontier.message_index,
                    event: core_event,
                });
            }
            page.logical_units = page.logical_units.saturating_add(1);
            self.advance_message()?;
        }
        self.frontier = normalize_frontier(self.frontier, self.active.as_ref())?;
        page.next = self.frontier;
        page.terminal = page.next.is_terminal();
        page.estimated_bytes = page
            .estimated_bytes
            .saturating_add(page.sessions.len().saturating_mul(2048))
            .saturating_add(4096);
        if page.estimated_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Trae Core page exceeds NativePath retained-byte bounds".into(),
            ));
        }
        Ok(Some(page))
    }

    pub(super) fn load_key(&mut self, key_index: u16) -> Result<TraeLoadedKey> {
        let Some(chat_key) = TRAE_CHAT_KEYS.get(usize::from(key_index)).copied() else {
            return Ok(TraeLoadedKey::Missing);
        };
        if !self.authority.snapshot.revalidate(&self.authority.path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let candidate = {
            let _guard = SqliteLengthPreflightGuard::new(self.conn);
            self.conn
                .query_row(
                    "select typeof(value), coalesce(octet_length(value), 0) \
                     from ItemTable where [key] = ?1",
                    [chat_key],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?
        };
        let Some((value_type, retained_bytes)) = candidate else {
            return Ok(TraeLoadedKey::Missing);
        };
        let retained_bytes = u64::try_from(retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload("Trae ItemTable value length is negative".into())
        })?;
        let observed_bytes = retained_bytes
            .saturating_add(TRAE_SQLITE_VALUE_OVERHEAD_BYTES)
            .saturating_add(u64::try_from(chat_key.len()).unwrap_or(u64::MAX));
        if observed_bytes > u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).unwrap_or(u64::MAX) {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` exceeds the provider JSON bound"
            )));
        }
        if value_type != "text" {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` has unsupported SQLite type `{value_type}`"
            )));
        }
        let bytes = with_sqlite_read_snapshot(self.conn, || {
            self.conn
                .query_row(
                    "select cast(value as text) from ItemTable where [key] = ?1",
                    [chat_key],
                    |row| row.get::<_, String>(0),
                )
                .map(String::into_bytes)
                .map_err(CaptureError::from)
        })?;
        if bytes.len() != usize::try_from(retained_bytes).unwrap_or(usize::MAX) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let key_bytes = u64::try_from(chat_key.len())
            .map_err(|_| CaptureError::SystemInvariant("Trae chat key length overflow"))?;
        let value_bytes = u64::try_from(bytes.len())
            .map_err(|_| CaptureError::SystemInvariant("Trae value length overflow"))?;
        self.source_content_hasher.update(key_bytes.to_be_bytes());
        self.source_content_hasher.update(chat_key.as_bytes());
        self.source_content_hasher.update(value_bytes.to_be_bytes());
        self.source_content_hasher.update(&bytes);
        self.certified_source_bytes = self
            .certified_source_bytes
            .checked_add(16)
            .and_then(|total| total.checked_add(key_bytes))
            .and_then(|total| total.checked_add(value_bytes))
            .ok_or(CaptureError::SystemInvariant(
                "Trae certified source byte count overflow",
            ))?;
        if let Err(error) = serde_json::from_slice::<IgnoredAny>(&bytes) {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` contains invalid JSON: {error}"
            )));
        }
        let sessions = match trae_session_selection(&bytes, chat_key) {
            Ok(None) => Vec::new(),
            Ok(Some(TraeSessionSelection::CnMessages(messages))) => vec![session_plan(
                &bytes,
                TraeStreamSession {
                    native_session_id: "trae-cn-input-history".to_owned(),
                    native_session_id_from_provider: true,
                    metadata_preview: json!({
                        "id": "trae-cn-input-history",
                        "title": "Trae CN input history",
                    }),
                    explicit_started_at: None,
                    explicit_ended_at: None,
                    explicit_title: Some("Trae CN input history".to_owned()),
                    messages,
                },
                0,
            )?],
            Ok(Some(TraeSessionSelection::Sessions(container))) => {
                let mut values = TraeJsonContainerValues::new(&bytes, container)?;
                let mut sessions = Vec::new();
                let mut session_index = 0_usize;
                while let Some(range) = values.next_range()? {
                    if let Some(session) = trae_stream_session(&bytes, range, session_index)? {
                        let raw_session_index = u32::try_from(session_index).map_err(|_| {
                            CaptureError::InvalidPayload(
                                "Trae raw session ordinal exceeds u32".into(),
                            )
                        })?;
                        sessions.push(session_plan(&bytes, session, raw_session_index)?);
                    }
                    session_index = session_index.saturating_add(1);
                }
                sessions
            }
            Err(error) => {
                return Ok(TraeLoadedKey::Rejected(format!(
                    "Trae ItemTable key `{chat_key}` cannot be decoded: {error}"
                )));
            }
        };
        if !self.authority.snapshot.revalidate(&self.authority.path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let value_digest = Sha256::digest(&bytes);
        let mut value_digest_bytes = [0_u8; 32];
        value_digest_bytes.copy_from_slice(&value_digest);
        let record_digest = CompleteContentBodyDigest::parse(format!("{value_digest:x}")).ok_or(
            CaptureError::SystemInvariant("Trae SHA-256 digest encoding is invalid"),
        )?;
        Ok(TraeLoadedKey::Active(TraeActiveKey {
            key_index,
            chat_key,
            record_digest,
            value_digest: value_digest_bytes,
            bytes,
            sessions,
        }))
    }

    pub(super) fn advance_key(&mut self) -> Result<()> {
        self.frontier.key_index = self
            .frontier
            .key_index
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant("Trae key frontier exhausted"))?;
        self.frontier.session_index = 0;
        self.frontier.message_index = 0;
        self.active = None;
        Ok(())
    }

    pub(super) fn advance_message(&mut self) -> Result<()> {
        self.frontier.message_index =
            self.frontier
                .message_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae message frontier exhausted",
                ))?;
        Ok(())
    }

    pub(super) fn source_content_digest(&self) -> [u8; 32] {
        self.source_content_hasher.clone().finalize().into()
    }

    pub(super) fn certified_source_bytes(&self) -> u64 {
        self.certified_source_bytes
    }
}

pub(super) fn normalize_frontier(
    mut frontier: TraeFrontier,
    active: Option<&TraeActiveKey>,
) -> Result<TraeFrontier> {
    if frontier.is_terminal() {
        return Ok(TraeFrontier::terminal());
    }
    let Some(active) = active.filter(|active| active.key_index == frontier.key_index) else {
        return Ok(frontier);
    };
    loop {
        let session_index = usize::try_from(frontier.session_index).map_err(|_| {
            CaptureError::InvalidPayload("Trae session frontier exceeds platform limits".into())
        })?;
        let Some(session) = active.sessions.get(session_index) else {
            frontier.key_index = frontier
                .key_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant("Trae key frontier exhausted"))?;
            frontier.session_index = 0;
            frontier.message_index = 0;
            return Ok(if frontier.is_terminal() {
                TraeFrontier::terminal()
            } else {
                frontier
            });
        };
        if usize::try_from(frontier.message_index).unwrap_or(usize::MAX) < session.messages.len() {
            return Ok(frontier);
        }
        frontier.session_index =
            frontier
                .session_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae session frontier exhausted",
                ))?;
        frontier.message_index = 0;
    }
}

pub(super) fn session_plan(
    bytes: &[u8],
    session: TraeStreamSession,
    raw_session_index: u32,
) -> Result<TraeSessionPlan> {
    let mut values = TraeJsonArrayValues::new(bytes, session.messages.clone())?;
    let mut messages = Vec::new();
    while let Some(range) = values.next_range()? {
        messages.push(range);
    }
    Ok(TraeSessionPlan {
        session,
        raw_session_index,
        messages,
    })
}

pub(super) fn session_fact(
    provider_session_id: &str,
    chat_key: &'static str,
    session: &TraeStreamSession,
    event: &TraeEventInput,
    title_eligible: bool,
) -> TraeSessionFact {
    let generated = title_eligible
        .then(|| {
            event
                .text
                .replace('\n', " ")
                .chars()
                .take(50)
                .collect::<String>()
        })
        .filter(|title| !title.trim().is_empty());
    TraeSessionFact {
        provider_session_id: provider_session_id.to_owned(),
        native_session_id: session.native_session_id.clone(),
        chat_key,
        metadata_preview: trae_session_metadata_preview(&session.metadata_preview),
        started_at: session.explicit_started_at.unwrap_or(event.occurred_at),
        ended_at: session.explicit_ended_at.or(Some(event.occurred_at)),
        title: session.explicit_title.clone().or(generated),
    }
}

pub(super) fn merge_session_fact(current: &mut TraeSessionFact, next: &TraeSessionFact) {
    current.started_at = current.started_at.min(next.started_at);
    current.ended_at = match (current.ended_at, next.ended_at) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    };
    if current.title.is_none() {
        current.title.clone_from(&next.title);
    }
}

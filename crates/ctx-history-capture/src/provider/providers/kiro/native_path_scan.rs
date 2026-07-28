use super::*;

pub(super) struct KiroScanner<'source> {
    source: &'source KiroSource,
    frontier: KiroFrontier,
    emitted_terminal: bool,
    imported_at: DateTime<Utc>,
}

impl<'source> KiroScanner<'source> {
    pub(super) fn new(
        source: &'source KiroSource,
        frontier: KiroFrontier,
        imported_at: DateTime<Utc>,
    ) -> Result<Self> {
        if frontier.version != KIRO_NATIVE_CURSOR_VERSION {
            return Err(CaptureError::InvalidPayload(
                "Kiro scanner frontier version is unsupported".to_owned(),
            ));
        }
        Ok(Self {
            source,
            frontier,
            emitted_terminal: false,
            imported_at,
        })
    }

    pub(super) fn next_page(&mut self) -> Result<Option<KiroCorePage>> {
        if self.emitted_terminal {
            return Ok(None);
        }
        self.source.revalidate()?;
        let expected = self.frontier.clone();
        let candidate = self.next_candidate()?;
        let Some(candidate) = candidate else {
            self.emitted_terminal = true;
            let next = self.frontier.clone();
            return Ok(Some(KiroCorePage::terminal_empty(expected, next)));
        };
        if let Some(reason) = candidate.rejection_reason() {
            self.hash_rejected_candidate(&candidate, reason);
            self.complete_candidate(candidate.phase, candidate.rowid)?;
            let terminal = !self.has_more()?;
            if terminal {
                self.emitted_terminal = true;
            }
            return Ok(Some(KiroCorePage::rejected(
                expected,
                self.frontier.clone(),
                terminal,
                candidate.row_ordinal,
                reason.to_owned(),
            )));
        }
        if candidate.retained_bytes > MAX_PROVIDER_SQLITE_VALUE_BYTES as u64 {
            let reason = format!(
                "Kiro {} row {} exceeds the provider SQLite value bound",
                candidate.phase.table(),
                candidate.rowid
            );
            self.hash_rejected_candidate(&candidate, &reason);
            self.complete_candidate(candidate.phase, candidate.rowid)?;
            let terminal = !self.has_more()?;
            if terminal {
                self.emitted_terminal = true;
            }
            return Ok(Some(KiroCorePage::rejected(
                expected,
                self.frontier.clone(),
                terminal,
                candidate.row_ordinal,
                reason,
            )));
        }

        let row = hydrate_row(&self.source.connection, candidate.phase, candidate.rowid)?;
        let value: Value = match serde_json::from_str(&row.value) {
            Ok(value) => value,
            Err(error) => {
                let reason = format!(
                    "invalid JSON in Kiro {} row {} for key {}: {error}",
                    row.table, row.rowid, row.key
                );
                self.hash_raw_row(&row);
                self.complete_candidate(candidate.phase, candidate.rowid)?;
                let terminal = !self.has_more()?;
                if terminal {
                    self.emitted_terminal = true;
                }
                return Ok(Some(KiroCorePage::rejected(
                    expected,
                    self.frontier.clone(),
                    terminal,
                    candidate.row_ordinal,
                    reason,
                )));
            }
        };
        if value.get("history").is_some()
            && value.get("history").and_then(Value::as_array).is_none()
        {
            let reason = format!(
                "Kiro {} row {} history must be an array when present",
                row.table, row.rowid
            );
            self.hash_raw_row(&row);
            self.complete_candidate(candidate.phase, candidate.rowid)?;
            let terminal = !self.has_more()?;
            if terminal {
                self.emitted_terminal = true;
            }
            return Ok(Some(KiroCorePage::rejected(
                expected,
                self.frontier.clone(),
                terminal,
                candidate.row_ordinal,
                reason,
            )));
        }
        self.prepare_row_page(expected, candidate, row, value)
    }

    fn prepare_row_page(
        &mut self,
        expected: KiroFrontier,
        candidate: KiroCandidate,
        row: KiroConversationRow,
        value: Value,
    ) -> Result<Option<KiroCorePage>> {
        let provider_session_id = kiro_provider_session_id(&row, &value);
        let started_at = kiro_session_started_at(&row, &value, self.source_imported_at());
        let ended_at = kiro_session_ended_at(&row, &value, started_at);
        let history = value.get("history").and_then(Value::as_array);
        let history_len = history.map_or(0, Vec::len);
        let start = usize::try_from(self.frontier.next_history_index).map_err(|_| {
            CaptureError::InvalidPayload("Kiro history frontier exceeds usize".to_owned())
        })?;
        if start > history_len {
            return Err(CaptureError::InvalidPayload(
                "Kiro history frontier is beyond the active conversation".to_owned(),
            ));
        }
        if self.frontier.active_rowid.is_none() {
            self.frontier.active_rowid = Some(row.rowid);
            self.hash_row_header(&row, &value);
        } else if self.frontier.active_rowid != Some(row.rowid) {
            return Err(CaptureError::InvalidPayload(
                "Kiro active-row frontier does not match SQLite keyset".to_owned(),
            ));
        }

        let values = kiro_row_complete_values(&row);
        let locator = kiro_locator(candidate.phase, row.rowid)?;
        let mut events = Vec::new();
        let mut rejections = Vec::new();
        let initial_retained_bytes = KIRO_PAGE_BASE_BYTES
            .saturating_add(provider_session_id.len())
            .saturating_add(row.key.len());
        let mut retained_bytes = initial_retained_bytes;
        let mut logical_units = KIRO_PAGE_OVERHEAD_UNITS;
        let mut consumed = 0_usize;
        let mut next_event_ordinal = self.frontier.next_event_ordinal;
        for (history_index, entry) in history
            .into_iter()
            .flatten()
            .enumerate()
            .skip(start)
            .take(KIRO_PAGE_HISTORY_ITEMS)
        {
            let mut prepared_entry_events = Vec::new();
            let mut entry_rejection = None;
            let mut entry_retained_bytes = 0_usize;
            let mut entry_next_event_ordinal = next_event_ordinal;
            for mut native in kiro_history_entry_events(
                &row,
                &provider_session_id,
                history_index,
                entry,
                started_at,
            ) {
                let event = &mut native.event;
                if let Some(metadata) = event.metadata.as_object_mut() {
                    metadata.insert(
                        "source_record_ordinal".to_owned(),
                        Value::from(candidate.row_ordinal),
                    );
                    metadata.insert(
                        "source_record_subrecord_index".to_owned(),
                        Value::from(entry_next_event_ordinal),
                    );
                }
                attach_kiro_complete_content(event, &locator, &values, &native.complete_text)?;
                let mut touches = Vec::new();
                let touch_limit_exceeded = if matches!(
                    event.event_type,
                    ctx_history_core::EventType::ToolCall
                        | ctx_history_core::EventType::ToolOutput
                        | ctx_history_core::EventType::CommandOutput
                        | ctx_history_core::EventType::FileTouched
                ) {
                    visit_provider_file_touch_drafts_with_limit(
                        entry,
                        event_type_supports_structured_file_touches(event.event_type),
                        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
                        |(touch_ordinal, touch)| {
                            let provider_touch_index =
                                if event.provider_event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                                    touch_ordinal
                                } else {
                                    (event.provider_event_index << 16) | touch_ordinal
                                };
                            touches.push(KiroFileTouch {
                                provider_touch_index,
                                provider_event_index: Some(event.provider_event_index),
                                raw_source_path: Some(
                                    self.source.canonical_path.display().to_string(),
                                ),
                                source_root: Some(
                                    self.source.configured_source_root.display().to_string(),
                                ),
                                path: touch.path,
                                change_kind: touch.change_kind,
                                old_path: touch.old_path,
                                line_count_delta: None,
                                confidence: touch.confidence,
                                occurred_at: event.occurred_at,
                                metadata: touch.metadata,
                            });
                            Ok::<(), Infallible>(())
                        },
                    )
                    .unwrap_or_else(|never| match never {})
                    .limit_exceeded()
                } else {
                    false
                };
                if touch_limit_exceeded {
                    entry_rejection.get_or_insert_with(|| {
                        format!(
                            "Kiro history entry {history_index} exceeds the NativePath file-touch bound"
                        )
                    });
                }
                let event_bytes = estimated_event_bytes(event, &touches);
                entry_retained_bytes = entry_retained_bytes.saturating_add(event_bytes);
                prepared_entry_events.push(KiroPreparedEvent {
                    event: native.event,
                    touches,
                });
                entry_next_event_ordinal = entry_next_event_ordinal.checked_add(1).ok_or(
                    CaptureError::SystemInvariant("Kiro event ordinal overflowed"),
                )?;
            }

            let mut entry_units = prepared_entry_events
                .iter()
                .fold(prepared_entry_events.len(), |units, prepared| {
                    units.saturating_add(prepared.touches.len())
                });
            if entry_rejection.is_none()
                && KIRO_PAGE_OVERHEAD_UNITS.saturating_add(entry_units) > KIRO_PAGE_MAX_UNITS
            {
                entry_rejection = Some(format!(
                    "Kiro history entry {history_index} exceeds the NativePath 64-unit bound"
                ));
            }
            if entry_rejection.is_none()
                && initial_retained_bytes.saturating_add(entry_retained_bytes) > KIRO_PAGE_MAX_BYTES
            {
                entry_rejection = Some(format!(
                    "Kiro history entry {history_index} exceeds the NativePath page byte bound"
                ));
            }
            let mut entry_rejections = Vec::new();
            if let Some(reason) = entry_rejection {
                prepared_entry_events.clear();
                entry_retained_bytes = 512_usize.saturating_add(reason.len());
                entry_units = 1;
                entry_rejections.push(KiroRejection {
                    line: candidate.row_ordinal,
                    reason,
                });
            }
            if consumed != 0
                && (logical_units.saturating_add(entry_units) > KIRO_PAGE_MAX_UNITS
                    || retained_bytes.saturating_add(entry_retained_bytes) > KIRO_PAGE_MAX_BYTES)
            {
                break;
            }

            self.hash_history_entry(history_index, entry);
            logical_units = logical_units.saturating_add(entry_units);
            retained_bytes = retained_bytes.saturating_add(entry_retained_bytes);
            events.extend(prepared_entry_events);
            rejections.extend(entry_rejections);
            next_event_ordinal = entry_next_event_ordinal;
            consumed = consumed.saturating_add(1);
            if logical_units == KIRO_PAGE_MAX_UNITS {
                break;
            }
        }

        let next_history_index = start.saturating_add(consumed);
        let row_complete = next_history_index >= history_len;
        if row_complete {
            self.complete_candidate(candidate.phase, candidate.rowid)?;
        } else {
            self.frontier.next_history_index =
                u64::try_from(next_history_index).unwrap_or(u64::MAX);
            self.frontier.next_event_ordinal = next_event_ordinal;
        }
        let terminal = row_complete && !self.has_more()?;
        if terminal {
            self.emitted_terminal = true;
        }
        let fact = KiroSessionFact {
            table: row.table,
            rowid: row.rowid,
            key: row.key,
            provider_session_id,
            started_at,
            ended_at,
            history_len,
        };
        let page = KiroCorePage {
            expected_frontier: expected,
            next_frontier: self.frontier.clone(),
            terminal,
            retained_bytes,
            fact: Some(fact),
            events,
            rejections,
        };
        if page.logical_units() > KIRO_PAGE_MAX_UNITS {
            return Err(CaptureError::SystemInvariant(
                "Kiro page exceeded the NativePath logical-unit bound",
            ));
        }
        Ok(Some(page))
    }

    fn source_imported_at(&self) -> DateTime<Utc> {
        self.imported_at
    }

    fn next_candidate(&mut self) -> Result<Option<KiroCandidate>> {
        loop {
            let active = self.frontier.active_rowid;
            let after = self.frontier.after_rowid;
            if let Some(rowid) = active {
                return candidate_at(
                    &self.source.connection,
                    self.frontier.phase,
                    rowid,
                    self.frontier.next_row_ordinal,
                );
            }
            if let Some(candidate) = next_candidate(
                &self.source.connection,
                self.frontier.phase,
                after,
                self.frontier.next_row_ordinal,
            )? {
                return Ok(Some(candidate));
            }
            match self.frontier.phase {
                KiroPhase::V2 if self.source.tables.legacy => {
                    self.frontier.phase = KiroPhase::Legacy;
                    self.frontier.after_rowid = None;
                }
                _ => return Ok(None),
            }
        }
    }

    fn complete_candidate(&mut self, phase: KiroPhase, rowid: i64) -> Result<()> {
        self.frontier.phase = phase;
        self.frontier.after_rowid = Some(rowid);
        self.frontier.active_rowid = None;
        self.frontier.next_history_index = 0;
        self.frontier.next_event_ordinal = 0;
        self.frontier.next_row_ordinal = self
            .frontier
            .next_row_ordinal
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant("Kiro row ordinal overflowed"))?;
        Ok(())
    }

    fn has_more(&mut self) -> Result<bool> {
        if self.frontier.active_rowid.is_some() {
            return Ok(true);
        }
        if next_candidate(
            &self.source.connection,
            self.frontier.phase,
            self.frontier.after_rowid,
            self.frontier.next_row_ordinal,
        )?
        .is_some()
        {
            return Ok(true);
        }
        if self.frontier.phase == KiroPhase::V2 && self.source.tables.legacy {
            return Ok(next_candidate(
                &self.source.connection,
                KiroPhase::Legacy,
                None,
                self.frontier.next_row_ordinal,
            )?
            .is_some());
        }
        Ok(false)
    }

    fn hash_row_header(&mut self, row: &KiroConversationRow, value: &Value) {
        let mut header = value.as_object().cloned().unwrap_or_default();
        header.remove("history");
        let mut digest = Sha256::new();
        digest.update(KIRO_PREFIX_DOMAIN);
        digest.update(self.frontier.prefix_sha256);
        digest.update([self.frontier.phase.tag()]);
        digest.update(row.rowid.to_be_bytes());
        hash_field(&mut digest, row.key.as_bytes());
        hash_field(
            &mut digest,
            row.conversation_id
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        hash_field(
            &mut digest,
            serde_json::to_string(&header)
                .unwrap_or_default()
                .as_bytes(),
        );
        self.frontier.prefix_sha256 = digest.finalize().into();
    }

    fn hash_history_entry(&mut self, history_index: usize, entry: &Value) {
        let mut digest = Sha256::new();
        digest.update(KIRO_PREFIX_DOMAIN);
        digest.update(self.frontier.prefix_sha256);
        digest.update((history_index as u64).to_be_bytes());
        hash_field(
            &mut digest,
            serde_json::to_string(entry).unwrap_or_default().as_bytes(),
        );
        self.frontier.prefix_sha256 = digest.finalize().into();
    }

    fn hash_raw_row(&mut self, row: &KiroConversationRow) {
        let mut digest = Sha256::new();
        digest.update(KIRO_PREFIX_DOMAIN);
        digest.update(self.frontier.prefix_sha256);
        digest.update([self.frontier.phase.tag()]);
        digest.update(row.rowid.to_be_bytes());
        hash_field(&mut digest, row.key.as_bytes());
        hash_field(&mut digest, row.value.as_bytes());
        self.frontier.prefix_sha256 = digest.finalize().into();
    }

    fn hash_rejected_candidate(&mut self, candidate: &KiroCandidate, reason: &str) {
        let mut digest = Sha256::new();
        digest.update(KIRO_PREFIX_DOMAIN);
        digest.update(self.frontier.prefix_sha256);
        digest.update([candidate.phase.tag()]);
        digest.update(candidate.rowid.to_be_bytes());
        digest.update(candidate.retained_bytes.to_be_bytes());
        hash_field(&mut digest, reason.as_bytes());
        self.frontier.prefix_sha256 = digest.finalize().into();
    }
}

pub(super) struct KiroCandidate {
    pub(super) phase: KiroPhase,
    pub(super) rowid: i64,
    pub(super) row_ordinal: u64,
    pub(super) retained_bytes: u64,
    pub(super) type_valid: [bool; 5],
}

impl KiroCandidate {
    pub(super) fn rejection_reason(&self) -> Option<&'static str> {
        let [key, conversation_id, value, created_at, updated_at] = self.type_valid;
        if !key {
            return Some("Kiro conversation key has an unsupported SQLite storage class");
        }
        if self.phase == KiroPhase::V2 && !conversation_id {
            return Some(
                "Kiro conversations_v2.conversation_id has an unsupported SQLite storage class",
            );
        }
        if !value {
            return Some("Kiro conversation value has an unsupported SQLite storage class");
        }
        if self.phase == KiroPhase::V2 && !created_at {
            return Some(
                "Kiro conversations_v2.created_at has an unsupported SQLite storage class",
            );
        }
        if self.phase == KiroPhase::V2 && !updated_at {
            return Some(
                "Kiro conversations_v2.updated_at has an unsupported SQLite storage class",
            );
        }
        None
    }
}

pub(super) fn next_candidate(
    connection: &Connection,
    phase: KiroPhase,
    after_rowid: Option<i64>,
    row_ordinal: u64,
) -> Result<Option<KiroCandidate>> {
    let table = phase.table();
    let where_clause = if after_rowid.is_some() {
        " where rowid > ?1"
    } else {
        ""
    };
    let fields = match phase {
        KiroPhase::V2 => {
            "coalesce(octet_length(key), 0) + coalesce(octet_length(conversation_id), 0) + \
             coalesce(octet_length(value), 0), typeof(key) = 'text', \
             typeof(conversation_id) = 'text', typeof(value) = 'text', \
             typeof(created_at) in ('null', 'integer'), \
             typeof(updated_at) in ('null', 'integer')"
        }
        KiroPhase::Legacy => {
            "coalesce(octet_length(key), 0) + coalesce(octet_length(value), 0), \
             typeof(key) = 'text', 1, typeof(value) = 'text', 1, 1"
        }
    };
    let sql = format!("select rowid, {fields} from {table}{where_clause} order by rowid limit 1");
    let _guard = SqliteLengthPreflightGuard::new(connection);
    let mut statement = connection.prepare(&sql)?;
    let read = |row: &rusqlite::Row<'_>| {
        let bytes = row.get::<_, i64>(1)?;
        Ok(KiroCandidate {
            phase,
            rowid: row.get(0)?,
            row_ordinal,
            retained_bytes: u64::try_from(bytes).unwrap_or(u64::MAX),
            type_valid: [
                row.get::<_, i64>(2)? != 0,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, i64>(4)? != 0,
                row.get::<_, i64>(5)? != 0,
                row.get::<_, i64>(6)? != 0,
            ],
        })
    };
    match after_rowid {
        Some(rowid) => statement
            .query_row([rowid], read)
            .optional()
            .map_err(Into::into),
        None => statement.query_row([], read).optional().map_err(Into::into),
    }
}

pub(super) fn candidate_at(
    connection: &Connection,
    phase: KiroPhase,
    rowid: i64,
    row_ordinal: u64,
) -> Result<Option<KiroCandidate>> {
    let candidate = match rowid.checked_sub(1) {
        Some(prior) => next_candidate(connection, phase, Some(prior), row_ordinal)?,
        None => next_candidate(connection, phase, None, row_ordinal)?,
    };
    Ok(candidate.filter(|candidate| candidate.rowid == rowid))
}

pub(super) fn hydrate_row(
    connection: &Connection,
    phase: KiroPhase,
    rowid: i64,
) -> Result<KiroConversationRow> {
    match phase {
        KiroPhase::V2 => connection
            .query_row(
                "select rowid, key, conversation_id, value, created_at, updated_at \
                 from conversations_v2 where rowid = ?1",
                [rowid],
                |row| {
                    Ok(KiroConversationRow {
                        table: "conversations_v2",
                        rowid: row.get(0)?,
                        key: row.get(1)?,
                        conversation_id: Some(row.get(2)?),
                        value: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )
            .map_err(Into::into),
        KiroPhase::Legacy => connection
            .query_row(
                "select rowid, key, value from conversations where rowid = ?1",
                [rowid],
                |row| {
                    Ok(KiroConversationRow {
                        table: "conversations",
                        rowid: row.get(0)?,
                        key: row.get(1)?,
                        conversation_id: None,
                        value: row.get(2)?,
                        created_at: None,
                        updated_at: None,
                    })
                },
            )
            .map_err(Into::into),
    }
}

fn attach_kiro_complete_content(
    event: &mut KiroNativeEvent,
    locator: &NativeLocator,
    values: &[NativeSqliteValue],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != ctx_history_core::EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported SQLite message route must have a verified-content profile",
    ))?;
    let native_record_id = event
        .provider_event_hash
        .clone()
        .unwrap_or_else(|| event.cursor.clone());
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        native_record_id,
        kiro_record_digest(values),
    )
    .ok_or(CaptureError::SystemInvariant(
        "SQLite complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

fn kiro_record_digest(values: &[NativeSqliteValue]) -> CompleteContentBodyDigest {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
        .expect("SHA-256 formatter must return a valid digest")
}

pub(super) struct KiroCorePage {
    pub(super) expected_frontier: KiroFrontier,
    pub(super) next_frontier: KiroFrontier,
    pub(super) terminal: bool,
    pub(super) retained_bytes: usize,
    pub(super) fact: Option<KiroSessionFact>,
    pub(super) events: Vec<KiroPreparedEvent>,
    pub(super) rejections: Vec<KiroRejection>,
}

impl KiroCorePage {
    pub(super) fn accepted_content_records(&self) -> usize {
        self.events
            .iter()
            .fold(self.events.len(), |records, event| {
                records.saturating_add(event.touches.len())
            })
    }

    pub(super) fn logical_units(&self) -> usize {
        KIRO_PAGE_OVERHEAD_UNITS
            .saturating_add(self.accepted_content_records())
            .saturating_add(self.rejections.len())
    }

    fn terminal_empty(expected_frontier: KiroFrontier, next_frontier: KiroFrontier) -> Self {
        Self {
            expected_frontier,
            next_frontier,
            terminal: true,
            retained_bytes: KIRO_PAGE_BASE_BYTES,
            fact: None,
            events: Vec::new(),
            rejections: Vec::new(),
        }
    }

    fn rejected(
        expected_frontier: KiroFrontier,
        next_frontier: KiroFrontier,
        terminal: bool,
        line: u64,
        reason: String,
    ) -> Self {
        Self {
            expected_frontier,
            next_frontier,
            terminal,
            retained_bytes: KIRO_PAGE_BASE_BYTES.saturating_add(reason.len()),
            fact: None,
            events: Vec::new(),
            rejections: vec![KiroRejection { line, reason }],
        }
    }
}

pub(super) struct KiroSessionFact {
    pub(super) table: &'static str,
    pub(super) rowid: i64,
    pub(super) key: String,
    pub(super) provider_session_id: String,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: DateTime<Utc>,
    pub(super) history_len: usize,
}

pub(super) struct KiroPreparedEvent {
    pub(super) event: KiroNativeEvent,
    pub(super) touches: Vec<KiroFileTouch>,
}

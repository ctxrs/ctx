use super::*;

pub(crate) struct GeminiNativePageReader<'a> {
    pub(super) source: &'a GeminiTranscriptSource,
    pub(super) source_file: crate::common::io::OpenedProviderSourceFile,
    pub(super) previous: Option<&'a GeminiPreviousSource>,
    pub(super) initial_observation: GeminiFileObservation,
    pub(super) source_hasher: Sha256,
    pub(super) resumed_prefix: bool,
    pub(super) skip_scan: bool,
    pub(super) reader: BufReader<File>,
    pub(super) prefix_hasher: Sha256,
    pub(super) offset: u64,
    pub(super) raw_ordinal: u64,
    pub(super) complete_prefix_end: u64,
    pub(super) append_boundary_safe: bool,
    pub(super) terminal: bool,
    pub(super) retained_event_count: u64,
    pub(super) state: ScanState<'a>,
    pub(super) outcome: Option<GeminiScanOutcome>,
}

pub(super) struct ScanState<'a> {
    pub(super) source: &'a GeminiTranscriptSource,
    pub(super) session: Option<GeminiSession>,
    pub(super) metrics: GeminiParserMetrics,
    pub(super) rejected_records: u64,
    pub(super) rejections: Vec<GeminiRejection>,
    pub(super) retained_rows_this_scan: u64,
    pub(super) emitted_rows_this_scan: u64,
}

struct GeminiReaderPosition {
    prefix_hasher: Sha256,
    source_hasher: Sha256,
    offset: u64,
    raw_ordinal: u64,
    complete_prefix_end: u64,
    append_boundary_safe: bool,
    terminal: bool,
    retained_event_count: u64,
    metrics: GeminiParserMetrics,
    rejected_records: u64,
    rejection_details: usize,
    retained_rows_this_scan: u64,
    emitted_rows_this_scan: u64,
    session_was_absent: bool,
}

struct ScannedGeminiRecord {
    events: Vec<(GeminiRetainedEvent, usize)>,
    rejections: Vec<GeminiRejection>,
    native_event_id: Option<String>,
    completed: bool,
}

impl<'a> GeminiNativePageReader<'a> {
    /// Returns the next bounded page. The caller must drain through `None` to
    /// obtain the final source revalidation and scanner outcome.
    pub(crate) fn next_page(&mut self) -> GeminiScanResult<Option<GeminiNativePage>> {
        if self.outcome.is_some() {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish()?;
            return Ok(None);
        }

        let expected_frontier = self.frontier();
        let initial_page_bytes =
            core_page_conservative_bytes(&expected_frontier, &expected_frontier, 0, 0).ok_or(
                GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini page accounting overflowed",
                )),
            )?;
        let mut page = GeminiNativePage {
            identity: GeminiPageIdentity([0; 32]),
            expected_frontier: expected_frontier.clone(),
            next_safe_frontier: expected_frontier,
            terminal: false,
            events: Vec::new(),
            rejections: Vec::new(),
            physical_records: 0,
            logical_units: 0,
            retained_event_bytes: 0,
            conservative_serialized_bytes: initial_page_bytes,
        };
        // Cross-restart/source-wide duplicate authority belongs to canonical
        // event IDs at the bounded consumer. The provider rejects only IDs
        // that conflict inside the independently retryable page it owns.
        let mut page_native_event_ids = GeminiNativeEventIds::default();

        while page.physical_records < MAX_GEMINI_NATIVE_PAGE_RECORDS {
            let position = self.position();
            let mut record = match self.scan_next_record(&page_native_event_ids) {
                Ok(Some(record)) => record,
                Ok(None) => {
                    self.finish()?;
                    break;
                }
                Err(error) => {
                    self.restore(position)?;
                    if page.physical_records == 0 {
                        return Err(error);
                    }
                    break;
                }
            };
            if !record.completed {
                self.finish()?;
                break;
            }

            let record_units = record
                .events
                .len()
                .checked_add(record.rejections.len())
                .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini Core page logical-unit accounting overflowed",
                )))?;
            let record_event_bytes = record
                .events
                .iter()
                .try_fold(0_usize, |total, (_, bytes)| total.checked_add(*bytes))
                .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini retained-event page byte count overflowed",
                )))?;
            let record_rejection_bytes = record
                .rejections
                .iter()
                .try_fold(0_usize, |total, rejection| {
                    total.checked_add(rejection_wire_bytes(rejection)?)
                });
            let record_rejection_bytes = record_rejection_bytes.ok_or(GeminiScanError::Capture(
                CaptureError::SystemInvariant(
                    "Gemini structural rejection page byte count overflowed",
                ),
            ))?;
            let page_rejection_bytes = page
                .rejections
                .iter()
                .try_fold(record_rejection_bytes, |total, rejection| {
                    total.checked_add(rejection_wire_bytes(rejection)?)
                });
            let page_rejection_bytes = page_rejection_bytes.ok_or(GeminiScanError::Capture(
                CaptureError::SystemInvariant(
                    "Gemini structural rejection page byte count overflowed",
                ),
            ))?;
            let next_units =
                page.logical_units
                    .checked_add(record_units)
                    .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                        "Gemini page logical-unit accounting overflowed",
                    )))?;
            let next_event_bytes = page
                .retained_event_bytes
                .checked_add(record_event_bytes)
                .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini retained-event page byte count overflowed",
                )))?;
            let next_safe_frontier = self.frontier();
            let next_page_bytes = core_page_conservative_bytes(
                &page.expected_frontier,
                &next_safe_frontier,
                next_event_bytes,
                page_rejection_bytes,
            )
            .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                "Gemini Core page accounting overflowed",
            )))?;
            let too_many_units = next_units > MAX_GEMINI_NATIVE_PAGE_RECORDS;
            let too_many_bytes = next_page_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES;
            if too_many_units || too_many_bytes {
                let raw_ordinal = position.raw_ordinal;
                let byte_start = position.offset;
                let byte_end_exclusive = self.offset;
                if page.physical_records != 0 {
                    self.restore(position)?;
                    break;
                }
                let reason = if too_many_units {
                    format!(
                        "Gemini native record expands to {record_units} logical units; \
                         page maximum is {MAX_GEMINI_NATIVE_PAGE_RECORDS}"
                    )
                } else {
                    format!(
                        "Gemini native record expands to {next_page_bytes} conservative Core \
                         serialized bytes; page maximum is {MAX_GEMINI_NATIVE_PAGE_BYTES}"
                    )
                };
                self.restore(position)?;
                return Err(GeminiScanError::UncommittedRecord {
                    raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    reason,
                });
            }

            page.physical_records =
                page.physical_records
                    .checked_add(1)
                    .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                        "Gemini physical-record page count overflowed",
                    )))?;
            if let Some(native_event_id) = record.native_event_id.take() {
                page_native_event_ids.commit_at(native_event_id, position.raw_ordinal);
            }
            page.logical_units = next_units;
            page.retained_event_bytes = next_event_bytes;
            let emitted_events = record.events.len() as u64;
            page.events
                .extend(record.events.into_iter().map(|(event, _)| event));
            self.state.emitted_rows_this_scan = self
                .state
                .emitted_rows_this_scan
                .saturating_add(emitted_events);
            page.rejections.extend(record.rejections);
            page.next_safe_frontier = next_safe_frontier;
            page.conservative_serialized_bytes = next_page_bytes;
        }

        if self.outcome.is_none()
            && page.physical_records == MAX_GEMINI_NATIVE_PAGE_RECORDS
            && self.reader.fill_buf()?.is_empty()
        {
            self.finish()?;
        }
        if page.physical_records == 0 {
            Ok(None)
        } else {
            self.certify_source_range()?;
            page.terminal = self
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.checkpoint.terminal);
            page.identity = derive_page_identity(
                &page.expected_frontier,
                &page.next_safe_frontier,
                &page.events,
                &page.rejections,
                page.terminal,
            );
            debug_assert!(page.physical_records <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
            debug_assert!(page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
            debug_assert!(page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
            Ok(Some(page))
        }
    }

    pub(crate) fn outcome(&self) -> Option<&GeminiScanOutcome> {
        self.outcome.as_ref()
    }

    fn certify_source_range(&self) -> GeminiScanResult<()> {
        if GeminiFileObservation::from_metadata(&self.reader.get_ref().metadata()?)?
            != self.initial_observation
        {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        self.source_file.revalidate_leaf()?;
        Ok(())
    }

    fn frontier(&self) -> GeminiPageFrontier {
        GeminiPageFrontier {
            parser_revision: GEMINI_NATIVEPATH_PARSER_REVISION,
            policy_revision: GEMINI_NATIVEPATH_POLICY_REVISION,
            complete_prefix_end: self.complete_prefix_end,
            complete_prefix_sha256: prefix_digest(&self.prefix_hasher),
            source_device: self.initial_observation.device,
            source_inode: self.initial_observation.inode,
            next_raw_ordinal: self.raw_ordinal,
            retained_event_count: self
                .retained_event_count
                .saturating_add(self.state.retained_rows_this_scan),
            rejected_records: self.state.rejected_records,
            append_boundary_safe: self.append_boundary_safe,
            session: self.state.session.clone(),
        }
    }

    fn position(&self) -> GeminiReaderPosition {
        GeminiReaderPosition {
            prefix_hasher: self.prefix_hasher.clone(),
            source_hasher: self.source_hasher.clone(),
            offset: self.offset,
            raw_ordinal: self.raw_ordinal,
            complete_prefix_end: self.complete_prefix_end,
            append_boundary_safe: self.append_boundary_safe,
            terminal: self.terminal,
            retained_event_count: self.retained_event_count,
            metrics: self.state.metrics.clone(),
            rejected_records: self.state.rejected_records,
            rejection_details: self.state.rejections.len(),
            retained_rows_this_scan: self.state.retained_rows_this_scan,
            emitted_rows_this_scan: self.state.emitted_rows_this_scan,
            session_was_absent: self.state.session.is_none(),
        }
    }

    fn restore(&mut self, position: GeminiReaderPosition) -> GeminiScanResult<()> {
        self.reader.seek(SeekFrom::Start(position.offset))?;
        self.prefix_hasher = position.prefix_hasher;
        self.source_hasher = position.source_hasher;
        self.offset = position.offset;
        self.raw_ordinal = position.raw_ordinal;
        self.complete_prefix_end = position.complete_prefix_end;
        self.append_boundary_safe = position.append_boundary_safe;
        self.terminal = position.terminal;
        self.retained_event_count = position.retained_event_count;
        self.state.metrics = position.metrics;
        self.state.rejected_records = position.rejected_records;
        self.state.rejections.truncate(position.rejection_details);
        self.state.retained_rows_this_scan = position.retained_rows_this_scan;
        self.state.emitted_rows_this_scan = position.emitted_rows_this_scan;
        if position.session_was_absent {
            self.state.session = None;
        }
        // Once present, the session is immutable: later headers are rejected
        // without replacing it. Only the absent-to-present transition needs
        // explicit rollback here, avoiding a per-record session clone.
        Ok(())
    }

    fn scan_next_record(
        &mut self,
        page_native_event_ids: &GeminiNativeEventIds,
    ) -> GeminiScanResult<Option<ScannedGeminiRecord>> {
        let mut line = Vec::new();
        let prefix_before_record = self.prefix_hasher.clone();
        let Some(record) = read_record(
            &mut self.reader,
            &mut line,
            &mut self.prefix_hasher,
            &mut self.source_hasher,
        )?
        else {
            return Ok(None);
        };
        let byte_start = self.offset;
        self.offset = self.offset.saturating_add(record.bytes_observed);
        let byte_end_exclusive = self.offset;
        let payload = trim_jsonl_ending(&line);

        // Gemini appends records in place. No unterminated final physical
        // record is committed, even if its current bytes form valid JSON or
        // exceed the line limit.
        if !record.terminated {
            self.prefix_hasher = prefix_before_record;
            self.terminal = false;
            return Ok(Some(ScannedGeminiRecord {
                events: Vec::new(),
                rejections: Vec::new(),
                native_event_id: None,
                completed: false,
            }));
        }

        if record.oversized || payload.len() > MAX_PROVIDER_JSONL_LINE_BYTES {
            let rejection = self.state.reject(
                self.raw_ordinal,
                byte_start,
                byte_end_exclusive,
                format!(
                    "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit \
                     (observed {} bytes)",
                    record.bytes_observed
                ),
            );
            self.observe_native_record(record.bytes_observed);
            self.complete_record(record.terminated);
            return Ok(Some(ScannedGeminiRecord {
                events: Vec::new(),
                rejections: vec![rejection],
                native_event_id: None,
                completed: true,
            }));
        }
        if payload.iter().all(u8::is_ascii_whitespace) {
            self.complete_prefix_end = self.offset;
            self.append_boundary_safe = record.terminated;
            return Ok(Some(ScannedGeminiRecord {
                events: Vec::new(),
                rejections: Vec::new(),
                native_event_id: None,
                completed: true,
            }));
        }

        let source_record = GeminiSourceRecordEvidence {
            byte_offset: byte_start,
            byte_length: byte_end_exclusive.saturating_sub(byte_start),
            record_digest: Sha256::digest(&line).into(),
        };
        let probe = match serde_json::from_slice::<GeminiRecordProbe>(payload) {
            Ok(probe) => probe,
            Err(error) => {
                let rejection = self.state.reject(
                    self.raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    format!("malformed Gemini JSONL: {error}"),
                );
                self.observe_native_record(record.bytes_observed);
                self.complete_record(record.terminated);
                return Ok(Some(ScannedGeminiRecord {
                    events: Vec::new(),
                    rejections: vec![rejection],
                    native_event_id: None,
                    completed: true,
                }));
            }
        };

        self.observe_native_record(record.bytes_observed);
        let class = probe.classify();
        let native_event_id = (class != GeminiRecordClass::Header)
            .then(|| nonempty(probe.id.clone()))
            .flatten();
        if let Some(native_event_id) = native_event_id.as_deref() {
            if let Err(error) = page_native_event_ids.validate(native_event_id, self.raw_ordinal) {
                let rejection = self.state.reject(
                    self.raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    error.to_string(),
                );
                self.complete_record(record.terminated);
                return Ok(Some(ScannedGeminiRecord {
                    events: Vec::new(),
                    rejections: vec![rejection],
                    native_event_id: None,
                    completed: true,
                }));
            }
        }
        if class != GeminiRecordClass::Header && self.state.session.is_none() {
            let rejection = self.state.reject(
                self.raw_ordinal,
                byte_start,
                byte_end_exclusive,
                format!(
                    "{}: record appeared before an importable native JSONL session header",
                    self.source.path.display()
                ),
            );
            self.complete_record(record.terminated);
            return Ok(Some(ScannedGeminiRecord {
                events: Vec::new(),
                rejections: vec![rejection],
                native_event_id: None,
                completed: true,
            }));
        }
        let mut events = Vec::new();
        let rejections = Vec::new();
        match class {
            GeminiRecordClass::Header => {
                if self.state.session.is_some() {
                    return Err(GeminiScanError::UncommittedRecord {
                        raw_ordinal: self.raw_ordinal,
                        byte_start,
                        byte_end_exclusive,
                        reason: "a second Gemini session header appeared in one transcript"
                            .to_owned(),
                    });
                } else {
                    let session =
                        hydrate_header(payload, &self.state.source.layout).map_err(|reason| {
                            GeminiScanError::UncommittedRecord {
                                raw_ordinal: self.raw_ordinal,
                                byte_start,
                                byte_end_exclusive,
                                reason,
                            }
                        })?;
                    self.state.session = Some(session);
                    self.state.metrics.header_records =
                        self.state.metrics.header_records.saturating_add(1);
                }
            }
            GeminiRecordClass::Result => {
                self.state.metrics.native_result_records_observed = self
                    .state
                    .metrics
                    .native_result_records_observed
                    .saturating_add(1);
                self.state.metrics.native_result_record_bytes_observed = self
                    .state
                    .metrics
                    .native_result_record_bytes_observed
                    .saturating_add(record.bytes_observed);
                let Some(session) = self.state.session.as_ref() else {
                    return Err(GeminiScanError::UncommittedRecord {
                        raw_ordinal: self.raw_ordinal,
                        byte_start,
                        byte_end_exclusive,
                        reason: "Gemini result appeared before an importable session header"
                            .to_owned(),
                    });
                };
                let hydrated = match hydrate_result_record(
                    payload,
                    GeminiNativePathProfile::CoreOnly,
                    self.source,
                    session,
                    self.raw_ordinal,
                    source_record,
                    byte_start,
                    byte_end_exclusive,
                ) {
                    Ok(hydrated) => hydrated,
                    Err(reason) => {
                        return Ok(Some(self.reject_completed_record(
                            byte_start,
                            byte_end_exclusive,
                            reason,
                            record.terminated,
                        )));
                    }
                };
                self.state.metrics.result_body_bytes_decoded_or_allocated = self
                    .state
                    .metrics
                    .result_body_bytes_decoded_or_allocated
                    .saturating_add(hydrated.decoded_body_bytes);
                self.state.metrics.result_body_hashes_created = self
                    .state
                    .metrics
                    .result_body_hashes_created
                    .saturating_add(hydrated.failure_diagnostics as u64);
                self.state.metrics.result_previews_created = self
                    .state
                    .metrics
                    .result_previews_created
                    .saturating_add(hydrated.failure_previews as u64);
                for (event, event_bytes) in &hydrated.events {
                    if *event_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES {
                        return Ok(Some(self.reject_completed_record(
                            byte_start,
                            byte_end_exclusive,
                            format!(
                                "Gemini retained output diagnostic exceeds the \
                                 {MAX_GEMINI_NATIVE_PAGE_BYTES} byte page limit"
                            ),
                            record.terminated,
                        )));
                    }
                    self.state.count_retained(event);
                }
                events = hydrated.events;
            }
            GeminiRecordClass::Message
            | GeminiRecordClass::ToolCall
            | GeminiRecordClass::StateNotice
            | GeminiRecordClass::RewindNotice => {
                if self.state.session.is_none() {
                    return Err(GeminiScanError::UncommittedRecord {
                        raw_ordinal: self.raw_ordinal,
                        byte_start,
                        byte_end_exclusive,
                        reason: "record appeared before an importable Gemini session header"
                            .to_owned(),
                    });
                } else {
                    match hydrate_retained_event(payload, class, self.raw_ordinal, source_record) {
                        Ok(Some(mut hydrated)) => {
                            if hydrated.event.occurred_at.is_none() {
                                hydrated.event.occurred_at = self
                                    .state
                                    .session
                                    .as_ref()
                                    .and_then(|session| session.started_at);
                            }
                            match retained_event_bytes(&hydrated) {
                                Err(reason) => {
                                    return Ok(Some(self.reject_completed_record(
                                        byte_start,
                                        byte_end_exclusive,
                                        reason,
                                        record.terminated,
                                    )));
                                }
                                Ok(event_bytes) if event_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES => {
                                    return Ok(Some(self.reject_completed_record(
                                        byte_start,
                                        byte_end_exclusive,
                                        format!(
                                            "Gemini retained event exceeds the {MAX_GEMINI_NATIVE_PAGE_BYTES} byte page limit"
                                        ),
                                        record.terminated,
                                    )));
                                }
                                Ok(event_bytes) => {
                                    self.state.count_retained(&hydrated.event);
                                    events.push((hydrated.event, event_bytes));
                                }
                            }
                        }
                        Ok(None) => {
                            self.state.metrics.ignored_records =
                                self.state.metrics.ignored_records.saturating_add(1);
                        }
                        Err(GeminiHydrationError::Invalid(reason)) => {
                            return Ok(Some(self.reject_completed_record(
                                byte_start,
                                byte_end_exclusive,
                                reason,
                                record.terminated,
                            )));
                        }
                        Err(GeminiHydrationError::TouchOverflow(error)) => {
                            return Ok(Some(self.reject_completed_record(
                                byte_start,
                                byte_end_exclusive,
                                error.to_string(),
                                record.terminated,
                            )));
                        }
                    }
                }
            }
            GeminiRecordClass::Ignored => {
                self.state.metrics.ignored_records =
                    self.state.metrics.ignored_records.saturating_add(1);
            }
        }
        self.complete_record(record.terminated);
        Ok(Some(ScannedGeminiRecord {
            events,
            rejections,
            native_event_id,
            completed: true,
        }))
    }

    fn observe_native_record(&mut self, bytes_observed: u64) {
        self.state.metrics.native_records_observed =
            self.state.metrics.native_records_observed.saturating_add(1);
        self.state.metrics.native_record_bytes_observed = self
            .state
            .metrics
            .native_record_bytes_observed
            .saturating_add(bytes_observed);
    }

    fn complete_record(&mut self, terminated: bool) {
        self.raw_ordinal = self.raw_ordinal.saturating_add(1);
        self.complete_prefix_end = self.offset;
        self.append_boundary_safe = terminated;
    }

    fn reject_completed_record(
        &mut self,
        byte_start: u64,
        byte_end_exclusive: u64,
        reason: String,
        terminated: bool,
    ) -> ScannedGeminiRecord {
        let rejection = self
            .state
            .reject(self.raw_ordinal, byte_start, byte_end_exclusive, reason);
        self.complete_record(terminated);
        ScannedGeminiRecord {
            events: Vec::new(),
            rejections: vec![rejection],
            native_event_id: None,
            completed: true,
        }
    }

    fn finish(&mut self) -> GeminiScanResult<()> {
        self.certify_source_range()?;
        let final_observation =
            GeminiFileObservation::from_metadata(&self.reader.get_ref().metadata()?)?;
        self.source.authority.revalidate()?;
        self.retained_event_count = self
            .retained_event_count
            .saturating_add(self.state.retained_rows_this_scan);
        let checkpoint = GeminiCheckpoint {
            parser_revision: GEMINI_NATIVEPATH_PARSER_REVISION,
            policy_revision: GEMINI_NATIVEPATH_POLICY_REVISION,
            source_path: self.source.path.clone(),
            source_observation: final_observation.clone(),
            session: self.state.session.clone(),
            complete_prefix_end: self.complete_prefix_end,
            complete_prefix_sha256: prefix_digest(&self.prefix_hasher),
            source_sha256: prefix_digest(&self.source_hasher),
            next_raw_ordinal: self.raw_ordinal,
            retained_event_count: self.retained_event_count,
            rejected_records: self.state.rejected_records,
            append_boundary_safe: self.append_boundary_safe,
            terminal: self.terminal,
        };
        let cross_path_change = classify_cross_path_source(&checkpoint, self.previous);
        let signals = lifecycle_signals(
            &checkpoint,
            self.previous,
            self.resumed_prefix,
            self.state.emitted_rows_this_scan,
            cross_path_change,
        );
        self.outcome = Some(GeminiScanOutcome {
            checkpoint,
            signals,
            metrics: self.state.metrics.clone(),
            rejected_records: self.state.rejected_records,
            rejections: self.state.rejections.clone(),
            terminal_source_observation: final_observation,
        });
        Ok(())
    }
}

impl ScanState<'_> {
    fn reject(
        &mut self,
        raw_ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        reason: String,
    ) -> GeminiRejection {
        let rejection = GeminiRejection {
            raw_ordinal,
            byte_start,
            byte_end_exclusive,
            kind: GeminiRejectionKind::InvalidRecord,
            reason,
        };
        self.rejected_records = self.rejected_records.saturating_add(1);
        if self.rejections.len() < MAX_REJECTION_DETAILS {
            self.rejections.push(rejection.clone());
        }
        rejection
    }

    fn count_retained(&mut self, event: &GeminiRetainedEvent) {
        match event.event_type {
            EventType::Message => {
                self.metrics.retained_messages = self.metrics.retained_messages.saturating_add(1);
            }
            EventType::ToolCall => {
                self.metrics.retained_tool_calls =
                    self.metrics.retained_tool_calls.saturating_add(1);
            }
            EventType::Notice | EventType::Summary => {
                self.metrics.retained_notices = self.metrics.retained_notices.saturating_add(1);
            }
            _ => {}
        }
        self.metrics.retained_rows = self.metrics.retained_rows.saturating_add(1);
        self.retained_rows_this_scan = self.retained_rows_this_scan.saturating_add(1);
    }
}

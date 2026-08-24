use super::*;
use crate::provider::source_backed::ProviderRuntimeBinding;

impl CodexNativeScanner {
    pub(in crate::codex::nativepath) fn new_semantic(
        source: CodexCatalogSource,
        base_event_lookup: Option<impl crate::provider::source_backed::BaseEventLookup + 'static>,
    ) -> Result<Self> {
        let native_session_id = source.catalog_native_session_id.as_deref().ok_or_else(|| {
            CaptureError::from(CodexSourceBackedErrorV0::MissingNativeSessionId {
                path: source.source_path.clone(),
            })
        })?;
        let core_source = codex_source_key_in_root(source.source_root_lineage, native_session_id)?;
        let core_session_id = codex_session_identity(&core_source, native_session_id)?;
        Ok(Self {
            source,
            owner: None,
            session_metadata: Vec::new(),
            pending_calls: BTreeMap::new(),
            terminal_authority: CodexTerminalAuthority::default(),
            counters: CodexScanCounters::default(),
            local_turn_started: false,
            core_source,
            core_session_id,
            event_identity_state: base_event_lookup
                .map(CodexEventIdentityStateV0::for_append)
                .unwrap_or_default(),
            active_core_page: None,
            exhausted: false,
            ownership_quarantined: false,
        })
    }

    pub(in crate::codex::nativepath) fn restore_semantic_checkpoint(
        &mut self,
        checkpoint: &super::super::checkpoint::CodexSemanticCheckpoint,
    ) -> Result<()> {
        if !checkpoint.direct_append_safe()
            || self.owner.is_some()
            || !self.session_metadata.is_empty()
            || !self.pending_calls.is_empty()
            || !self
                .terminal_authority
                .restore(checkpoint.terminal_authority())
        {
            return Err(CaptureError::InvalidPayload(
                "Codex semantic checkpoint cannot resume this scanner".to_owned(),
            ));
        }
        self.owner = checkpoint.owner().cloned();
        if let Some(owner) = self.owner.clone() {
            self.session_metadata.push(owner);
        }
        self.local_turn_started = checkpoint.local_turn_started();
        self.pending_calls.clone_from(checkpoint.pending_calls());
        Ok(())
    }

    pub(in crate::codex::nativepath) fn preflight_semantic(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<impl ProviderRuntimeBinding>,
    ) -> Result<bool> {
        let direct_append = input.is_direct_append_resume();
        while let Some(record) = input.next_record()? {
            if !record.complete() {
                break;
            }
            if record.oversized() {
                self.terminal_authority.saturate();
            } else if !record.terminal_nul_padding() {
                let bytes = input.record_bytes(record)?;
                self.terminal_authority.observe_record(bytes);
                if !self.ownership_quarantined
                    && classify_codex_record(bytes)
                        .ok()
                        .filter(|probe| !probe.lineage_malformed())
                        .or_else(|| classify_after_selector_ambiguity(bytes))
                        .is_some_and(|probe| probe.class == CodexRecordClass::SessionMeta)
                {
                    match parse_session_meta(bytes) {
                        Some(metadata) => {
                            if let Err(error) = self.observe_session_metadata(metadata) {
                                match error {
                                    CaptureError::InvalidPayload(_) => self.quarantine_ownership(),
                                    error => return Err(error),
                                }
                            }
                        }
                        // A recognized ownership record whose complete schema
                        // is malformed cannot be safely ignored while sibling
                        // events from this rollout are published.
                        None => self.quarantine_ownership(),
                    }
                }
            }
        }
        if !self.ownership_quarantined {
            if let Err(error) = self.validate_session_metadata_owner() {
                match error {
                    // Ownership admission is the only semantic failure this
                    // preflight contains locally. I/O, source-change, and
                    // invariant errors still abort the route.
                    CaptureError::InvalidPayload(_) => self.quarantine_ownership(),
                    error => return Err(error),
                }
            }
        }
        // Ownership observations above are admission-only. Projection owns the
        // published counters and will re-observe the same metadata on a normal
        // scan after this seek-free streaming preflight settles.
        self.counters = CodexScanCounters::default();
        Ok(direct_append
            && (self.ownership_quarantined
                || self.terminal_authority.append_requires_replacement()))
    }

    fn quarantine_ownership(&mut self) {
        self.ownership_quarantined = true;
        // A direct-append checkpoint may have restored an admitted owner and
        // pending state before the full-file preflight finds ambiguity. Clear
        // it so this leaf cannot retain or project prior ownership state.
        self.owner = None;
        self.session_metadata.clear();
        self.pending_calls.clear();
        self.local_turn_started = false;
        self.event_identity_state = CodexEventIdentityStateV0::default();
        self.active_core_page = None;
    }

    pub(in crate::codex::nativepath) fn ownership_quarantined_source(
        &self,
    ) -> Option<&CodexCatalogSource> {
        self.ownership_quarantined.then_some(&self.source)
    }

    pub(in crate::codex::nativepath) fn next_semantic_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<impl ProviderRuntimeBinding>,
    ) -> Result<Option<CodexNativePage>> {
        if self.exhausted {
            return Ok(None);
        }
        if self.active_core_page.is_none() {
            self.active_core_page = Some(self.new_semantic_page(input)?);
        }

        loop {
            let input_offset = input.offset()?;
            let page_start = self.active_semantic_page()?.expected_offset;
            let page_progress = input_offset.checked_sub(page_start).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Codex semantic physical page progress regressed".to_owned(),
                )
            })?;
            let page = self.active_semantic_page()?;
            let core_is_full = page.records.len() >= MAX_CODEX_PAGE_UNITS
                || page.serialized_bytes > MAX_CODEX_PAGE_BYTES
                || page.physical_records >= MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS
                || page_progress >= MAX_CODEX_SOURCE_BACKED_PAGE_PROGRESS_BYTES;
            if core_is_full {
                return self.emit_active_semantic_page().map(Some);
            }

            let position = self.semantic_position(input)?;
            let Some(record) = input.next_record()? else {
                self.exhausted = true;
                return self.emit_semantic_end_page();
            };
            self.counters.bytes_read = self.counters.bytes_read.saturating_add(record.byte_len());
            self.counters.peak_line_buffer_bytes = self
                .counters
                .peak_line_buffer_bytes
                .max(record.stored_len());
            if !record.complete() {
                self.counters.incomplete_records =
                    self.counters.incomplete_records.saturating_add(1);
                if record.oversized() {
                    self.counters.oversized_records =
                        self.counters.oversized_records.saturating_add(1);
                }
                self.exhausted = true;
                return self.emit_semantic_end_page();
            }

            self.counters.complete_records = self.counters.complete_records.saturating_add(1);
            let mut projection = if self.ownership_quarantined {
                if record.terminal_nul_padding() {
                    self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                } else {
                    self.reject(record.oversized());
                }
                CodexRecordProjection::default()
            } else if record.terminal_nul_padding() {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                CodexRecordProjection::default()
            } else if record.oversized() {
                self.reject(true);
                CodexRecordProjection::default()
            } else {
                self.process_record(
                    input.record_bytes(record)?,
                    CodexPhysicalRecordContext {
                        raw_ordinal: record.physical_ordinal(),
                        start_byte: record.byte_start(),
                        end_byte: record.byte_end_exclusive(),
                    },
                )?
            };

            let page = self.active_semantic_page()?;
            let (record_units, record_bytes) = match projection.context_mutation.as_ref() {
                Some(CodexContextMutation::SourceBackedRow {
                    estimated_bytes, ..
                }) => (1, *estimated_bytes),
                None => (0, 0),
            };
            let next_units = page.records.len().saturating_add(record_units);
            let next_bytes = page.serialized_bytes.saturating_add(record_bytes);
            let next_byte_limit = if page.records.is_empty() && record_units == 1 {
                MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
            } else {
                MAX_CODEX_PAGE_BYTES
            };
            if next_units > MAX_CODEX_PAGE_UNITS || next_bytes > next_byte_limit {
                if page.physical_records != 0 {
                    self.restore_semantic(input, position)?;
                    return self.emit_active_semantic_page().map(Some);
                }
                self.reject(false);
                projection = CodexRecordProjection::default();
            } else {
                self.active_semantic_page()?.serialized_bytes = next_bytes;
            }
            if let Some(mutation) = projection.context_mutation.take() {
                self.apply_context_mutation(mutation)?;
            }
            let page = self.active_semantic_page()?;
            page.physical_records = page.physical_records.saturating_add(1);
        }
    }
}

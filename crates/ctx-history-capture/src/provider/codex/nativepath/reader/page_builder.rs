use super::*;

impl CodexNativeScanner {
    /// Drops the bounded physical-record scratch allocation before ownership
    /// of a prepared page crosses to the writer. The next window reserves and
    /// reacquires that input capacity through the shared preparation budget.
    pub(crate) fn release_transient_record_buffer(&mut self) {
        self.record_buffer = Vec::new();
    }

    pub(super) fn new_core_page(&mut self) -> Result<CodexNativePage> {
        let expected_frontier = self.frontier();
        let owner_bytes = if self.profile.projection_mode() == CodexProjectionMode::Legacy {
            let owner_bytes = self
                .owner
                .as_ref()
                .map(serialized_owner_bytes)
                .transpose()?
                .unwrap_or_default();
            if self.owner.is_some() {
                self.counters.legacy_page_owner_json_serializations = self
                    .counters
                    .legacy_page_owner_json_serializations
                    .saturating_add(1);
            }
            owner_bytes
        } else {
            0
        };
        Ok(CodexNativePage {
            identity: CodexNativePageIdentity::default(),
            owner: self.owner.clone(),
            projection_mode: self.profile.projection_mode(),
            next_safe_frontier: expected_frontier.clone(),
            expected_frontier,
            core_rows: Vec::new(),
            source_backed_rows: Vec::new(),
            serialized_bytes: PAGE_FIXED_WIRE_BYTES.saturating_add(owner_bytes),
            physical_records: 0,
            terminal: false,
        })
    }

    pub(super) fn take_ready_page(&mut self) -> Option<CodexNativeOwnedPage> {
        self.ready_pro_page
            .take()
            .map(Box::new)
            .map(CodexNativeOwnedPage::Pro)
            .or_else(|| {
                self.ready_core_page
                    .take()
                    .map(Box::new)
                    .map(CodexNativeOwnedPage::Core)
            })
    }

    pub(super) fn emit_active_core_page(&mut self) -> Result<CodexNativeOwnedPage> {
        let page = self
            .active_core_page
            .take()
            .ok_or(CaptureError::SystemInvariant(
                "Codex NativePath has no active Core page to emit",
            ))?;
        Ok(CodexNativeOwnedPage::Core(Box::new(
            self.finish_page(page)?,
        )))
    }

    pub(super) fn queue_end_pages(&mut self, terminal: bool) -> Result<()> {
        if let Some(mut page) = self.active_core_page.take() {
            if page.has_progress() {
                page.terminal = terminal;
                self.ready_core_page = Some(self.finish_page(page)?);
            }
        }
        self.flush_pro_page()
    }

    pub(super) fn push_pro_output(
        &mut self,
        output: ProOutputObservation,
        serialized_bytes: usize,
        next_frontier: CodexNativeFrontier,
    ) -> Result<()> {
        let page = self.pro_page.as_ref().ok_or(CaptureError::SystemInvariant(
            "Codex NativePath produced Pro output without an active Pro lane",
        ))?;
        if page.units() >= MAX_CODEX_PAGE_UNITS
            || page
                .serialized_bytes
                .checked_add(serialized_bytes)
                .is_none_or(|bytes| bytes > MAX_CODEX_PAGE_BYTES)
        {
            self.flush_pro_page()?;
        }
        let page = self.pro_page.as_mut().ok_or(CaptureError::SystemInvariant(
            "Codex NativePath lost its active Pro page",
        ))?;
        if serialized_bytes > MAX_CODEX_PAGE_BYTES
            || page.units() >= MAX_CODEX_PAGE_UNITS
            || page
                .serialized_bytes
                .checked_add(serialized_bytes)
                .is_none_or(|bytes| bytes > MAX_CODEX_PAGE_BYTES)
        {
            return Err(CaptureError::SystemInvariant(
                "Codex NativePath Pro output was pushed past an individual page bound",
            ));
        }
        page.outputs.push(output);
        page.serialized_bytes = page.serialized_bytes.checked_add(serialized_bytes).ok_or(
            CaptureError::SystemInvariant("Codex NativePath Pro page byte count overflowed"),
        )?;
        page.next_safe_frontier = next_frontier;
        if page.units() == MAX_CODEX_PAGE_UNITS {
            self.flush_pro_page()?;
        }
        Ok(())
    }

    pub(super) fn flush_pro_page(&mut self) -> Result<()> {
        let Some(mut page) = self.pro_page.take() else {
            return Ok(());
        };
        let next = new_pro_page(page.next_safe_frontier.clone());
        if page.outputs.is_empty() {
            self.pro_page = Some(next);
            return Ok(());
        }
        if self.ready_pro_page.is_some() {
            return Err(CaptureError::SystemInvariant(
                "Codex NativePath attempted to queue multiple unacknowledged Pro pages",
            ));
        }
        debug_assert!(page.units() <= MAX_CODEX_PAGE_UNITS);
        debug_assert!(page.serialized_bytes <= MAX_CODEX_PAGE_BYTES);
        page.identity = pro_page_identity(&page)?;
        self.counters.pro_output_pages_emitted =
            self.counters.pro_output_pages_emitted.saturating_add(1);
        self.counters.peak_pro_page_rows = self.counters.peak_pro_page_rows.max(page.units());
        self.counters.peak_pro_page_bytes =
            self.counters.peak_pro_page_bytes.max(page.serialized_bytes);
        self.ready_pro_page = Some(page);
        self.pro_page = Some(next);
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<CodexSourceScan> {
        if !self.exhausted
            || self.active_core_page.is_some()
            || self.ready_core_page.is_some()
            || self.ready_pro_page.is_some()
            || self
                .pro_page
                .as_ref()
                .is_some_and(|page| !page.outputs.is_empty())
        {
            return Err(CaptureError::InvalidPayload(
                "Codex NativePath scan must drain every owned page before certification".to_owned(),
            ));
        }
        if let Some(mut replay) = self.replay.take() {
            let after = observed_file(&replay.source)?;
            if after != replay.before_observation {
                return Err(source_changed_during_scan());
            }
            replay.after_observation = after;
            return Ok(replay);
        }

        let full_revision_sha256 = self.full_hasher.finalize().into();
        let complete_prefix_sha256 = self.complete_hasher.finalize().into();
        let after = observed_file(&self.source)?;
        if after != self.before {
            return Err(source_changed_during_scan());
        }
        if let Some(owner) = self.owner.as_ref() {
            validate_catalog_owner(
                self.source.catalog_native_session_id.as_deref(),
                &owner.native_session_id,
            )?;
        }

        Ok(CodexSourceScan {
            source: self.source,
            before_observation: self.before,
            after_observation: after,
            disposition: self.disposition,
            prefix_proof: self.prefix_proof,
            resume_proof: self.resume_proof,
            full_revision_sha256,
            complete_prefix_sha256,
            complete_prefix_end: self
                .incomplete_tail
                .as_ref()
                .map(|tail| tail.start_byte)
                .unwrap_or(self.offset),
            next_raw_ordinal: self.raw_ordinal,
            owner: self.owner,
            pending_tool_authorities: self.tool_authorities.into_values().collect(),
            rejections: self.rejections,
            incomplete_tail: self.incomplete_tail,
            counters: self.counters,
        })
    }

    pub(super) fn position(&self) -> ScannerPosition {
        ScannerPosition {
            offset: self.offset,
            raw_ordinal: self.raw_ordinal,
            had_owner: self.owner.is_some(),
            complete_hasher: self.complete_hasher.clone(),
            full_hasher: self.full_hasher.clone(),
            rejection_len: self.rejections.len(),
            counters: self.counters,
        }
    }

    pub(super) fn restore(&mut self, position: ScannerPosition) -> Result<()> {
        let actual_parse_counts = (
            self.counters.prefiltered_records,
            self.counters.structural_json_parses,
            self.counters.typed_json_parses,
            self.counters.structural_output_probes,
            self.counters.typed_output_parses,
        );
        self.reader.seek(SeekFrom::Start(position.offset))?;
        self.offset = position.offset;
        self.raw_ordinal = position.raw_ordinal;
        if !position.had_owner {
            self.owner = None;
        }
        self.complete_hasher = position.complete_hasher;
        self.full_hasher = position.full_hasher;
        self.rejections.truncate(position.rejection_len);
        self.counters = position.counters;
        (
            self.counters.prefiltered_records,
            self.counters.structural_json_parses,
            self.counters.typed_json_parses,
            self.counters.structural_output_probes,
            self.counters.typed_output_parses,
        ) = actual_parse_counts;
        Ok(())
    }

    pub(super) fn frontier(&self) -> CodexNativeFrontier {
        CodexNativeFrontier {
            complete_prefix_end: self
                .incomplete_tail
                .as_ref()
                .map(|tail| tail.start_byte)
                .unwrap_or(self.offset),
            next_raw_ordinal: self.raw_ordinal,
            complete_prefix_sha256: self.complete_hasher.clone().finalize().into(),
        }
    }

    pub(super) fn finish_page(&mut self, mut page: CodexNativePage) -> Result<CodexNativePage> {
        page.owner = self.owner.clone();
        page.next_safe_frontier = self.frontier();
        debug_assert!(match page.projection_mode {
            CodexProjectionMode::Legacy => {
                page.physical_records <= MAX_CODEX_PAGE_UNITS as u64
            }
            CodexProjectionMode::SourceBackedV0 => {
                page.physical_records <= MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS
            }
        });
        debug_assert!(page.units() <= MAX_CODEX_PAGE_UNITS);
        debug_assert!(page.serialized_bytes <= MAX_CODEX_PAGE_BYTES);
        self.counters.emitted_pages = self.counters.emitted_pages.saturating_add(1);
        self.counters.peak_page_rows = self.counters.peak_page_rows.max(page.units());
        self.counters.peak_page_bytes = self.counters.peak_page_bytes.max(page.serialized_bytes);
        let (identity, operations) = core_page_identity(&page)?;
        page.identity = identity;
        self.counters.legacy_page_identity_owner_json_serializations = self
            .counters
            .legacy_page_identity_owner_json_serializations
            .saturating_add(operations.owner_json_serializations);
        self.counters.legacy_page_identity_row_json_serializations = self
            .counters
            .legacy_page_identity_row_json_serializations
            .saturating_add(operations.row_json_serializations);
        Ok(page)
    }
}

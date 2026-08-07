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
        Ok(CodexNativePage {
            owner: self.owner.clone(),
            next_safe_frontier: expected_frontier.clone(),
            expected_frontier,
            core_rows: Vec::new(),
            source_backed_rows: Vec::new(),
            serialized_bytes: PAGE_FIXED_WIRE_BYTES,
            physical_records: 0,
            terminal: false,
        })
    }

    pub(super) fn take_ready_page(&mut self) -> Option<CodexNativeOwnedPage> {
        self.ready_core_page
            .take()
            .map(Box::new)
            .map(CodexNativeOwnedPage::Core)
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
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<CodexSourceScan> {
        if !self.exhausted || self.active_core_page.is_some() || self.ready_core_page.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Codex NativePath scan must drain every owned page before certification".to_owned(),
            ));
        }
        if let Some(mut replay) = self.replay.take() {
            let current = opened_file_observation(&replay.source.source_path, self.opened.file())?;
            self.opened.revalidate_same_object()?;
            if current != replay.before_observation {
                revalidate_opened_prefix(
                    self.opened.file(),
                    replay.before_observation.len,
                    replay.full_revision_sha256,
                )?;
                self.opened.revalidate_same_object()?;
            }
            if let Some(mut lineage_facts) = self.lineage_facts.take() {
                lineage_facts.seal();
                replay.lineage_facts = Some(lineage_facts);
            }
            replay.after_observation = replay.before_observation.clone();
            return Ok(replay);
        }

        let full_revision_sha256: [u8; 32] = self.full_hasher.finalize().into();
        let complete_prefix_sha256 = self.complete_hasher.finalize().into();
        let current = opened_file_observation(&self.source.source_path, self.opened.file())?;
        self.opened.revalidate_same_object()?;
        if current != self.before {
            revalidate_opened_prefix(self.opened.file(), self.before.len, full_revision_sha256)?;
            self.opened.revalidate_same_object()?;
        }
        if let Some(owner) = self.owner.as_ref() {
            validate_catalog_owner(&self.source, owner.clone())?;
        }

        if let Some(lineage_facts) = self.lineage_facts.as_mut() {
            lineage_facts.seal();
        }
        Ok(CodexSourceScan {
            source: self.source,
            before_observation: self.before.clone(),
            after_observation: self.before,
            disposition: self.disposition,
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
            incomplete_tail: self.incomplete_tail,
            counters: self.counters,
            lineage_facts: self.lineage_facts,
        })
    }

    pub(super) fn position(&self) -> ScannerPosition {
        ScannerPosition {
            offset: self.offset,
            raw_ordinal: self.raw_ordinal,
            had_owner: self.owner.is_some(),
            complete_hasher: self.complete_hasher.clone(),
            full_hasher: self.full_hasher.clone(),
            counters: self.counters,
            lineage_mark: self.lineage_facts.as_ref().map(CodexLineageFactsV0::mark),
        }
    }

    pub(super) fn restore(&mut self, position: ScannerPosition) -> Result<()> {
        let actual_parse_counts = (
            self.counters.prefiltered_records,
            self.counters.structural_json_parses,
            self.counters.typed_json_parses,
            self.counters.structural_output_probes,
        );
        self.reader.seek(SeekFrom::Start(position.offset))?;
        self.offset = position.offset;
        self.raw_ordinal = position.raw_ordinal;
        if !position.had_owner {
            self.owner = None;
        }
        self.complete_hasher = position.complete_hasher;
        self.full_hasher = position.full_hasher;
        self.counters = position.counters;
        if let (Some(lineage_facts), Some(mark)) =
            (self.lineage_facts.as_mut(), position.lineage_mark)
        {
            lineage_facts.restore(mark);
        }
        (
            self.counters.prefiltered_records,
            self.counters.structural_json_parses,
            self.counters.typed_json_parses,
            self.counters.structural_output_probes,
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
        page.owner = self
            .owner
            .clone()
            .map(|owner| validate_catalog_owner(&self.source, owner))
            .transpose()?;
        page.next_safe_frontier = self.frontier();
        debug_assert!(page.physical_records <= MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS);
        debug_assert!(page.units() <= MAX_CODEX_PAGE_UNITS);
        debug_assert!(
            page.serialized_bytes <= MAX_CODEX_PAGE_BYTES
                || (page.source_backed_rows.len() == 1
                    && page.serialized_bytes <= MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES)
        );
        self.counters.emitted_pages = self.counters.emitted_pages.saturating_add(1);
        self.counters.peak_page_rows = self.counters.peak_page_rows.max(page.units());
        self.counters.peak_page_bytes = self.counters.peak_page_bytes.max(page.serialized_bytes);
        Ok(page)
    }
}

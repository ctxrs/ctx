use super::*;

impl ClaudeNativeScanner {
    pub(super) fn lane_at_physical_bound(&self) -> bool {
        self.core_page
            .as_ref()
            .is_some_and(|page| page.logical_units >= CLAUDE_MAX_PAGE_ROWS)
            || self
                .pro_page
                .as_ref()
                .is_some_and(|page| page.logical_units >= CLAUDE_MAX_PAGE_ROWS)
    }

    pub(super) fn flush_full_lanes(&mut self) -> Result<(), ClaudeNativePathError> {
        if self.profile == ClaudeNativeProfile::CoreAndPro {
            self.flush_core_page(false)?;
            self.flush_pro_page(false)?;
            return Ok(());
        }
        if self
            .core_page
            .as_ref()
            .is_some_and(|page| page.logical_units >= CLAUDE_MAX_PAGE_ROWS)
        {
            self.flush_core_page(false)?;
        }
        if self
            .pro_page
            .as_ref()
            .is_some_and(|page| page.logical_units >= CLAUDE_MAX_PAGE_ROWS)
        {
            self.flush_pro_page(false)?;
        }
        Ok(())
    }

    pub(super) fn queue_end_pages(&mut self, terminal: bool) -> Result<(), ClaudeNativePathError> {
        if self.profile.includes_core()
            && (self
                .core_page
                .as_ref()
                .is_some_and(|page| page.logical_units != 0)
                || !self.emitted_core)
        {
            self.flush_core_page(terminal)?;
        }
        if self.profile.includes_pro()
            && (self
                .pro_page
                .as_ref()
                .is_some_and(|page| page.logical_units != 0)
                || !self.emitted_pro)
        {
            self.flush_pro_page(terminal)?;
        }
        Ok(())
    }

    pub(super) fn flush_core_page(&mut self, terminal: bool) -> Result<(), ClaudeNativePathError> {
        let page =
            self.core_page
                .take()
                .ok_or_else(|| ClaudeNativePathError::InvalidCheckpoint {
                    reason: "Claude Core page is unavailable".to_owned(),
                })?;
        let next = CorePageBuilder::new(page.next_safe_frontier.clone());
        let finished = self.finish_core_page(page, terminal)?;
        if self.ready_core.replace(finished).is_some() {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "multiple unacknowledged Claude Core pages".to_owned(),
            });
        }
        self.core_page = Some(next);
        self.emitted_core = true;
        Ok(())
    }

    pub(super) fn flush_pro_page(&mut self, terminal: bool) -> Result<(), ClaudeNativePathError> {
        let page =
            self.pro_page
                .take()
                .ok_or_else(|| ClaudeNativePathError::InvalidCheckpoint {
                    reason: "Claude Pro page is unavailable".to_owned(),
                })?;
        let next = ProPageBuilder::new(page.next_safe_frontier.clone());
        let finished = self.finish_pro_page(page, terminal)?;
        if self.ready_pro.replace(finished).is_some() {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "multiple unacknowledged Claude Pro pages".to_owned(),
            });
        }
        self.pro_page = Some(next);
        self.emitted_pro = true;
        Ok(())
    }

    pub(super) fn finish_core_page(
        &mut self,
        page: CorePageBuilder,
        terminal: bool,
    ) -> Result<ClaudeNativePage, ClaudeNativePathError> {
        revalidate_open_file(&self.source, self.reader.get_ref(), &self.before)?;
        let certificate = page_certificate(&self.source, &page.next_safe_frontier);
        let serialized_bytes = core_encoded_bytes(
            &self.session,
            &page.expected_frontier,
            &page.next_safe_frontier,
            &page.rows,
            &page.rejections,
            page.rejected_records,
            page.logical_units,
            terminal,
            &certificate,
        )?;
        if page.logical_units > CLAUDE_MAX_PAGE_ROWS
            || page.rows.len() > CLAUDE_MAX_PAGE_ROWS
            || serialized_bytes > CLAUDE_MAX_PAGE_BYTES
        {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "Claude Core page escaped its certified bounds".to_owned(),
            });
        }
        let identity = core_page_identity(&self.session, &page, terminal, &certificate)?;
        self.stats.emitted_pages = self.stats.emitted_pages.saturating_add(1);
        self.stats.emitted_rows = self
            .stats
            .emitted_rows
            .saturating_add(u64::try_from(page.rows.len()).unwrap_or(u64::MAX));
        self.stats.peak_page_rows = self.stats.peak_page_rows.max(page.rows.len());
        self.stats.peak_page_bytes = self.stats.peak_page_bytes.max(serialized_bytes);
        Ok(ClaudeNativePage {
            identity,
            session: self.session.clone(),
            expected_frontier: page.expected_frontier,
            next_safe_frontier: page.next_safe_frontier,
            rows: page.rows,
            rejections: page.rejections,
            rejected_records: page.rejected_records,
            logical_units: page.logical_units,
            serialized_bytes,
            terminal,
            certificate,
        })
    }

    pub(super) fn finish_pro_page(
        &mut self,
        page: ProPageBuilder,
        terminal: bool,
    ) -> Result<ClaudeNativeProOutputPage, ClaudeNativePathError> {
        revalidate_open_file(&self.source, self.reader.get_ref(), &self.before)?;
        let certificate = page_certificate(&self.source, &page.next_safe_frontier);
        let serialized_bytes = pro_page_encoded_bytes(&page, &self.source, &certificate)?;
        if page.logical_units > CLAUDE_MAX_PAGE_ROWS
            || page.outputs.len() > CLAUDE_MAX_PAGE_ROWS
            || serialized_bytes > CLAUDE_MAX_PAGE_BYTES
        {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "Claude Pro page escaped its certified bounds".to_owned(),
            });
        }
        let identity = pro_page_identity(&page, terminal, &certificate)?;
        self.stats.emitted_pro_pages = self.stats.emitted_pro_pages.saturating_add(1);
        self.stats.emitted_pro_outputs = self
            .stats
            .emitted_pro_outputs
            .saturating_add(u64::try_from(page.outputs.len()).unwrap_or(u64::MAX));
        self.stats.peak_pro_page_outputs = self.stats.peak_pro_page_outputs.max(page.outputs.len());
        self.stats.peak_pro_page_bytes = self.stats.peak_pro_page_bytes.max(serialized_bytes);
        Ok(ClaudeNativeProOutputPage {
            identity,
            expected_frontier: page.expected_frontier,
            next_safe_frontier: page.next_safe_frontier,
            outputs: page.outputs,
            rejections: page.rejections,
            rejected_outputs: page.rejected_outputs,
            logical_units: page.logical_units,
            serialized_bytes,
            terminal,
            certificate,
        })
    }

    pub(super) fn take_ready(
        &mut self,
    ) -> Result<Option<ClaudeNativeOwnedPage>, ClaudeNativePathError> {
        if self.ready_pro.is_some() || self.ready_core.is_some() {
            // A sibling may have waited while its peer was consumed. Recheck
            // the pinned descriptor and route immediately before every page
            // leaves provider ownership.
            revalidate_open_file(&self.source, self.reader.get_ref(), &self.before)?;
        }
        Ok(self
            .ready_pro
            .take()
            .map(Box::new)
            .map(ClaudeNativeOwnedPage::Pro)
            .or_else(|| {
                self.ready_core
                    .take()
                    .map(Box::new)
                    .map(ClaudeNativeOwnedPage::Core)
            }))
    }

    pub(super) fn frontier(&self) -> ClaudeNativeFrontier {
        ClaudeNativeFrontier {
            complete_offset: self.offset,
            next_raw_ordinal: self.raw_ordinal,
            complete_record_chain_sha256: self.record_chain,
            boundary_proof_len: u32::try_from(self.boundary_window.bytes.len()).unwrap_or(u32::MAX),
            boundary_proof_sha256: boundary_proof_hash(&self.boundary_window.bytes),
            native_identity_chain_sha256: self.native_identity_chain,
            native_identity_records: self.native_identity_records,
            appendable_boundary: self.offset == 0 || self.last_complete_terminated,
        }
    }
}

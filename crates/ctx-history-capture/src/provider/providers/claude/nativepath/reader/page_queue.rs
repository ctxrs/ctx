use super::*;

impl ClaudeNativeScanner {
    pub(super) fn lane_at_physical_bound(&self) -> bool {
        self.core_page.logical_units >= CLAUDE_MAX_PAGE_ROWS
    }

    pub(super) fn flush_full_lanes(&mut self) -> Result<(), ClaudeNativePathError> {
        if self.core_page.logical_units >= CLAUDE_MAX_PAGE_ROWS {
            self.flush_core_page(false)?;
        }
        Ok(())
    }

    pub(super) fn queue_end_pages(&mut self, terminal: bool) -> Result<(), ClaudeNativePathError> {
        if self.core_page.logical_units != 0 || !self.emitted_core {
            self.flush_core_page(terminal)?;
        }
        Ok(())
    }

    pub(super) fn flush_core_page(&mut self, terminal: bool) -> Result<(), ClaudeNativePathError> {
        let frontier = self.frontier();
        let page = std::mem::replace(&mut self.core_page, CorePageBuilder::new(frontier));
        let next = CorePageBuilder::new(page.next_safe_frontier.clone());
        let finished = self.finish_core_page(page, terminal)?;
        if self.ready_core.replace(finished).is_some() {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "multiple unacknowledged Claude Core pages".to_owned(),
            });
        }
        self.core_page = next;
        self.emitted_core = true;
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
        self.stats.emitted_pages = self.stats.emitted_pages.saturating_add(1);
        self.stats.emitted_rows = self
            .stats
            .emitted_rows
            .saturating_add(u64::try_from(page.rows.len()).unwrap_or(u64::MAX));
        self.stats.peak_page_rows = self.stats.peak_page_rows.max(page.rows.len());
        self.stats.peak_page_bytes = self.stats.peak_page_bytes.max(serialized_bytes);
        Ok(ClaudeNativePage {
            session: self.session.clone(),
            rows: page.rows,
        })
    }

    pub(super) fn take_ready(&mut self) -> Result<Option<ClaudeNativePage>, ClaudeNativePathError> {
        if self.ready_core.is_some() {
            revalidate_open_file(&self.source, self.reader.get_ref(), &self.before)?;
        }
        Ok(self.ready_core.take())
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

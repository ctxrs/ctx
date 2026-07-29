use super::*;

impl CodexNativeScanner {
    pub(super) fn new(
        source: CodexCatalogSource,
        proof: Option<&CodexAppendProof>,
    ) -> Result<Self> {
        let opened = open_codex_source_capability(&source)?;
        Self::new_retained(source, opened, proof)
    }

    pub(super) fn new_retained(
        mut source: CodexCatalogSource,
        opened: Arc<OpenedProviderSourceFile>,
        proof: Option<&CodexAppendProof>,
    ) -> Result<Self> {
        source.opened = Some(Arc::clone(&opened));
        if let Some(proof) = proof {
            proof.validate_source(&source)?;
        }

        let before = observed_opened_file(&source, &opened)?;
        let file = opened.file().try_clone()?;
        let mut reader = BufReader::new(file);
        let validated = if let Some(proof) = proof {
            if before.len < proof.checkpoint.observation.len {
                return Err(invalid_checkpoint_proof(
                    "checkpoint generation is longer than the observed source",
                ));
            }
            Some(validate_checkpoint_source(
                &mut reader,
                &proof.checkpoint,
                before.len > proof.checkpoint.observation.len,
            )?)
        } else {
            None
        };

        if let (Some(proof), Some(validated)) = (
            proof.filter(|proof| proof.checkpoint.observation == before),
            validated.as_ref(),
        ) {
            validate_catalog_owner(
                source.catalog_native_session_id.as_deref(),
                &proof.checkpoint.owner.native_session_id,
            )?;
            let incomplete_tail = proof
                .checkpoint
                .incomplete_tail()
                .map(|(byte_len, sha256)| CodexIncompleteTail {
                    raw_ordinal: proof.checkpoint.next_raw_ordinal(),
                    start_byte: proof.checkpoint.complete_prefix_end(),
                    byte_len,
                    sha256,
                });
            let replay = CodexSourceScan {
                source: source.clone(),
                before_observation: before.clone(),
                after_observation: before.clone(),
                disposition: CodexParseDisposition::ObservationReplay,
                full_revision_sha256: proof.checkpoint.full_revision_sha256,
                complete_prefix_sha256: proof.checkpoint.complete_prefix_sha256,
                complete_prefix_end: proof.checkpoint.complete_prefix_end(),
                next_raw_ordinal: proof.checkpoint.next_raw_ordinal(),
                owner: Some(proof.checkpoint.owner.clone()),
                pending_tool_authorities: proof.checkpoint.pending_tool_authorities().to_vec(),
                incomplete_tail,
                counters: CodexScanCounters {
                    bytes_read: validated.bytes_read,
                    checkpoint_validation_bytes: validated.bytes_read,
                    prefix_bytes_read: proof.checkpoint.complete_prefix_end(),
                    peak_line_buffer_bytes: CHECKPOINT_READ_BUFFER_BYTES
                        .min(usize::try_from(validated.bytes_read).unwrap_or(usize::MAX)),
                    ..CodexScanCounters::default()
                },
            };
            return Ok(Self {
                source,
                opened,
                before,
                reader,
                disposition: CodexParseDisposition::ObservationReplay,
                offset: replay.complete_prefix_end,
                raw_ordinal: replay.next_raw_ordinal,
                owner: replay.owner.clone(),
                tool_contexts: BTreeMap::new(),
                tool_authorities: BTreeMap::new(),
                complete_hasher: Sha256::new(),
                full_hasher: Sha256::new(),
                record_buffer: Vec::new(),
                incomplete_tail: None,
                counters: replay.counters,
                replay: Some(replay),
                active_core_page: None,
                ready_core_page: None,
                exhausted: true,
            });
        }

        let (
            disposition,
            owner,
            tool_contexts,
            tool_authorities,
            raw_ordinal,
            offset,
            complete_hasher,
            validation_bytes,
        ) = match (proof, validated) {
            (Some(proof), Some(validated)) if before.len > proof.checkpoint.observation.len => {
                let ValidatedCheckpoint {
                    bytes_read,
                    complete_prefix_hasher,
                    complete_prefix_ends_with_terminal_nul_padding,
                    pending_tool_contexts: tool_contexts,
                    pending_tool_authorities: tool_authorities,
                } = validated;
                if complete_prefix_ends_with_terminal_nul_padding {
                    return Err(invalid_checkpoint_proof(
                        "terminal NUL padding is not an append boundary",
                    ));
                }
                reader.seek(SeekFrom::Start(proof.checkpoint.complete_prefix_end()))?;
                (
                    CodexParseDisposition::AppendDelta,
                    Some(proof.checkpoint.owner.clone()),
                    tool_contexts,
                    tool_authorities,
                    proof.checkpoint.next_raw_ordinal(),
                    proof.checkpoint.complete_prefix_end(),
                    complete_prefix_hasher,
                    bytes_read,
                )
            }
            (Some(_), Some(_)) => {
                return Err(invalid_checkpoint_proof(
                    "checkpoint generation is neither an exact replay nor an append prefix",
                ));
            }
            (None, None) => {
                reader.seek(SeekFrom::Start(0))?;
                (
                    CodexParseDisposition::FullGeneration,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                    0,
                    0,
                    Sha256::new(),
                    0,
                )
            }
            _ => {
                return Err(CaptureError::SystemInvariant(
                    "Codex checkpoint validation state is incomplete",
                ));
            }
        };

        Ok(Self {
            source,
            opened,
            before,
            reader,
            disposition,
            offset,
            raw_ordinal,
            owner,
            tool_contexts,
            tool_authorities,
            complete_hasher: complete_hasher.clone(),
            full_hasher: complete_hasher,
            record_buffer: Vec::new(),
            incomplete_tail: None,
            counters: CodexScanCounters {
                bytes_read: validation_bytes,
                checkpoint_validation_bytes: validation_bytes,
                prefix_bytes_read: offset,
                ..CodexScanCounters::default()
            },
            replay: None,
            active_core_page: None,
            ready_core_page: None,
            exhausted: false,
        })
    }

    pub(crate) fn next_page(&mut self) -> Result<Option<CodexNativeOwnedPage>> {
        if let Some(page) = self.take_ready_page() {
            return Ok(Some(page));
        }
        if self.exhausted {
            return Ok(None);
        }
        if self.active_core_page.is_none() {
            self.active_core_page = Some(self.new_core_page()?);
        }

        loop {
            let core_is_full = self.active_core_page.as_ref().is_some_and(|page| {
                page.units() >= MAX_CODEX_PAGE_UNITS
                    || page.physical_records >= MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS
                    || self
                        .offset
                        .saturating_sub(page.expected_frontier.complete_prefix_end)
                        >= MAX_CODEX_SOURCE_BACKED_PAGE_PROGRESS_BYTES
            });
            if core_is_full {
                return self.emit_active_core_page().map(Some);
            }

            let position = self.position();
            let record_start = self.offset;
            let record_read = {
                let reader = &mut self.reader;
                let record_buffer = &mut self.record_buffer;
                let full_hasher = &mut self.full_hasher;
                let complete_hasher = &mut self.complete_hasher;
                read_bounded_record(reader, record_buffer, full_hasher, complete_hasher)?
            };
            let Some(record_read) = record_read else {
                self.exhausted = true;
                self.queue_end_pages(true)?;
                return Ok(self.take_ready_page());
            };

            self.offset = self.offset.checked_add(record_read.byte_len).ok_or(
                CaptureError::SystemInvariant("Codex source offset exceeds u64"),
            )?;
            self.counters.bytes_read = self
                .counters
                .bytes_read
                .saturating_add(record_read.byte_len);
            self.counters.peak_line_buffer_bytes = self
                .counters
                .peak_line_buffer_bytes
                .max(record_read.stored_len);

            if !record_read.complete {
                self.incomplete_tail = Some(CodexIncompleteTail {
                    raw_ordinal: self.raw_ordinal,
                    start_byte: record_start,
                    byte_len: record_read.byte_len,
                    sha256: record_read.sha256,
                });
                self.counters.incomplete_records =
                    self.counters.incomplete_records.saturating_add(1);
                if record_read.oversized {
                    self.counters.oversized_records =
                        self.counters.oversized_records.saturating_add(1);
                }
                self.exhausted = true;
                self.queue_end_pages(false)?;
                return Ok(self.take_ready_page());
            }

            self.counters.complete_records = self.counters.complete_records.saturating_add(1);
            let record_end = self.offset;
            let mut projection = if record_read.terminal_nul_padding {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                CodexRecordProjection::default()
            } else if record_read.oversized {
                self.reject(true);
                CodexRecordProjection::default()
            } else {
                let record_buffer = std::mem::take(&mut self.record_buffer);
                let result = self.process_record(
                    &record_buffer[..record_read.stored_len],
                    record_start,
                    record_end,
                    record_read.sha256,
                );
                self.record_buffer = record_buffer;
                result?
            };

            let page = self
                .active_core_page
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex NativePath lost its active Core page",
                ))?;
            let next_units = page.units().saturating_add(projection.core_units());
            let next_bytes = page
                .serialized_bytes
                .saturating_add(projection.core_serialized_bytes);
            if next_units > MAX_CODEX_PAGE_UNITS || next_bytes > MAX_CODEX_PAGE_BYTES {
                if page.has_progress() {
                    self.restore(position)?;
                    return self.emit_active_core_page().map(Some);
                }
                self.reject(false);
                projection = CodexRecordProjection::default();
            } else {
                let page = self
                    .active_core_page
                    .as_mut()
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex NativePath lost its active Core page",
                    ))?;
                page.serialized_bytes = next_bytes;
            }
            if let Some(mutation) = projection.context_mutation.take() {
                self.apply_context_mutation(mutation);
            }

            self.raw_ordinal = self.raw_ordinal.saturating_add(1);
            let page = self
                .active_core_page
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex NativePath lost its active Core page",
                ))?;
            page.physical_records = page.physical_records.saturating_add(1);
        }
    }
}

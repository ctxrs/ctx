use super::project::{mcp_terminal_candidate_evidence, CodexMcpTerminalAuthority};
use super::*;
use crate::provider::codex::nativepath::record::codex_record_class;

struct McpTerminalAuthorityPreflight {
    authority: CodexMcpTerminalAuthority,
    bytes_read: u64,
    peak_record_bytes: usize,
}

fn result_terminal_authority_is_ambiguous(record: &[u8]) -> bool {
    // Codex treats a NUL-prefixed suffix as framing corruption, not a JSON
    // record candidate. Preserve that dedicated append-boundary diagnosis.
    if record.first() == Some(&0) {
        return false;
    }
    !crate::common::json::raw_object_keys_are_unique(record)
}

fn observe_result_terminal_call_id(
    authority: &mut CodexMcpTerminalAuthority,
    record: &[u8],
    in_certified_prefix: bool,
) {
    if let Ok(probe) = classify_codex_record(record) {
        if matches!(probe.class, CodexRecordClass::ExcludedResult(_)) {
            if let Some(call_id) = probe
                .call_id
                .as_deref()
                .filter(|call_id| !call_id.is_empty())
            {
                authority.observe_result_call_id(call_id, in_certified_prefix);
                return;
            }
        }
    }

    // Projection can recover a bounded valid terminal after the strict
    // selector probe declines to expose linkage metadata. Observe that same
    // provider-recognized envelope here so uniqueness never depends on which
    // valid projection path retained the result.
    let Ok(envelope) = serde_json::from_slice::<Value>(record) else {
        return;
    };
    let Some(record_type) = envelope.get("type").and_then(Value::as_str) else {
        return;
    };
    let Some(payload) = envelope.get("payload") else {
        return;
    };
    let item_type = payload.get("type").and_then(Value::as_str);
    if !matches!(
        codex_record_class(record_type, item_type),
        CodexRecordClass::ExcludedResult(_)
    ) {
        return;
    }
    if let Some(call_id) = payload
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
    {
        authority.observe_result_call_id(call_id, in_certified_prefix);
    }
}

fn preflight_mcp_terminal_authority(
    opened: &OpenedProviderSourceFile,
    frozen_len: u64,
    certified_prefix_end: Option<u64>,
) -> Result<McpTerminalAuthorityPreflight> {
    let mut reader = BufReader::new(opened.file().try_clone()?);
    reader.seek(SeekFrom::Start(0))?;
    let mut offset = 0_u64;
    let mut record_buffer = Vec::new();
    let mut full_hasher = Sha256::new();
    let mut complete_hasher = Sha256::new();
    let mut authority = CodexMcpTerminalAuthority::default();
    let mut peak_record_bytes = 0_usize;
    while offset < frozen_len {
        let Some(record_read) = read_bounded_record(
            &mut reader,
            &mut record_buffer,
            &mut full_hasher,
            &mut complete_hasher,
            frozen_len.saturating_sub(offset),
        )?
        else {
            break;
        };
        offset = offset
            .checked_add(record_read.byte_len)
            .ok_or(CaptureError::SystemInvariant(
                "Codex authority preflight offset exceeds u64",
            ))?;
        peak_record_bytes = peak_record_bytes.max(record_read.stored_len);
        if !record_read.complete {
            break;
        }
        if record_read.terminal_nul_padding {
            continue;
        }
        if record_read.oversized {
            authority.observe_ambiguous_result_terminal();
            continue;
        }
        let record = trim_jsonl_terminator(&record_buffer[..record_read.stored_len]);
        let in_certified_prefix =
            certified_prefix_end.is_some_and(|prefix_end| offset <= prefix_end);
        if result_terminal_authority_is_ambiguous(record) {
            authority.observe_ambiguous_result_terminal();
        }
        if let Some(evidence) = mcp_terminal_candidate_evidence(record) {
            authority.observe(&evidence, in_certified_prefix);
        }
        observe_result_terminal_call_id(&mut authority, record, in_certified_prefix);
    }
    Ok(McpTerminalAuthorityPreflight {
        authority,
        bytes_read: offset,
        peak_record_bytes,
    })
}

impl CodexNativeScanner {
    #[cfg(test)]
    pub(super) fn new(
        source: CodexCatalogSource,
        proof: Option<&CodexAppendProof>,
    ) -> Result<Self> {
        Self::new_with_lineage(source, proof, None)
    }

    pub(super) fn new_with_lineage(
        source: CodexCatalogSource,
        proof: Option<&CodexAppendProof>,
        lineage_facts: Option<CodexLineageFactsV0>,
    ) -> Result<Self> {
        let opened = open_codex_source_capability(&source)?;
        Self::new_retained(source, opened, proof, lineage_facts)
    }

    pub(super) fn new_retained(
        mut source: CodexCatalogSource,
        opened: Arc<OpenedProviderSourceFile>,
        proof: Option<&CodexAppendProof>,
        mut lineage_facts: Option<CodexLineageFactsV0>,
    ) -> Result<Self> {
        source.opened = Some(Arc::clone(&opened));
        if let Some(proof) = proof {
            proof.validate_source(&source)?;
            validate_checkpoint_catalog_owner(&source, proof.checkpoint.owner.clone())?;
        }

        let before = observed_opened_file(&source, &opened)?;
        source.catalog_observation = before.clone();
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
                lineage_facts.as_mut(),
            )?)
        } else {
            None
        };

        if let (Some(proof), Some(validated)) = (
            proof.filter(|proof| proof.checkpoint.observation == before),
            validated.as_ref(),
        ) {
            let replay_owner =
                validate_checkpoint_catalog_owner(&source, proof.checkpoint.owner.clone())?;
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
                owner: Some(replay_owner),
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
                lineage_facts: None,
            };
            return Ok(Self {
                source,
                opened,
                frozen_len: before.len,
                before,
                reader,
                disposition: CodexParseDisposition::ObservationReplay,
                offset: replay.complete_prefix_end,
                raw_ordinal: replay.next_raw_ordinal,
                owner: replay.owner.clone(),
                tool_contexts: BTreeMap::new(),
                tool_authorities: BTreeMap::new(),
                continuations: BTreeMap::new(),
                mcp_terminal_authority: CodexMcpTerminalAuthority::default(),
                complete_hasher: Sha256::new(),
                full_hasher: Sha256::new(),
                record_buffer: Vec::new(),
                incomplete_tail: None,
                counters: replay.counters,
                lineage_facts,
                replay: Some(replay),
                active_core_page: None,
                ready_core_page: None,
                exhausted: true,
            });
        }

        let certified_prefix_end = proof.map(|proof| proof.checkpoint.complete_prefix_end());
        let authority_preflight =
            preflight_mcp_terminal_authority(&opened, before.len, certified_prefix_end)?;
        if proof.is_some()
            && before.len
                > proof
                    .map(|proof| proof.checkpoint.observation.len)
                    .unwrap_or_default()
            && authority_preflight.authority.append_requires_replacement()
        {
            return Err(invalid_checkpoint_proof(
                "an appended terminal reuses a certified native call ID",
            ));
        }
        let authority_entries = authority_preflight.authority.entry_count();
        let authority_bytes = authority_preflight.authority.estimated_owned_bytes();

        let (
            disposition,
            owner,
            tool_contexts,
            tool_authorities,
            continuations,
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
                    pending_continuations: continuations,
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
                    continuations,
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
            frozen_len: before.len,
            before,
            reader,
            disposition,
            offset,
            raw_ordinal,
            owner,
            tool_contexts,
            tool_authorities,
            continuations,
            mcp_terminal_authority: authority_preflight.authority,
            complete_hasher: complete_hasher.clone(),
            full_hasher: complete_hasher,
            record_buffer: Vec::new(),
            incomplete_tail: None,
            counters: CodexScanCounters {
                bytes_read: validation_bytes,
                checkpoint_validation_bytes: validation_bytes,
                prefix_bytes_read: offset,
                mcp_terminal_authority_bytes_read: authority_preflight.bytes_read,
                peak_mcp_terminal_authority_entries: authority_entries,
                peak_mcp_terminal_authority_bytes: authority_bytes,
                peak_line_buffer_bytes: authority_preflight.peak_record_bytes,
                ..CodexScanCounters::default()
            },
            lineage_facts,
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
                    || page.serialized_bytes > MAX_CODEX_PAGE_BYTES
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
                read_bounded_record(
                    reader,
                    record_buffer,
                    full_hasher,
                    complete_hasher,
                    self.frozen_len.saturating_sub(self.offset),
                )?
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
                if let Some(lineage_facts) = self.lineage_facts.as_mut() {
                    lineage_facts.record(CodexLineageRecordEvidence::UnattributedAmbiguity)?;
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
                if let Some(lineage_facts) = self.lineage_facts.as_mut() {
                    lineage_facts.record(CodexLineageRecordEvidence::UnattributedAmbiguity)?;
                }
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
            let next_byte_limit = if page.units() == 0 && projection.core_units() == 1 {
                MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
            } else {
                MAX_CODEX_PAGE_BYTES
            };
            if next_units > MAX_CODEX_PAGE_UNITS || next_bytes > next_byte_limit {
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

#[cfg(test)]
mod terminal_authority_tests {
    use super::result_terminal_authority_is_ambiguous;

    #[test]
    fn duplicate_selector_cannot_hide_terminal_authority() {
        assert!(result_terminal_authority_is_ambiguous(
            br#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"call","output":"hidden"},"payload":{"type":"message","role":"user","content":[]}}"#,
        ));
        assert!(!result_terminal_authority_is_ambiguous(
            br#"{"type":"response_item","payload":{"type":"message","role":"user","content":[]}}"#,
        ));
    }
}

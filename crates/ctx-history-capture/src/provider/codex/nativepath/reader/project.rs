use super::*;

impl CodexNativeScanner {
    pub(super) fn process_record(
        &mut self,
        record: &[u8],
        start_byte: u64,
        end_byte: u64,
        record_digest: [u8; 32],
    ) -> Result<CodexRecordProjection> {
        let record = trim_jsonl_terminator(record);
        if record.iter().all(u8::is_ascii_whitespace) {
            self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
            return Ok(CodexRecordProjection::default());
        }

        // Records Core never materializes are the bulk of a Codex rollout. The
        // prefilter answers from the raw bytes, so they never reach a parse,
        // an allocation, or a payload hash.
        if let CodexRecordAdmission::NoProjection(projection) = prefilter_codex_record(record) {
            self.counters.prefiltered_records = self.counters.prefiltered_records.saturating_add(1);
            self.project_without_parse(projection, start_byte, end_byte);
            return Ok(CodexRecordProjection::default());
        }

        self.counters.structural_json_parses =
            self.counters.structural_json_parses.saturating_add(1);
        let probe = match classify_codex_record(record) {
            Ok(probe) => probe,
            Err(_) => {
                self.reject(false);
                return Ok(CodexRecordProjection::default());
            }
        };
        match probe.class {
            CodexRecordClass::SessionMeta => {
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                match parse_session_meta(record) {
                    Some(owner) if self.owner.is_none() => {
                        self.owner = Some(owner);
                        return Ok(CodexRecordProjection::default());
                    }
                    Some(_) => {
                        self.counters.ignored_records =
                            self.counters.ignored_records.saturating_add(1);
                    }
                    None => self.reject(false),
                }
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::Ignored => {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::Retained(kind) => {
                let Some(owner) = self.owner.as_ref() else {
                    self.reject(false);
                    return Ok(CodexRecordProjection::default());
                };
                self.counters.retained_json_parses =
                    self.counters.retained_json_parses.saturating_add(1);
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                let Some(retained) = parse_decoded_record(record, owner) else {
                    self.reject(false);
                    return Ok(CodexRecordProjection::default());
                };
                let mut built = match build_source_backed_event_row(
                    self.raw_ordinal,
                    kind,
                    &retained,
                    start_byte,
                    end_byte,
                    record_digest,
                )? {
                    Ok(built) => built,
                    Err(CodexRetainedNonMaterialized::ValidUnmaterializable) => {
                        self.counters.ignored_records =
                            self.counters.ignored_records.saturating_add(1);
                        return Ok(CodexRecordProjection::default());
                    }
                    Err(CodexRetainedNonMaterialized::Malformed) => {
                        self.reject(false);
                        return Ok(CodexRecordProjection::default());
                    }
                };
                let touch_outcome = visit_provider_file_touch_drafts_with_limit(
                    &retained.payload,
                    event_type_supports_structured_file_touches(built.row.event_type),
                    MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
                    |(_, touch)| {
                        built.row.touched_paths.push(touch.path);
                        Ok::<(), CaptureError>(())
                    },
                )?;
                if touch_outcome.limit_exceeded() {
                    self.reject(false);
                    return Ok(CodexRecordProjection::default());
                }
                let row_bytes = built.row.estimated_owned_bytes().unwrap_or(usize::MAX);
                if row_bytes > MAX_CODEX_PAGE_BYTES.saturating_sub(PAGE_FIXED_WIRE_BYTES) {
                    self.reject(false);
                    return Ok(CodexRecordProjection::default());
                }
                let lexical_bytes = built.row.lexical_body.len();
                self.counters.retained_records = self.counters.retained_records.saturating_add(1);
                self.counters.retained_body_bytes = self
                    .counters
                    .retained_body_bytes
                    .saturating_add(u64::try_from(lexical_bytes).unwrap_or(u64::MAX));
                let insert_context = built.tool_context.map(|(call_id, context)| {
                    let authority = CodexPendingToolAuthority::new(
                        &call_id,
                        start_byte,
                        end_byte,
                        self.raw_ordinal,
                    );
                    (call_id, context, authority)
                });
                Ok(CodexRecordProjection {
                    context_mutation: Some(CodexContextMutation::SourceBackedRow {
                        row: built.row,
                        insert_context,
                        remove_context: None,
                    }),
                    source_backed_units: 1,
                    core_serialized_bytes: row_bytes,
                })
            }
            CodexRecordClass::ExcludedResult(result_kind) => {
                self.process_output(&probe, result_kind, start_byte, end_byte, record_digest)
            }
        }
    }

    /// Applies the counter-only projection the prefilter proved sufficient.
    ///
    /// Each arm mirrors the corresponding arm of the parsed path exactly: the
    /// ignored-record counter, or the two native-result counters that
    /// [`Self::process_output`] advances before it consults the probe.
    fn project_without_parse(
        &mut self,
        projection: CodexSkipProjection,
        start_byte: u64,
        end_byte: u64,
    ) {
        match projection {
            CodexSkipProjection::Ignored => {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
            }
            CodexSkipProjection::NativeResult => {
                self.counters.native_result_records =
                    self.counters.native_result_records.saturating_add(1);
                self.counters.native_result_record_bytes = self
                    .counters
                    .native_result_record_bytes
                    .saturating_add(end_byte.saturating_sub(start_byte));
            }
        }
    }

    pub(super) fn process_output(
        &mut self,
        probe: &CodexRecordProbe<'_>,
        result_kind: CodexResultKind,
        start_byte: u64,
        end_byte: u64,
        record_digest: [u8; 32],
    ) -> Result<CodexRecordProjection> {
        self.counters.native_result_records = self.counters.native_result_records.saturating_add(1);
        self.counters.native_result_record_bytes = self
            .counters
            .native_result_record_bytes
            .saturating_add(end_byte.saturating_sub(start_byte));

        if !result_kind.is_eligible_output() {
            return Ok(CodexRecordProjection::default());
        }

        self.counters.structural_output_probes =
            self.counters.structural_output_probes.saturating_add(1);
        let Some(structural) = probe.output.as_ref() else {
            return Err(CaptureError::SystemInvariant(
                "eligible Codex output is missing its structural outcome probe",
            ));
        };
        let sparse_core_diagnostic = matches!(
            structural.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        );
        let Some(owner) = self.owner.clone() else {
            self.reject(false);
            return Ok(CodexRecordProjection::default());
        };
        let Some(occurred_at) = probe_timestamp(probe, owner.started_at) else {
            self.reject(false);
            return Ok(CodexRecordProjection::default());
        };

        let call_id = probe.call_id.as_deref();
        let context = call_id
            .and_then(|call_id| self.tool_contexts.get(call_id))
            .cloned();

        if !sparse_core_diagnostic {
            // Structural admission is complete and successful/unknown output
            // bodies have no Core projection. Retire the context without
            // hydrating canonical output or allocating a removal key.
            if let Some(call_id) = probe.call_id.as_deref() {
                self.tool_contexts.remove(call_id);
                self.tool_authorities.remove(call_id);
            }
            return Ok(CodexRecordProjection::default());
        }

        match source_backed_output_eligibility(result_kind, structural) {
            CodexSourceBackedDocumentEligibility::Eligible(()) => {}
            CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay => {
                if let Some(call_id) = call_id {
                    self.tool_contexts.remove(call_id);
                    self.tool_authorities.remove(call_id);
                }
                return Ok(CodexRecordProjection::default());
            }
            CodexSourceBackedDocumentEligibility::ParserRevisionGap => {
                return Err(CaptureError::SystemInvariant(
                    "Codex output eligibility has an unsupported parser revision",
                ));
            }
        }
        let core_row = build_source_backed_sparse_output_row(
            self.raw_ordinal,
            start_byte,
            end_byte,
            record_digest,
            occurred_at,
            result_kind,
            context.as_ref(),
            &structural.outcome,
        )?;
        let context_mutation = match core_row {
            Some(row) => {
                let row_bytes = row.estimated_owned_bytes().unwrap_or(usize::MAX);
                if row_bytes > MAX_CODEX_PAGE_BYTES.saturating_sub(PAGE_FIXED_WIRE_BYTES) {
                    self.reject(false);
                    return Ok(CodexRecordProjection::default());
                }
                self.counters.retained_records = self.counters.retained_records.saturating_add(1);
                self.counters.retained_body_bytes = self
                    .counters
                    .retained_body_bytes
                    .saturating_add(u64::try_from(row.lexical_body.len()).unwrap_or(u64::MAX));
                return Ok(CodexRecordProjection {
                    context_mutation: Some(CodexContextMutation::SourceBackedRow {
                        row,
                        insert_context: None,
                        remove_context: call_id.map(str::to_owned),
                    }),
                    source_backed_units: 1,
                    core_serialized_bytes: row_bytes,
                });
            }
            None => call_id.map(|call_id| CodexContextMutation::Remove(call_id.to_owned())),
        };
        Ok(CodexRecordProjection {
            context_mutation,
            source_backed_units: 0,
            core_serialized_bytes: 0,
        })
    }

    pub(super) fn apply_context_mutation(&mut self, mutation: CodexContextMutation) {
        match mutation {
            CodexContextMutation::Remove(call_id) => {
                self.tool_contexts.remove(&call_id);
                self.tool_authorities.remove(&call_id);
            }
            CodexContextMutation::SourceBackedRow {
                row,
                insert_context,
                remove_context,
            } => {
                if let Some(call_id) = remove_context {
                    self.tool_contexts.remove(&call_id);
                    self.tool_authorities.remove(&call_id);
                }
                if let Some((call_id, mut context, authority)) = insert_context {
                    if call_id.len() <= MAX_CODEX_TOOL_CALL_ID_BYTES {
                        context = bound_tool_context(context);
                        self.tool_authorities.insert(call_id.clone(), authority);
                        self.tool_contexts.insert(call_id, context);
                        while self.tool_contexts.len() > MAX_CODEX_TOOL_CONTEXTS {
                            let Some(oldest) = self.tool_contexts.keys().next().cloned() else {
                                break;
                            };
                            self.tool_contexts.remove(&oldest);
                            self.tool_authorities.remove(&oldest);
                        }
                    }
                }
                debug_assert!(self.active_core_page.is_some());
                if let Some(page) = self.active_core_page.as_mut() {
                    page.source_backed_rows.push(row);
                }
            }
        }
    }

    pub(super) fn reject(&mut self, oversized: bool) {
        if oversized {
            self.counters.oversized_records = self.counters.oversized_records.saturating_add(1);
        } else {
            self.counters.malformed_records = self.counters.malformed_records.saturating_add(1);
        }
        self.counters.rejected_complete_records =
            self.counters.rejected_complete_records.saturating_add(1);
    }
}

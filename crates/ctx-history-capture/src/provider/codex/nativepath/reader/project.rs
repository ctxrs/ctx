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
            CodexRecordClass::TurnContext => {
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                match (self.owner.as_mut(), parse_turn_context_cwd(record)) {
                    (Some(owner), Some(cwd)) => owner.cwd = Some(cwd),
                    (None, _) | (_, None) => self.reject(false),
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
                let mut built =
                    match build_source_backed_event_row(self.raw_ordinal, kind, &retained)? {
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
                built.row.session_cwd.clone_from(&owner.cwd);
                let touch_outcome = visit_provider_file_touch_drafts_with_limit(
                    &retained.payload,
                    event_type_supports_structured_file_touches(built.row.event_type),
                    MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
                    |(_, touch)| {
                        built.row.touched_paths.push(touch.path.clone());
                        built.row.repository_files.push(
                            crate::repository_attribution::UnscopedFileObservation {
                                path: touch.path,
                                prior_path: touch.old_path,
                                kind:
                                    crate::provider::codex::nativepath::rows::repository_file_kind(
                                        touch.change_kind,
                                    ),
                            },
                        );
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
                let insert_context = built.tool_context.map(|(call_id, mut context)| {
                    context.session_cwd = owner.cwd.clone();
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
                        remove_contexts: Vec::new(),
                    }),
                    source_backed_units: 1,
                    core_serialized_bytes: row_bytes,
                })
            }
            CodexRecordClass::ExcludedResult(result_kind) => self.process_output(
                record,
                &probe,
                result_kind,
                start_byte,
                end_byte,
                record_digest,
            ),
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
        record: &[u8],
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
        let needs_repository_payload = context.as_ref().is_some_and(|context| {
            context.continuation_cell_id.is_some()
                || context
                    .exact_command
                    .as_deref()
                    .is_some_and(crate::repository_attribution::bounded_outcome_evidence_relevant)
        });
        let decoded = if needs_repository_payload || sparse_core_diagnostic {
            self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
            parse_decoded_record(record, &owner)
        } else {
            None
        };

        if let (Some(call_id), Some(context), Some(decoded)) =
            (call_id, context.as_ref(), decoded.as_ref())
        {
            if let Some(cell_id) =
                crate::provider::codex::repository::running_continuation_cell_id(&decoded.payload)
            {
                if context
                    .continuation_cell_id
                    .as_deref()
                    .is_none_or(|expected| expected == cell_id)
                {
                    if context.continuation_cell_id.is_some() {
                        return Ok(CodexRecordProjection {
                            context_mutation: Some(CodexContextMutation::Remove(vec![
                                call_id.to_owned()
                            ])),
                            ..CodexRecordProjection::default()
                        });
                    }
                    return Ok(CodexRecordProjection {
                        context_mutation: Some(CodexContextMutation::RegisterContinuation {
                            cell_id,
                            origin_call_id: call_id.to_owned(),
                        }),
                        ..CodexRecordProjection::default()
                    });
                }
            }
        }

        let repository_result = decoded.as_ref().and_then(|decoded| {
            crate::provider::codex::repository::repository_result_evidence(
                &decoded.payload,
                context.as_ref()?,
                call_id?,
                record_digest,
                occurred_at.timestamp_millis(),
                &structural.outcome,
            )
        });

        if sparse_core_diagnostic {
            match source_backed_output_eligibility(result_kind, structural) {
                CodexSourceBackedDocumentEligibility::Eligible(()) => {}
                CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay => {
                    return Ok(CodexRecordProjection {
                        context_mutation: call_id.map(|call_id| {
                            CodexContextMutation::Remove(linked_call_ids(call_id, context.as_ref()))
                        }),
                        ..CodexRecordProjection::default()
                    });
                }
                CodexSourceBackedDocumentEligibility::ParserRevisionGap => {
                    return Err(CaptureError::SystemInvariant(
                        "Codex output eligibility has an unsupported parser revision",
                    ));
                }
            }
        } else if repository_result.is_none() {
            return Ok(CodexRecordProjection {
                context_mutation: call_id.map(|call_id| {
                    CodexContextMutation::Remove(linked_call_ids(call_id, context.as_ref()))
                }),
                ..CodexRecordProjection::default()
            });
        }
        let normalized_body = if sparse_core_diagnostic {
            let decoded = decoded.as_ref().ok_or(CaptureError::SystemInvariant(
                "Codex diagnostic output could not be decoded for direct Core publication",
            ))?;
            match source_backed_display_text(probe, &decoded.payload) {
                CodexSourceBackedDocumentEligibility::Eligible(body) => body,
                CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay => {
                    return Err(CaptureError::SystemInvariant(
                        "Codex diagnostic output lost its selected Core body",
                    ));
                }
                CodexSourceBackedDocumentEligibility::ParserRevisionGap => {
                    return Err(CaptureError::SystemInvariant(
                        "Codex diagnostic output has an unsupported Core body shape",
                    ));
                }
            }
        } else {
            String::new()
        };
        let core_row = build_source_backed_sparse_output_row(
            self.raw_ordinal,
            occurred_at,
            result_kind,
            context.as_ref(),
            &structural.outcome,
            normalized_body,
            repository_result,
            context
                .as_ref()
                .and_then(|context| context.session_cwd.clone())
                .or_else(|| owner.cwd.clone()),
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
                        remove_contexts: call_id
                            .map(|call_id| linked_call_ids(call_id, context.as_ref()))
                            .unwrap_or_default(),
                    }),
                    source_backed_units: 1,
                    core_serialized_bytes: row_bytes,
                });
            }
            None => call_id.map(|call_id| {
                CodexContextMutation::Remove(linked_call_ids(call_id, context.as_ref()))
            }),
        };
        Ok(CodexRecordProjection {
            context_mutation,
            source_backed_units: 0,
            core_serialized_bytes: 0,
        })
    }

    pub(super) fn apply_context_mutation(&mut self, mutation: CodexContextMutation) {
        match mutation {
            CodexContextMutation::Remove(call_ids) => {
                for call_id in call_ids {
                    self.remove_tool_context(&call_id);
                }
            }
            CodexContextMutation::RegisterContinuation {
                cell_id,
                origin_call_id,
            } => match self.continuations.get(&cell_id).cloned() {
                Some(existing) if existing != origin_call_id => {
                    let conflicted_origins = self
                        .tool_authorities
                        .iter()
                        .filter_map(|(call_id, authority)| {
                            (authority.continuation_cell_id() == Some(cell_id.as_str()))
                                .then_some(call_id.clone())
                        })
                        .collect::<Vec<_>>();
                    for call_id in conflicted_origins {
                        if let Some(context) = self.tool_contexts.get_mut(&call_id) {
                            context.correlation_ambiguous = true;
                        }
                        if let Some(authority) = self.tool_authorities.get_mut(&call_id) {
                            authority.mark_correlation_ambiguous();
                            authority.clear_continuation();
                        }
                    }
                    if let Some(context) = self.tool_contexts.get_mut(&origin_call_id) {
                        context.correlation_ambiguous = true;
                    }
                    if let Some(authority) = self.tool_authorities.get_mut(&origin_call_id) {
                        authority.mark_correlation_ambiguous();
                        authority.mark_continuation_conflict(&cell_id);
                    }
                    self.continuations.insert(cell_id, String::new());
                }
                _ => {
                    if self.tool_contexts.contains_key(&origin_call_id)
                        && self
                            .tool_authorities
                            .get_mut(&origin_call_id)
                            .is_some_and(|authority| authority.assign_continuation(&cell_id))
                    {
                        self.continuations.insert(cell_id, origin_call_id);
                    }
                }
            },
            CodexContextMutation::SourceBackedRow {
                row,
                insert_context,
                remove_contexts,
            } => {
                for call_id in remove_contexts {
                    self.remove_tool_context(&call_id);
                }
                if let Some((call_id, mut context, authority)) = insert_context {
                    if call_id.len() <= MAX_CODEX_TOOL_CALL_ID_BYTES {
                        if self.tool_contexts.contains_key(&call_id)
                            || self.tool_authorities.contains_key(&call_id)
                        {
                            if let Some(existing) = self.tool_contexts.get_mut(&call_id) {
                                existing.correlation_ambiguous = true;
                            }
                            if let Some(existing) = self.tool_authorities.get_mut(&call_id) {
                                existing.mark_correlation_ambiguous();
                            }
                        } else {
                            self.link_continuation_context(&call_id, &mut context);
                            context = bound_tool_context(context);
                            self.tool_authorities.insert(call_id.clone(), authority);
                            self.tool_contexts.insert(call_id, context);
                        }
                        while self.tool_contexts.len() > MAX_CODEX_TOOL_CONTEXTS {
                            let Some(oldest) = self
                                .tool_authorities
                                .iter()
                                .min_by_key(|(_, authority)| authority.raw_ordinal)
                                .map(|(call_id, _)| call_id.clone())
                            else {
                                break;
                            };
                            self.remove_tool_context(&oldest);
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

    fn link_continuation_context(&mut self, call_id: &str, context: &mut CodexToolCallContext) {
        let Some(cell_id) = context.continuation_cell_id.as_deref() else {
            return;
        };
        let Some(origin_call_id) = self.continuations.get(cell_id).cloned() else {
            return;
        };
        let overlapping_waits = self
            .tool_contexts
            .iter()
            .filter_map(|(active_call_id, active)| {
                (active_call_id != call_id
                    && active.continuation_cell_id.as_deref() == Some(cell_id)
                    && active.origin_call_id.as_deref() == Some(origin_call_id.as_str()))
                .then_some(active_call_id.clone())
            })
            .collect::<Vec<_>>();
        if !overlapping_waits.is_empty() {
            for active_call_id in &overlapping_waits {
                if let Some(active) = self.tool_contexts.get_mut(active_call_id) {
                    active.correlation_ambiguous = true;
                }
                if let Some(authority) = self.tool_authorities.get_mut(active_call_id) {
                    authority.mark_correlation_ambiguous();
                }
            }
            if let Some(origin) = self.tool_contexts.get_mut(&origin_call_id) {
                origin.correlation_ambiguous = true;
            }
            if let Some(authority) = self.tool_authorities.get_mut(&origin_call_id) {
                authority.mark_correlation_ambiguous();
            }
        }
        let Some(origin) = self.tool_contexts.get_mut(&origin_call_id) else {
            return;
        };
        let digest = crate::provider::codex::repository::continuation_call_id_sha256(call_id);
        if origin.continuation_call_id_sha256.contains(&digest) {
            origin.correlation_ambiguous = true;
        } else if origin.continuation_call_id_sha256.len() >= MAX_CODEX_TOOL_CONTEXTS {
            origin.continuation_capacity_exceeded = true;
        } else {
            origin.continuation_call_id_sha256.push(digest);
        }
        if let Some(authority) = self.tool_authorities.get_mut(&origin_call_id) {
            if origin.correlation_ambiguous {
                authority.mark_correlation_ambiguous();
            }
            authority.record_continuation_call(digest);
        }
        context.exact_command.clone_from(&origin.exact_command);
        context.session_cwd.clone_from(&origin.session_cwd);
        context
            .declared_workdir
            .clone_from(&origin.declared_workdir);
        context.origin_call_id = Some(origin_call_id);
        context.origin_event_sequence = origin.origin_event_sequence;
        context
            .continuation_call_id_sha256
            .clone_from(&origin.continuation_call_id_sha256);
        context.continuation_capacity_exceeded = origin.continuation_capacity_exceeded;
        context.correlation_ambiguous = origin.correlation_ambiguous;
    }

    fn remove_tool_context(&mut self, call_id: &str) {
        let conflicted_cell = self
            .tool_authorities
            .get(call_id)
            .filter(|authority| authority.continuation_conflicted())
            .and_then(CodexPendingToolAuthority::continuation_cell_id)
            .map(str::to_owned);
        self.tool_contexts.remove(call_id);
        self.tool_authorities.remove(call_id);
        if let Some(cell_id) = conflicted_cell {
            self.continuations.remove(&cell_id);
        }
        self.continuations.retain(|_, origin| origin != call_id);
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

fn linked_call_ids(call_id: &str, context: Option<&CodexToolCallContext>) -> Vec<String> {
    let mut call_ids = vec![call_id.to_owned()];
    if context
        .and_then(|context| context.continuation_cell_id.as_ref())
        .is_some()
    {
        if let Some(origin_call_id) = context.and_then(|context| context.origin_call_id.as_deref())
        {
            if origin_call_id != call_id {
                call_ids.push(origin_call_id.to_owned());
            }
        }
    }
    call_ids
}

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
                if malformed_record_may_contain_lineage(record) {
                    if let Some(lineage_facts) = self.lineage_facts.as_mut() {
                        lineage_facts.record(CodexLineageRecordEvidence::UnattributedAmbiguity)?;
                    }
                }
                return Ok(CodexRecordProjection::default());
            }
        };
        let lineage_evidence = codex_lineage_record_evidence(&probe);
        if let Some(lineage_facts) = self.lineage_facts.as_mut() {
            lineage_facts.record(lineage_evidence)?;
        }
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
                if row_bytes
                    > MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
                        .saturating_sub(PAGE_FIXED_WIRE_BYTES)
                {
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
    /// The arm mirrors the ignored-record counter in the parsed path exactly.
    fn project_without_parse(
        &mut self,
        projection: CodexSkipProjection,
        _start_byte: u64,
        _end_byte: u64,
    ) {
        match projection {
            CodexSkipProjection::Ignored => {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
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

        self.counters.structural_output_probes =
            self.counters.structural_output_probes.saturating_add(1);
        let Some(structural) = probe.output.as_ref() else {
            return Err(CaptureError::SystemInvariant(
                "eligible Codex output is missing its structural outcome probe",
            ));
        };
        let call_id = probe.call_id.as_deref();
        let context = call_id
            .and_then(|call_id| self.tool_contexts.get(call_id))
            .cloned();
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
        let Some(owner) = self.owner.clone() else {
            self.reject(false);
            return Ok(CodexRecordProjection::default());
        };
        let Some(occurred_at) = probe_timestamp(probe, owner.started_at) else {
            self.reject(false);
            return Ok(CodexRecordProjection::default());
        };

        self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
        let decoded = parse_decoded_record(record, &owner);
        let decoded = decoded.as_ref().ok_or(CaptureError::SystemInvariant(
            "Codex output could not be decoded for complete Core publication",
        ))?;
        let projected_output = match project_codex_output(probe, &decoded.payload) {
            Ok(Some(projected)) => projected,
            Ok(None) => {
                return Err(CaptureError::SystemInvariant(
                    "Codex output has an unsupported Core body shape",
                ));
            }
            Err(()) => {
                self.reject(false);
                return Ok(CodexRecordProjection {
                    context_mutation: call_id.map(|call_id| {
                        CodexContextMutation::Remove(linked_call_ids(call_id, context.as_ref()))
                    }),
                    ..CodexRecordProjection::default()
                });
            }
        };

        if let (Some(call_id), Some(context)) = (call_id, context.as_ref()) {
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

        let repository_result = context.as_ref().and_then(|context| {
            crate::provider::codex::repository::repository_result_evidence(
                &decoded.payload,
                context,
                call_id?,
                record_digest,
                occurred_at.timestamp_millis(),
                &structural.outcome,
            )
        });

        let structured_content = Some(projected_tool_result_content(
            result_kind,
            call_id,
            &projected_output,
        ));
        let core_row = build_source_backed_sparse_output_row(
            self.raw_ordinal,
            provider_event_identity(&decoded.payload),
            occurred_at,
            result_kind,
            context.as_ref(),
            &structural.outcome,
            projected_output.normalized_body,
            structured_content,
            repository_result,
            context
                .as_ref()
                .and_then(|context| context.session_cwd.clone())
                .or_else(|| owner.cwd.clone()),
        )?;
        let context_mutation = match core_row {
            Some(row) => {
                let row_bytes = row.estimated_owned_bytes().unwrap_or(usize::MAX);
                if row_bytes
                    > MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
                        .saturating_sub(PAGE_FIXED_WIRE_BYTES)
                {
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
        context.command_too_large = origin.command_too_large;
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

struct ProjectedCodexOutput {
    normalized_body: String,
    result_variant: Option<&'static str>,
    result_metadata: Option<Value>,
    invocation: Option<Value>,
    duration: Option<Value>,
}

fn project_codex_output(
    probe: &CodexRecordProbe<'_>,
    payload: &Value,
) -> std::result::Result<Option<ProjectedCodexOutput>, ()> {
    if payload.get("type").and_then(Value::as_str) == Some("mcp_tool_call_end") {
        return project_mcp_tool_call_end(payload).map(Some);
    }
    let Some(result) = codex_result_value(payload) else {
        return Ok(None);
    };
    let projected = codex_output_content(result);
    let normalized_body = match source_backed_display_text(probe, payload) {
        CodexSourceBackedDocumentEligibility::Eligible(body) => body,
        CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay
        | CodexSourceBackedDocumentEligibility::ParserRevisionGap => return Ok(None),
    };
    Ok(Some(ProjectedCodexOutput {
        normalized_body,
        result_variant: None,
        result_metadata: projected.metadata,
        invocation: None,
        duration: None,
    }))
}

fn project_mcp_tool_call_end(payload: &Value) -> std::result::Result<ProjectedCodexOutput, ()> {
    payload
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty() && call_id.len() <= MAX_CODEX_TOOL_CALL_ID_BYTES)
        .ok_or(())?;
    let invocation = payload
        .get("invocation")
        .and_then(Value::as_object)
        .ok_or(())?;
    invocation
        .get("server")
        .and_then(Value::as_str)
        .filter(|server| !server.is_empty())
        .ok_or(())?;
    invocation
        .get("tool")
        .and_then(Value::as_str)
        .filter(|tool| !tool.is_empty())
        .ok_or(())?;
    invocation.get("arguments").ok_or(())?;

    let duration = payload
        .get("duration")
        .and_then(Value::as_object)
        .ok_or(())?;
    duration.get("secs").and_then(Value::as_u64).ok_or(())?;
    duration
        .get("nanos")
        .and_then(Value::as_u64)
        .filter(|nanos| *nanos < 1_000_000_000)
        .ok_or(())?;

    if payload.get("output").is_some() || payload.get("tools").is_some() {
        return Err(());
    }
    let result = payload.get("result").and_then(Value::as_object).ok_or(())?;
    if result.len() != 1 {
        return Err(());
    }
    let (result_variant, result_value) = result.iter().next().ok_or(())?;
    let result_variant = match result_variant.as_str() {
        "Ok" => {
            validate_mcp_call_tool_result(result_value)?;
            "Ok"
        }
        "Err" => {
            result_value
                .as_str()
                .filter(|message| !message.trim().is_empty())
                .ok_or(())?;
            "Err"
        }
        _ => return Err(()),
    };
    let projected = codex_output_content(result_value);
    Ok(ProjectedCodexOutput {
        normalized_body: projected.text.into_owned(),
        result_variant: Some(result_variant),
        result_metadata: projected.metadata,
        invocation: Some(Value::Object(invocation.clone())),
        duration: Some(Value::Object(duration.clone())),
    })
}

fn validate_mcp_call_tool_result(result: &Value) -> std::result::Result<(), ()> {
    let result = result.as_object().ok_or(())?;
    let content = result.get("content").and_then(Value::as_array).ok_or(())?;
    if result
        .get("isError")
        .is_some_and(|is_error| !is_error.is_boolean())
        || result
            .get("_meta")
            .is_some_and(|metadata| !metadata.is_object())
    {
        return Err(());
    }
    for block in content {
        let block = block.as_object().ok_or(())?;
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .filter(|block_type| !block_type.is_empty())
            .ok_or(())?;
        match block_type {
            "text" => {
                block.get("text").and_then(Value::as_str).ok_or(())?;
            }
            "image" | "audio" => {
                block.get("data").and_then(Value::as_str).ok_or(())?;
                block.get("mimeType").and_then(Value::as_str).ok_or(())?;
            }
            "resource" => {
                block.get("resource").and_then(Value::as_object).ok_or(())?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn projected_tool_result_content(
    result_kind: CodexResultKind,
    call_id: Option<&str>,
    projected: &ProjectedCodexOutput,
) -> Value {
    let mut result = serde_json::Map::from_iter([
        (
            "item_type".to_owned(),
            Value::String(result_kind.item_type().to_owned()),
        ),
        (
            "call_id".to_owned(),
            call_id.map_or(Value::Null, |call_id| Value::String(call_id.to_owned())),
        ),
        (
            "result_content_location".to_owned(),
            Value::String("normalized_body".to_owned()),
        ),
        ("result_content_complete".to_owned(), Value::Bool(true)),
    ]);
    if let Some(variant) = projected.result_variant {
        result.insert(
            "result_variant".to_owned(),
            Value::String(variant.to_owned()),
        );
    }
    if let Some(metadata) = projected.result_metadata.clone() {
        result.insert("result_metadata".to_owned(), metadata);
    }
    if let Some(invocation) = projected.invocation.clone() {
        result.insert("invocation".to_owned(), invocation);
    }
    if let Some(duration) = projected.duration.clone() {
        result.insert("duration".to_owned(), duration);
    }
    serde_json::json!({
        "provider_native_tool_result": Value::Object(result),
    })
}

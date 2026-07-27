use super::*;

impl CodexNativeScanner {
    pub(super) fn process_record(
        &mut self,
        record: &[u8],
        start_byte: u64,
        end_byte: u64,
    ) -> Result<CodexRecordProjection> {
        let record = trim_jsonl_terminator(record);
        if record.iter().all(u8::is_ascii_whitespace) {
            self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
            return Ok(CodexRecordProjection::default());
        }

        self.counters.structural_json_parses =
            self.counters.structural_json_parses.saturating_add(1);
        let probe = match classify_codex_record(record) {
            Ok(probe) => probe,
            Err(_) => {
                self.reject(start_byte, end_byte, "malformed Codex JSON record", false);
                return Ok(CodexRecordProjection::default());
            }
        };
        match probe.class {
            CodexRecordClass::SessionMeta => {
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                match parse_session_meta(record) {
                    Some(owner) if self.owner.is_none() => {
                        let owner_bytes = serialized_owner_bytes(&owner)?;
                        if owner_bytes > MAX_CODEX_PAGE_BYTES.saturating_sub(PAGE_FIXED_WIRE_BYTES)
                        {
                            self.reject(
                                start_byte,
                                end_byte,
                                "Codex session metadata exceeds the bounded NativePath page",
                                false,
                            );
                            return Ok(CodexRecordProjection::default());
                        }
                        self.owner = Some(owner);
                        return Ok(CodexRecordProjection {
                            core_row: None,
                            pro_output: None,
                            context_mutation: None,
                            core_serialized_bytes: owner_bytes,
                            pro_serialized_bytes: 0,
                        });
                    }
                    Some(_) => {
                        self.counters.ignored_records =
                            self.counters.ignored_records.saturating_add(1);
                    }
                    None => self.reject(
                        start_byte,
                        end_byte,
                        "malformed Codex session metadata",
                        false,
                    ),
                }
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::Ignored => {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::Retained(kind) => {
                let Some(owner) = self.owner.as_ref() else {
                    self.reject(
                        start_byte,
                        end_byte,
                        "Codex retained record appeared before session metadata",
                        false,
                    );
                    return Ok(CodexRecordProjection::default());
                };
                self.counters.retained_json_parses =
                    self.counters.retained_json_parses.saturating_add(1);
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                let Some(retained) = parse_decoded_record(record, owner) else {
                    self.reject(
                        start_byte,
                        end_byte,
                        "malformed retained Codex record",
                        false,
                    );
                    return Ok(CodexRecordProjection::default());
                };
                let mut row = match build_event_row(self.raw_ordinal, kind, &retained)? {
                    Ok(row) => row,
                    Err(CodexRetainedNonMaterialized::ValidUnmaterializable) => {
                        self.counters.ignored_records =
                            self.counters.ignored_records.saturating_add(1);
                        return Ok(CodexRecordProjection::default());
                    }
                    Err(CodexRetainedNonMaterialized::Malformed(reason)) => {
                        self.reject(start_byte, end_byte, reason, false);
                        return Ok(CodexRecordProjection::default());
                    }
                };
                attach_complete_message_locator(&mut row, &retained, record, start_byte, end_byte)?;
                let raw_source_path = self.source.source_path.display().to_string();
                let line_number = usize::try_from(self.raw_ordinal)
                    .ok()
                    .and_then(|ordinal| ordinal.checked_add(1))
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex NativePath raw ordinal exceeds platform limits",
                    ))?;
                let provider_event_index = row.provider_event.provider_event_index;
                let occurred_at = row.provider_event.occurred_at;
                let touch_outcome = visit_provider_file_touch_drafts_with_limit(
                    &retained.payload,
                    event_type_supports_structured_file_touches(row.provider_event.event_type),
                    MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
                    |(touch_ordinal, touch)| {
                        let provider_touch_index =
                            if provider_event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                                touch_ordinal
                            } else {
                                ((line_number as u64) << 16) | touch_ordinal
                            };
                        row.file_touches.push(super::super::CodexFileTouch {
                            provider: ctx_history_core::CaptureProvider::Codex,
                            provider_session_id: owner.native_session_id.clone(),
                            provider_touch_index,
                            provider_event_index: Some(provider_event_index),
                            raw_source_path: Some(raw_source_path.clone()),
                            source_root: Some(self.source.source_root.clone()),
                            path: touch.path,
                            change_kind: touch.change_kind,
                            old_path: touch.old_path,
                            line_count_delta: None,
                            confidence: touch.confidence,
                            occurred_at,
                            source_format: crate::CODEX_SESSION_SOURCE_FORMAT.to_owned(),
                            metadata: touch.metadata,
                        });
                        Ok::<(), CaptureError>(())
                    },
                )?;
                if touch_outcome.limit_exceeded() {
                    self.reject(
                        start_byte,
                        end_byte,
                        PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
                        false,
                    );
                }
                let body_bytes = serde_json::to_vec(&row.provider_event.payload)?.len();
                let row_bytes = serde_json::to_vec(&row)?.len().saturating_add(1);
                self.counters.retained_records = self.counters.retained_records.saturating_add(1);
                self.counters.retained_body_bytes = self
                    .counters
                    .retained_body_bytes
                    .saturating_add(u64::try_from(body_bytes).unwrap_or(u64::MAX));
                self.counters.retained_hashes_created =
                    self.counters.retained_hashes_created.saturating_add(1);
                let context_mutation = tool_context_from_row(&row).map(|(call_id, context)| {
                    let authority = CodexPendingToolAuthority::new(
                        &call_id,
                        start_byte,
                        end_byte,
                        self.raw_ordinal,
                    );
                    CodexContextMutation::Insert(call_id, context, authority)
                });
                Ok(CodexRecordProjection {
                    core_row: Some(row),
                    pro_output: None,
                    context_mutation,
                    core_serialized_bytes: row_bytes,
                    pro_serialized_bytes: 0,
                })
            }
            CodexRecordClass::ExcludedResult(result_kind) => {
                self.process_output(record, &probe, result_kind, start_byte, end_byte)
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
            self.reject(
                start_byte,
                end_byte,
                "Codex output appeared before session metadata",
                false,
            );
            return Ok(CodexRecordProjection::default());
        };
        let Some(occurred_at) = probe_timestamp(probe, owner.started_at) else {
            self.reject(
                start_byte,
                end_byte,
                "Codex output timestamp is not valid RFC3339",
                false,
            );
            return Ok(CodexRecordProjection::default());
        };

        let call_id = probe.call_id.as_deref();
        let context = call_id
            .and_then(|call_id| self.tool_contexts.get(call_id))
            .cloned();

        if self.profile == CodexNativeProfile::CoreOnly && !sparse_core_diagnostic {
            // Structural admission is complete and successful/unknown output
            // bodies have no Core projection. Retire the context without
            // hydrating canonical output or allocating a removal key.
            if let Some(call_id) = probe.call_id.as_deref() {
                self.tool_contexts.remove(call_id);
                self.tool_authorities.remove(call_id);
            }
            return Ok(CodexRecordProjection::default());
        }

        let core_row = build_sparse_output_row(
            self.raw_ordinal,
            occurred_at,
            result_kind,
            call_id,
            context.as_ref(),
            &structural.outcome,
            structural.output_bytes,
        );
        let core_bytes = core_row
            .as_ref()
            .map(|row| serde_json::to_vec(row).map(|bytes| bytes.len().saturating_add(1)))
            .transpose()?
            .unwrap_or_default();
        if let Some(row) = core_row.as_ref() {
            let body_bytes = serde_json::to_vec(&row.provider_event.payload)?.len();
            self.counters.retained_records = self.counters.retained_records.saturating_add(1);
            self.counters.retained_body_bytes = self
                .counters
                .retained_body_bytes
                .saturating_add(u64::try_from(body_bytes).unwrap_or(u64::MAX));
            self.counters.retained_hashes_created =
                self.counters.retained_hashes_created.saturating_add(1);
        }
        let context_mutation =
            call_id.map(|call_id| CodexContextMutation::Remove(call_id.to_owned()));
        let mut projection = CodexRecordProjection {
            core_row,
            pro_output: None,
            context_mutation,
            core_serialized_bytes: core_bytes,
            pro_serialized_bytes: 0,
        };
        if self.profile == CodexNativeProfile::CoreOnly {
            return Ok(projection);
        }

        self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
        self.counters.typed_output_parses = self.counters.typed_output_parses.saturating_add(1);
        let Some(typed) = parse_decoded_record(record, &owner) else {
            // Structural admission is the shared Core authority. Any failure
            // to hydrate the transient Pro representation stays lane-local.
            return Ok(projection);
        };
        if typed.occurred_at != occurred_at {
            return Ok(projection);
        }
        let content = codex_result_content(&typed.payload)
            .map(|content| content.into_owned())
            .unwrap_or_default()
            .into_bytes();
        self.counters.result_body_bytes_decoded_or_allocated = self
            .counters
            .result_body_bytes_decoded_or_allocated
            .saturating_add(u64::try_from(content.len()).unwrap_or(u64::MAX));
        let output = match self.build_pro_output(
            call_id,
            &owner,
            result_kind,
            context.as_ref(),
            start_byte,
            end_byte,
            occurred_at,
            structural.outcome.clone(),
            content,
        ) {
            Ok(output) => output,
            Err(CaptureError::InvalidPayload(_)) => return Ok(projection),
            Err(error) => return Err(error),
        };
        let Some(output_bytes) = estimated_output_wire_bytes(&output) else {
            return Ok(projection);
        };
        if output_bytes > MAX_CODEX_PAGE_BYTES {
            return Ok(projection);
        }
        self.counters.result_handoffs_created =
            self.counters.result_handoffs_created.saturating_add(1);
        projection.pro_output = Some(output);
        projection.pro_serialized_bytes = output_bytes;
        Ok(projection)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_pro_output(
        &self,
        call_id: Option<&str>,
        owner: &CodexSessionRow,
        result_kind: CodexResultKind,
        context: Option<&CodexToolCallContext>,
        start_byte: u64,
        end_byte: u64,
        occurred_at: DateTime<Utc>,
        outcome: OutputOutcomeMetadata,
        content: Vec<u8>,
    ) -> Result<ProOutputObservation> {
        let locator = serde_json::to_vec(&CodexOutputSourceLocator {
            source_root: &self.source.source_root,
            source_path: &self.source.source_path,
            byte_start: start_byte,
            byte_end_exclusive: end_byte,
            raw_ordinal: self.raw_ordinal,
        })?;
        if locator.len() > MAX_CODEX_OUTPUT_LOCATOR_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Codex output locator exceeds its bounded page allowance".to_owned(),
            ));
        }
        let root_session_id = owner
            .root_native_session_id
            .clone()
            .or_else(|| owner.parent_native_session_id.clone())
            .unwrap_or_else(|| owner.native_session_id.clone());
        let tool_name = context
            .map(|context| context.tool_name.clone())
            .unwrap_or_else(|| result_kind.item_type().to_owned());
        let kind = if codex_is_command_tool(&tool_name) {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        };
        let command = context.map(|context| OutputCommandContext {
            tool_name: context.tool_name.clone(),
            command: context
                .command_preview
                .clone()
                .or_else(|| context.arguments_preview.clone())
                .unwrap_or_default(),
            working_directory: owner.cwd.clone(),
        });
        let line_number = self.raw_ordinal.saturating_add(1);
        Ok(ProOutputObservation {
            kind,
            coordinate: OutputNativeCoordinate {
                unit_key: format!(
                    "codex/nativepath/{}/{}/0",
                    owner.native_session_id, self.raw_ordinal
                ),
                native_sequence: self.raw_ordinal,
                native_record_id: Some(format!("line-{line_number}")),
                source_record_ordinal: Some(self.raw_ordinal),
                source_record_subrecord_index: Some(0),
                byte_start: Some(start_byte),
                byte_end_exclusive: Some(end_byte),
            },
            occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
            associations: OutputAssociations {
                direct_session_id: owner.native_session_id.clone(),
                root_session_id,
                parent_session_id: owner.parent_native_session_id.clone(),
                provider_session_id: Some(owner.native_session_id.clone()),
                agent_id: owner.external_agent_id.clone(),
                repository: None,
            },
            call_id: call_id.map(str::to_owned),
            command,
            outcome,
            locator: OutputSourceLocator {
                version: 1,
                kind: "codex/nativepath/jsonl-result".to_owned(),
                payload: locator,
            },
            content,
        })
    }

    pub(super) fn apply_context_mutation(&mut self, mutation: CodexContextMutation) {
        match mutation {
            CodexContextMutation::Insert(call_id, mut context, authority)
                if call_id.len() <= MAX_CODEX_TOOL_CALL_ID_BYTES =>
            {
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
            CodexContextMutation::Insert(_, _, _) => {}
            CodexContextMutation::Remove(call_id) => {
                self.tool_contexts.remove(&call_id);
                self.tool_authorities.remove(&call_id);
            }
        }
    }

    pub(super) fn reject(
        &mut self,
        start_byte: u64,
        end_byte: u64,
        reason: &'static str,
        oversized: bool,
    ) {
        if oversized {
            self.counters.oversized_records = self.counters.oversized_records.saturating_add(1);
        } else {
            self.counters.malformed_records = self.counters.malformed_records.saturating_add(1);
        }
        if self.rejections.len() < MAX_REJECTION_DETAILS {
            self.rejections.push(CodexRecordRejection {
                raw_ordinal: self.raw_ordinal,
                start_byte,
                end_byte,
                reason,
            });
        }
    }
}

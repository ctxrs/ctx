use super::super::checkpoint::{
    CodexPendingCallOriginV0, CodexPendingCallV0, MAX_CODEX_CALL_ID_BYTES, MAX_CODEX_PENDING_CALLS,
};
use super::*;
use crate::provider::codex::session_meta::{
    select_codex_session_meta_owner, CodexSessionMetaIdentity,
    MAX_CODEX_SESSION_META_PREFIX_RECORDS,
};
use ctx_history_core::{ProviderNativeSessionRelationship, TypedKey};

impl CodexNativeScanner {
    pub(super) fn process_record(
        &mut self,
        record: &[u8],
        physical: CodexPhysicalRecordContext,
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
            self.project_without_parse(projection);
            return Ok(CodexRecordProjection::default());
        }

        self.counters.structural_json_parses =
            self.counters.structural_json_parses.saturating_add(1);
        let classified = classify_codex_record(record).ok();
        if classified
            .as_ref()
            .is_some_and(|probe| probe.lineage_malformed() && probe.item_completed_discriminator())
        {
            self.reject_record(
                physical,
                Some("item_completed"),
                SourceBackedRecordRejectionClass::MalformedRecord,
                "Codex item_completed envelope selectors are ambiguous",
                false,
            );
            return Ok(CodexRecordProjection::default());
        }
        let Some(probe) = classified
            .filter(|probe| !probe.lineage_malformed())
            .or_else(|| classify_after_selector_ambiguity(record))
        else {
            self.reject_record(
                physical,
                None,
                SourceBackedRecordRejectionClass::MalformedRecord,
                "Codex record is not valid projectable JSON",
                false,
            );
            return Ok(CodexRecordProjection::default());
        };
        let payload_type = codex_record_payload_type(probe.class);
        match probe.class {
            CodexRecordClass::DescendantActivity | CodexRecordClass::DescendantStarted => {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::SessionMeta => {
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                match parse_session_meta(record) {
                    Some(metadata) => self.observe_session_metadata(metadata)?,
                    None => self.reject_record(
                        physical,
                        Some(payload_type),
                        SourceBackedRecordRejectionClass::MalformedRecord,
                        "Codex session_meta record is malformed",
                        false,
                    ),
                }
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::TurnContext => {
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                match (self.owner.as_mut(), parse_turn_context(record)) {
                    (Some(owner), Some((cwd, turn_id))) => {
                        owner.cwd = Some(cwd);
                        self.local_turn_started |= turn_id.as_deref().is_some_and(|turn_id| {
                            valid_local_turn_boundary(&owner.native_session_id, turn_id)
                        });
                    }
                    (None, _) | (_, None) => self.reject_record(
                        physical,
                        Some(payload_type),
                        SourceBackedRecordRejectionClass::MalformedRecord,
                        "Codex turn_context record is malformed or has no admitted owner",
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
                    self.reject_record(
                        physical,
                        Some(payload_type),
                        SourceBackedRecordRejectionClass::MalformedRecord,
                        "Codex retained record has no admitted session owner",
                        false,
                    );
                    return Ok(CodexRecordProjection::default());
                };
                self.counters.retained_json_parses =
                    self.counters.retained_json_parses.saturating_add(1);
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                let Some(retained) = parse_decoded_record(record, owner) else {
                    self.reject_record(
                        physical,
                        Some(payload_type),
                        SourceBackedRecordRejectionClass::MalformedRecord,
                        "Codex retained record payload is malformed",
                        false,
                    );
                    return Ok(CodexRecordProjection::default());
                };
                let mut built = match build_source_backed_event_row(
                    physical.raw_ordinal,
                    kind,
                    &owner.native_session_id,
                    &retained,
                    record,
                )? {
                    Ok(built) => built,
                    Err(CodexRetainedNonMaterialized::ValidUnmaterializable) => {
                        self.counters.ignored_records =
                            self.counters.ignored_records.saturating_add(1);
                        return Ok(CodexRecordProjection::default());
                    }
                    Err(CodexRetainedNonMaterialized::KnownNonMaterialized) => {
                        self.counters.ignored_records =
                            self.counters.ignored_records.saturating_add(1);
                        return Ok(CodexRecordProjection::default());
                    }
                    Err(CodexRetainedNonMaterialized::Unsupported) => {
                        self.reject_record(
                            physical,
                            Some(payload_type),
                            SourceBackedRecordRejectionClass::UnsupportedRecord,
                            "Codex item_completed variant is not projected; response_item remains authoritative where available",
                            false,
                        );
                        return Ok(CodexRecordProjection::default());
                    }
                    Err(CodexRetainedNonMaterialized::Malformed) => {
                        self.reject_record(
                            physical,
                            Some(payload_type),
                            SourceBackedRecordRejectionClass::MalformedRecord,
                            "Codex retained record semantic payload is malformed",
                            false,
                        );
                        return Ok(CodexRecordProjection::default());
                    }
                };
                built.row.session_cwd.clone_from(&owner.cwd);
                let insert_pending_call = pending_call_for_row(
                    owner,
                    self.local_turn_started,
                    physical.raw_ordinal,
                    &built.row,
                );
                let row_bytes = built.row.estimated_owned_bytes().unwrap_or(usize::MAX);
                if row_bytes
                    > MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
                        .saturating_sub(PAGE_FIXED_WIRE_BYTES)
                {
                    self.reject_record(
                        physical,
                        Some(payload_type),
                        SourceBackedRecordRejectionClass::UnsupportedRecord,
                        "Codex retained record exceeds the bounded projection row envelope",
                        false,
                    );
                    return Ok(CodexRecordProjection::default());
                }
                let lexical_bytes = built.row.lexical_body.len();
                self.counters.retained_records = self.counters.retained_records.saturating_add(1);
                self.counters.retained_body_bytes = self
                    .counters
                    .retained_body_bytes
                    .saturating_add(u64::try_from(lexical_bytes).unwrap_or(u64::MAX));
                Ok(CodexRecordProjection {
                    context_mutation: Some(CodexContextMutation::SourceBackedRow {
                        row: built.row,
                        estimated_bytes: row_bytes,
                        insert_pending_call,
                        remove_pending_call_id: None,
                    }),
                })
            }
            CodexRecordClass::ExcludedResult(result_kind) => {
                self.process_output(record, &probe, result_kind, physical)
            }
        }
    }

    pub(super) fn observe_session_metadata(&mut self, metadata: CodexSessionRow) -> Result<()> {
        let is_owner = self.source.catalog_native_session_id.as_deref()
            == Some(metadata.native_session_id.as_str());
        if let Some(existing) = self
            .session_metadata
            .iter()
            .find(|existing| existing.native_session_id == metadata.native_session_id)
        {
            validate_repeated_owner(existing, &metadata)?;
            if is_owner && self.owner.is_none() {
                self.owner = Some(validate_catalog_owner(&self.source, metadata)?);
            }
            self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
            return Ok(());
        }
        if self.session_metadata.len() >= MAX_CODEX_SESSION_META_PREFIX_RECORDS {
            return Err(CaptureError::InvalidPayload(
                "Codex session_meta prefix exceeds the ownership bound".to_owned(),
            ));
        }
        if is_owner {
            self.owner = Some(validate_catalog_owner(&self.source, metadata.clone())?);
        } else {
            self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
        }
        self.session_metadata.push(metadata);
        Ok(())
    }

    pub(super) fn validate_session_metadata_owner(&mut self) -> Result<()> {
        if self.session_metadata.is_empty() {
            return Ok(());
        }
        let identities = self
            .session_metadata
            .iter()
            .map(|metadata| CodexSessionMetaIdentity {
                native_session_id: metadata.native_session_id.clone(),
                parent_native_session_id: metadata.parent_native_session_id.clone(),
                root_native_session_id: metadata.root_native_session_id.clone(),
                session_relationship: metadata.session_relationship,
            })
            .collect::<Vec<_>>();
        let expected_owner = self.source.catalog_native_session_id.as_deref();
        let owner =
            select_codex_session_meta_owner(&identities, expected_owner).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Codex session_meta owner changed within one source".to_owned(),
                )
            })?;
        let owner =
            self.session_metadata
                .get(owner)
                .cloned()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex selected session_meta owner is missing",
                ))?;
        let owner = validate_catalog_owner(&self.source, owner)?;
        if let Some(admitted_owner) = self.owner.as_ref() {
            validate_repeated_owner(admitted_owner, &owner)?;
        } else {
            self.owner = Some(owner);
        }
        Ok(())
    }

    /// Applies the counter-only projection the prefilter proved sufficient.
    ///
    /// The arm mirrors the ignored-record counter in the parsed path exactly.
    fn project_without_parse(&mut self, projection: CodexSkipProjection) {
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
        _result_kind: CodexResultKind,
        physical: CodexPhysicalRecordContext,
    ) -> Result<CodexRecordProjection> {
        self.counters.native_result_records = self.counters.native_result_records.saturating_add(1);
        self.counters.native_result_record_bytes = self
            .counters
            .native_result_record_bytes
            .saturating_add(physical.end_byte.saturating_sub(physical.start_byte));

        let call_id = probe.call_id.as_deref();
        let source_unique_terminal =
            call_id.is_some_and(|call_id| self.terminal_authority.is_unique(call_id));
        let linked_invocation_discovery_exclusion = source_unique_terminal
            .then(|| {
                call_id
                    .and_then(|call_id| self.pending_calls.get(call_id))
                    .and_then(|pending| pending.discovery_exclusion)
            })
            .flatten();
        let result_event_type = call_id
            .and_then(|call_id| self.pending_calls.get(call_id))
            .map_or(ctx_history_core::EventType::ToolOutput, |pending| {
                pending.result_event_type
            });
        let provider_event_copy = call_id.and_then(|call_id| {
            self.pending_calls
                .get(call_id)
                .and_then(|pending| match &pending.origin {
                    CodexPendingCallOriginV0::CopiedFromAncestor {
                        ancestor_native_session_id,
                    } => Some(super::super::rows::CodexProviderNativeEventCopyV0 {
                        ancestor_native_session_id: ancestor_native_session_id.clone(),
                        result_call_id: call_id.to_owned(),
                    }),
                    CodexPendingCallOriginV0::CurrentSession
                    | CodexPendingCallOriginV0::Unproven => None,
                })
        });
        let Some(owner) = self.owner.clone() else {
            self.reject_record(
                physical,
                Some(codex_record_payload_type(probe.class)),
                SourceBackedRecordRejectionClass::MalformedRecord,
                "Codex result record has no admitted session owner",
                false,
            );
            return Ok(CodexRecordProjection::default());
        };
        let Some(occurred_at) = probe_timestamp(probe, owner.started_at) else {
            self.reject_record(
                physical,
                Some(codex_record_payload_type(probe.class)),
                SourceBackedRecordRejectionClass::MalformedRecord,
                "Codex result record has no valid timestamp",
                false,
            );
            return Ok(CodexRecordProjection::default());
        };

        self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
        let decoded = parse_decoded_record(record, &owner);
        let decoded = decoded.as_ref().ok_or(CaptureError::SystemInvariant(
            "Codex output could not be decoded for complete Core publication",
        ))?;
        let result_content = decoded
            .payload
            .get("output")
            .or_else(|| decoded.payload.get("result"));
        let audit = audit_codex_record(record)?;
        let normalized_body = if audit.selector_ambiguous(SelectorGroup::Result) {
            "Codex tool result".to_owned()
        } else {
            match result_content.unwrap_or(&decoded.payload) {
                Value::String(value) => value.clone(),
                value => serde_json::to_string(value)?,
            }
        };
        let content_exceeds_core = normalized_body.len() > ctx_history_core::MAX_CORE_CONTENT_BYTES;
        let structured_content = (!content_exceeds_core && !audit.any_selector_ambiguous())
            .then(|| decoded.payload.clone());
        let core_row = build_source_backed_sparse_output_row(
            physical.raw_ordinal,
            provider_event_identity(&decoded.payload),
            provider_event_copy,
            linked_invocation_discovery_exclusion,
            source_unique_terminal,
            result_event_type,
            call_id,
            occurred_at,
            normalized_body,
            structured_content,
            result_content,
            record,
            &decoded.payload,
            owner.cwd.clone(),
        )?;
        let context_mutation = match core_row {
            Some(row) => {
                let row_bytes = row.estimated_owned_bytes().unwrap_or(usize::MAX);
                if row_bytes
                    > MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
                        .saturating_sub(PAGE_FIXED_WIRE_BYTES)
                {
                    self.reject_record(
                        physical,
                        Some(codex_record_payload_type(probe.class)),
                        SourceBackedRecordRejectionClass::UnsupportedRecord,
                        "Codex result record exceeds the bounded projection row envelope",
                        false,
                    );
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
                        estimated_bytes: row_bytes,
                        insert_pending_call: None,
                        remove_pending_call_id: call_id.map(str::to_owned),
                    }),
                });
            }
            None => None,
        };
        Ok(CodexRecordProjection { context_mutation })
    }

    pub(super) fn apply_context_mutation(&mut self, mutation: CodexContextMutation) -> Result<()> {
        self.apply_context_mutation_inner(mutation, true)
    }

    fn apply_context_mutation_inner(
        &mut self,
        mutation: CodexContextMutation,
        emit_record: bool,
    ) -> Result<()> {
        match mutation {
            CodexContextMutation::SourceBackedRow {
                row,
                estimated_bytes,
                insert_pending_call,
                remove_pending_call_id,
            } => {
                let record = if emit_record {
                    debug_assert!(self.active_core_page.is_some());
                    let owner = self.owner.as_ref().ok_or(CaptureError::SystemInvariant(
                        "Codex retained record has no session owner",
                    ))?;
                    let raw_ordinal = row.raw_ordinal;
                    let retained_body_bytes =
                        u64::try_from(row.lexical_body.len()).unwrap_or(u64::MAX);
                    let record = codex_core_record(
                        &self.core_source,
                        self.core_session_id,
                        self.source.source_root_lineage,
                        owner,
                        row,
                        &mut self.event_identity_state,
                    )?;
                    let Some(record) = record else {
                        self.counters.retained_records =
                            self.counters.retained_records.saturating_sub(1);
                        self.counters.retained_body_bytes = self
                            .counters
                            .retained_body_bytes
                            .saturating_sub(retained_body_bytes);
                        if let Some(page) = self.active_core_page.as_mut() {
                            page.serialized_bytes =
                                page.serialized_bytes.saturating_sub(estimated_bytes);
                        }
                        self.reject_record_at_ordinal(
                            raw_ordinal,
                            None,
                            SourceBackedRecordRejectionClass::UnsupportedRecord,
                            "Codex retained record exceeds the Core selected-content envelope",
                            false,
                        );
                        return Ok(());
                    };
                    Some(record)
                } else {
                    None
                };
                if let Some(call_id) = remove_pending_call_id {
                    self.pending_calls.remove(&call_id);
                }
                if let Some((call_id, pending)) = insert_pending_call {
                    if call_id.len() <= MAX_CODEX_CALL_ID_BYTES {
                        match self.pending_calls.entry(call_id) {
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                entry.insert(pending);
                            }
                            std::collections::btree_map::Entry::Occupied(mut entry) => {
                                entry.get_mut().origin = CodexPendingCallOriginV0::Unproven;
                                entry.get_mut().result_event_type =
                                    ctx_history_core::EventType::ToolOutput;
                                entry.get_mut().discovery_exclusion = None;
                            }
                        }
                        while self.pending_calls.len() > MAX_CODEX_PENDING_CALLS {
                            let Some(oldest) = self
                                .pending_calls
                                .iter()
                                .min_by_key(|(_, pending)| pending.raw_ordinal)
                                .map(|(call_id, _)| call_id.clone())
                            else {
                                break;
                            };
                            self.pending_calls.remove(&oldest);
                        }
                    }
                }
                if let (Some(page), Some(record)) = (self.active_core_page.as_mut(), record) {
                    page.records.push(record);
                }
            }
        }
        Ok(())
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

    pub(super) fn reject_record(
        &mut self,
        physical: CodexPhysicalRecordContext,
        payload_type: Option<&str>,
        class: SourceBackedRecordRejectionClass,
        detail: &'static str,
        oversized: bool,
    ) {
        self.reject_record_at_ordinal(physical.raw_ordinal, payload_type, class, detail, oversized);
    }

    fn reject_record_at_ordinal(
        &mut self,
        raw_ordinal: u64,
        payload_type: Option<&str>,
        class: SourceBackedRecordRejectionClass,
        detail: &'static str,
        oversized: bool,
    ) {
        self.reject(oversized);
        self.record_rejections
            .record(SourceBackedRecordRejectionDraft {
                source: self.core_source.clone(),
                provider: ctx_history_core::CaptureProvider::Codex,
                source_selector: self.source.source_path.display().to_string(),
                line_number: raw_ordinal.saturating_add(1),
                payload_type: payload_type.map(str::to_owned),
                class,
                detail: detail.to_owned(),
            });
    }
}

fn codex_record_payload_type(class: CodexRecordClass) -> &'static str {
    match class {
        CodexRecordClass::SessionMeta => "session_meta",
        CodexRecordClass::TurnContext => "turn_context",
        CodexRecordClass::DescendantActivity => "agent_message",
        CodexRecordClass::DescendantStarted => "agent_message.started",
        CodexRecordClass::Retained(CodexRetainedKind::Message) => "message",
        CodexRecordClass::Retained(CodexRetainedKind::Reasoning) => "reasoning",
        CodexRecordClass::Retained(CodexRetainedKind::Compacted) => "compacted",
        CodexRecordClass::Retained(CodexRetainedKind::ToolCall) => "tool_call",
        CodexRecordClass::Retained(CodexRetainedKind::ItemCompleted) => "item_completed",
        CodexRecordClass::ExcludedResult(CodexResultKind::FunctionCallOutput) => {
            "function_call_output"
        }
        CodexRecordClass::ExcludedResult(CodexResultKind::CustomToolCallOutput) => {
            "custom_tool_call_output"
        }
        CodexRecordClass::ExcludedResult(CodexResultKind::ToolSearchOutput) => "tool_search_output",
        CodexRecordClass::ExcludedResult(CodexResultKind::OtherResult) => "tool_result",
        CodexRecordClass::Ignored => "ignored",
    }
}

fn pending_call_for_row(
    owner: &CodexSessionRow,
    local_turn_started: bool,
    raw_ordinal: u64,
    row: &CodexCoreRecordDraft,
) -> Option<(String, CodexPendingCallV0)> {
    let provider_identity = row.provider_event_identity.as_ref()?;
    let activity = row.activity.as_ref()?;
    let TypedKey::Utf8(call_id) = activity.provider_call_id.as_ref()? else {
        return None;
    };
    if provider_identity.kind != super::super::rows::CodexProviderEventIdentityKindV0::CallId
        || provider_identity.value != *call_id
        || activity.invocation.is_none()
        || activity.result.is_some()
    {
        return None;
    }
    Some((
        call_id.clone(),
        CodexPendingCallV0 {
            raw_ordinal,
            origin: pending_call_origin(owner, local_turn_started),
            result_event_type: activity
                .invocation
                .as_ref()
                .filter(|invocation| {
                    ctx_history_capture_model::tool_input::is_command_tool(&invocation.tool)
                })
                .map_or(ctx_history_core::EventType::ToolOutput, |_| {
                    ctx_history_core::EventType::CommandOutput
                }),
            discovery_exclusion: row.discovery_exclusion,
        },
    ))
}

fn pending_call_origin(
    owner: &CodexSessionRow,
    local_turn_started: bool,
) -> CodexPendingCallOriginV0 {
    match owner.session_relationship {
        Some(
            ProviderNativeSessionRelationship::Root
            | ProviderNativeSessionRelationship::Delegated
            | ProviderNativeSessionRelationship::WorkflowChild,
        ) => CodexPendingCallOriginV0::CurrentSession,
        Some(
            ProviderNativeSessionRelationship::Forked
            | ProviderNativeSessionRelationship::ResumedFrom,
        ) if local_turn_started => CodexPendingCallOriginV0::CurrentSession,
        Some(
            ProviderNativeSessionRelationship::Forked
            | ProviderNativeSessionRelationship::ResumedFrom,
        ) => owner.parent_native_session_id.clone().map_or(
            CodexPendingCallOriginV0::Unproven,
            |ancestor_native_session_id| CodexPendingCallOriginV0::CopiedFromAncestor {
                ancestor_native_session_id,
            },
        ),
        None => CodexPendingCallOriginV0::Unproven,
    }
}

fn valid_local_turn_boundary(native_session_id: &str, turn_id: &str) -> bool {
    let (Ok(native_session_id), Ok(turn_id)) = (
        uuid::Uuid::parse_str(native_session_id),
        uuid::Uuid::parse_str(turn_id),
    ) else {
        return false;
    };
    uuid_v7_unix_timestamp_ms(&native_session_id)
        .zip(uuid_v7_unix_timestamp_ms(&turn_id))
        .is_some_and(|(session_ms, turn_ms)| turn_ms > session_ms)
}

fn uuid_v7_unix_timestamp_ms(value: &uuid::Uuid) -> Option<u64> {
    (value.get_version_num() == 7).then_some((value.as_u128() >> 80) as u64)
}

#[cfg(test)]
mod tests;

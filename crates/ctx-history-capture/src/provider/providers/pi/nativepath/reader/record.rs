use super::*;

impl PiNativeScanner {
    pub(super) fn parse_pending(
        &mut self,
        ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        bytes: &[u8],
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(PendingRecord {
                core_units: Vec::new(),
                core_encoded_bytes: 0,
                output: None,
                output_estimated_bytes: 0,
                checkpoint,
            });
        }
        let line_number = ordinal.saturating_add(1);
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                self.stats.malformed_records = self.stats.malformed_records.saturating_add(1);
                return self.rejection_pending(
                    PiNativeRejectionKind::MalformedJson,
                    ordinal,
                    line_number,
                    byte_start,
                    byte_end_exclusive,
                    error.to_string(),
                    checkpoint,
                );
            }
        };
        let entry_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if entry_type == "session" {
            return match parse_pi_session_header(value) {
                Ok(header) => {
                    let row = self
                        .core_is_active()
                        .then(|| self.session_row(&header))
                        .transpose()?;
                    self.header = Some(header);
                    self.core_units_pending(
                        row.into_iter().map(PiNativeCoreUnit::Session).collect(),
                        checkpoint,
                    )
                }
                Err(error) => self.rejection_pending(
                    PiNativeRejectionKind::InvalidHeader,
                    ordinal,
                    line_number,
                    byte_start,
                    byte_end_exclusive,
                    error.to_string(),
                    checkpoint,
                ),
            };
        }

        let event_type = pi_native_event_type(entry_type, value.get("message"));
        if matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
            return self.output_pending(
                &value,
                event_type,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                checkpoint,
            );
        }
        let Some(header) = self.header.as_ref() else {
            return self.rejection_pending(
                PiNativeRejectionKind::BeforeHeader,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                "pi session entry appeared before session header",
                checkpoint,
            );
        };
        if !self.core_is_active() {
            return self.core_units_pending(Vec::new(), checkpoint);
        }
        let mut units = match self.event_and_touches(
            header,
            &value,
            ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
            Some(bytes),
            None,
        ) {
            Ok(units) => units,
            Err(error) => {
                return self.rejection_pending(
                    PiNativeRejectionKind::InvalidRecord,
                    ordinal,
                    line_number,
                    byte_start,
                    byte_end_exclusive,
                    error.to_string(),
                    checkpoint,
                );
            }
        };
        self.bound_core_units(
            &mut units,
            ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
            checkpoint,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn output_pending(
        &mut self,
        entry: &Value,
        event_type: EventType,
        ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        self.stats.native_result_records = self.stats.native_result_records.saturating_add(1);
        let Some(header) = self.header.as_ref() else {
            return self.rejection_pending(
                PiNativeRejectionKind::BeforeHeader,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                "pi session entry appeared before session header",
                checkpoint,
            );
        };
        let outcome = pi_output_outcome(entry, event_type);
        let occurred_at = match entry
            .get("timestamp")
            .and_then(Value::as_str)
            .ok_or_else(|| "pi session event missing timestamp".to_owned())
            .and_then(|timestamp| {
                chrono::DateTime::parse_from_rfc3339(timestamp)
                    .map(|timestamp| timestamp.with_timezone(&Utc))
                    .map_err(|error| error.to_string())
            }) {
            Ok(occurred_at) => occurred_at,
            Err(error) => {
                return self.rejection_pending(
                    PiNativeRejectionKind::InvalidRecord,
                    ordinal,
                    line_number,
                    byte_start,
                    byte_end_exclusive,
                    error,
                    checkpoint,
                );
            }
        };
        match outcome.outcome {
            OutputOutcome::Success => {
                self.stats.native_result_success =
                    self.stats.native_result_success.saturating_add(1)
            }
            OutputOutcome::Failure => {
                self.stats.native_result_failure =
                    self.stats.native_result_failure.saturating_add(1)
            }
            OutputOutcome::Timeout => {
                self.stats.native_result_timeout =
                    self.stats.native_result_timeout.saturating_add(1)
            }
            OutputOutcome::Unknown => {
                self.stats.native_result_unknown =
                    self.stats.native_result_unknown.saturating_add(1)
            }
        }
        let explicit_failure = matches!(
            outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        );
        let command = pi_output_command_context(entry, header);
        let retained_failure =
            explicit_failure && (event_type != EventType::CommandOutput || command.is_some());
        let wants_output = self.output.as_ref().is_some_and(|lane| lane.active);
        let wants_core = self.core_is_active();
        let result_body = (wants_output || retained_failure)
            .then(|| {
                self.stats.result_body_extractions =
                    self.stats.result_body_extractions.saturating_add(1);
                pi_result_content(entry)
            })
            .flatten();
        let mut output = None;
        let mut output_estimated_bytes = 0;
        if wants_output {
            if let Some(content) = result_body.as_ref() {
                if content.len() <= PI_OUTPUT_BODY_MAX_BYTES {
                    let observation = output_observation(
                        header,
                        entry,
                        event_type,
                        ordinal,
                        line_number,
                        byte_start,
                        byte_end_exclusive,
                        &self.locator_source_item,
                        occurred_at,
                        command.clone(),
                        outcome.clone(),
                        content,
                    )?;
                    output_estimated_bytes = output_estimated_bytes_for(&observation);
                    if PI_OUTPUT_PAGE_ENCODING_RESERVE.saturating_add(output_estimated_bytes)
                        <= PI_NATIVE_PAGE_MAX_BYTES
                    {
                        self.stats.pro_result_body_bytes = self
                            .stats
                            .pro_result_body_bytes
                            .saturating_add(u64::try_from(content.len()).unwrap_or(u64::MAX));
                        output = Some(observation);
                    } else {
                        self.stats.oversized_records =
                            self.stats.oversized_records.saturating_add(1);
                        output_estimated_bytes = 0;
                    }
                } else {
                    self.stats.oversized_records = self.stats.oversized_records.saturating_add(1);
                }
            }
        }

        let mut core_units = if wants_core && retained_failure {
            match self.event_and_touches(
                header,
                entry,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                None,
                Some((&outcome, result_body.as_deref())),
            ) {
                Ok(units) => units,
                Err(error) => {
                    return self.rejection_pending(
                        PiNativeRejectionKind::InvalidRecord,
                        ordinal,
                        line_number,
                        byte_start,
                        byte_end_exclusive,
                        error.to_string(),
                        checkpoint,
                    );
                }
            }
        } else {
            Vec::new()
        };
        if !retained_failure {
            debug_assert!(core_units.is_empty());
            debug_assert_eq!(self.stats.successful_or_unknown_core_bodies, 0);
            debug_assert_eq!(self.stats.successful_or_unknown_core_hashes, 0);
            debug_assert_eq!(self.stats.successful_or_unknown_core_previews, 0);
            debug_assert_eq!(self.stats.successful_or_unknown_core_touches, 0);
            debug_assert_eq!(self.stats.successful_or_unknown_core_fts_documents, 0);
        }
        let core_encoded_bytes = self.bound_core_units_encoded(
            &mut core_units,
            ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
        )?;
        Ok(PendingRecord {
            core_units,
            core_encoded_bytes,
            output,
            output_estimated_bytes,
            checkpoint,
        })
    }

    pub(super) fn session_row(
        &self,
        header: &PiNativeSessionHeader,
    ) -> Result<PiNativeSessionRow, PiNativePathError> {
        Ok(PiNativeSessionRow {
            provider_session_id: header.id.clone(),
            version: header.version,
            started_at: header.timestamp,
            cwd: header.cwd.clone(),
            parent_session: header.parent_session.clone(),
            source_metadata: json!({
                "adapter": PI_SOURCE_FORMAT,
                "source_fidelity": "documented_session_jsonl",
            }),
            session_metadata: json!({
                "source_format": PI_SOURCE_FORMAT,
                "source_fidelity": "documented_session_jsonl",
                "version": header.version,
                "parent_session": header.parent_session,
                "header": header.raw,
                "limitations": [
                    "message branch parentId values are preserved as event metadata, not ctx session edges",
                    "files touched are available only when Pi message payloads include them",
                    "raw image content is not expanded into artifacts by this importer"
                ],
            }),
            source_idempotency_key: format!("provider-source:pi:{PI_SOURCE_FORMAT}:{}", header.id),
            session_idempotency_key: format!("provider-session:pi:{}", header.id),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn event_and_touches(
        &self,
        header: &PiNativeSessionHeader,
        entry: &Value,
        ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        record_bytes: Option<&[u8]>,
        failure: Option<(
            &crate::provider::importer::OutputOutcomeMetadata,
            Option<&str>,
        )>,
    ) -> Result<Vec<PiNativeCoreUnit>, PiNativePathError> {
        let line_number_usize =
            usize::try_from(line_number).map_err(|_| PiNativePathError::PositionOverflow)?;
        let entry_type = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let message = entry.get("message");
        let message_role = message
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str);
        let occurred_at = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                PiNativePathError::Normalization(CaptureError::InvalidPayload(
                    "pi session event missing timestamp".to_owned(),
                ))
            })
            .and_then(|timestamp| {
                DateTime::parse_from_rfc3339(timestamp)
                    .map(|time| time.with_timezone(&Utc))
                    .map_err(CaptureError::from)
                    .map_err(PiNativePathError::from)
            })?;
        let event_type = pi_native_event_type(entry_type, message);
        let role = message_role.map(pi_event_role);
        let mut payload = pi_native_event_payload(entry, event_type);
        if let Some((outcome, content)) = failure {
            let payload = payload.as_object_mut().ok_or_else(|| {
                PiNativePathError::Normalization(CaptureError::SystemInvariant(
                    "Pi failure event payload must be an object",
                ))
            })?;
            payload.insert("result_outcome".to_owned(), json!("failure"));
            payload.insert(
                "timed_out".to_owned(),
                json!(outcome.outcome == OutputOutcome::Timeout),
            );
            if let Some(exit_code) = outcome.exit_code {
                payload.insert("exit_code".to_owned(), json!(exit_code));
            }
            if let Some(duration_ms) = outcome.duration_ms {
                payload.insert("duration_ms".to_owned(), json!(duration_ms));
            }
            if let Some(content) = content {
                payload.insert("output_bytes".to_owned(), json!(content.len()));
                let (preview, _) = crate::provider::normalization::provider_local_preview(
                    content,
                    PROVIDER_MAX_PREVIEW_CHARS,
                );
                if !preview.trim().is_empty() {
                    payload.insert("output_preview".to_owned(), Value::String(preview));
                }
            }
        }
        let provider_event_identity_index =
            pi_provider_event_identity_index(header, entry).unwrap_or(ordinal);
        let locator = PiNativePhysicalLocator {
            path: self.source.path.clone(),
            source_record_ordinal: ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
        };
        let mut event_row = PiNativeEventRow {
            provider_session_id: header.id.clone(),
            provider_event_index: ordinal,
            provider_event_identity_index,
            cursor: entry.get("id").and_then(Value::as_str).map(str::to_owned),
            event_type,
            role,
            occurred_at,
            idempotency_key: pi_event_idempotency_key(header, entry, line_number_usize),
            payload,
            metadata: json!({
                "source": "pi_session",
                "source_format": PI_SOURCE_FORMAT,
                "line": line_number,
                "entry_type": entry_type,
                "entry_id": entry.get("id").and_then(Value::as_str),
                "parent_id": entry.get("parentId").and_then(Value::as_str),
                "provider_event_identity_index": provider_event_identity_index,
                "message_role": message_role,
                "model": message
                    .and_then(|message| message.get("model"))
                    .and_then(Value::as_str),
                "provider": message
                    .and_then(|message| message.get("provider"))
                    .and_then(Value::as_str),
                "usage": message.and_then(|message| message.get("usage")).cloned(),
            }),
            locator,
        };
        if let Some(record_bytes) = record_bytes {
            attach_complete_message_locator(
                &mut event_row,
                entry,
                record_bytes,
                byte_start,
                byte_end_exclusive,
                line_number_usize,
            )?;
        }
        let mut units = vec![PiNativeCoreUnit::Event(event_row)];
        let provider_touch_base_index = ordinal
            .checked_shl(16)
            .ok_or(PiNativePathError::PositionOverflow)?;
        let raw_source_path = self
            .context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string());
        let source_root = self.context.source_root_display();
        let occurred_at = event_row_occurred_at(&units)?;
        let outcome = visit_provider_file_touch_drafts_with_limit(
            entry,
            false,
            PI_CORE_TOUCH_LIMIT,
            |(touch_ordinal, touch)| {
                let provider_touch_index = if ordinal > MAX_PACKED_PROVIDER_EVENT_INDEX {
                    touch_ordinal
                } else {
                    provider_touch_base_index | touch_ordinal
                };
                units.push(PiNativeCoreUnit::FileTouch(PiNativeFileTouchRow {
                    provider_session_id: header.id.clone(),
                    provider_touch_index,
                    provider_event_index: Some(ordinal),
                    raw_source_path: raw_source_path.clone(),
                    source_root: source_root.clone(),
                    path: touch.path,
                    change_kind: touch.change_kind,
                    old_path: touch.old_path,
                    line_count_delta: None,
                    confidence: touch.confidence,
                    occurred_at,
                    source_format: PI_SOURCE_FORMAT.to_owned(),
                    metadata: touch.metadata,
                }));
                Ok::<(), PiNativePathError>(())
            },
        )?;
        if outcome.limit_exceeded() {
            return Err(PiNativePathError::InvalidSource {
                path: self.source.path.clone(),
                reason: "Pi normalized record exceeds the NativePath Core unit limit".to_owned(),
            });
        }
        Ok(units)
    }
}

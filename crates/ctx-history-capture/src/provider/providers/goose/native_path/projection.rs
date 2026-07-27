use super::*;

impl GooseNativeScanner<'_> {
    pub(super) fn project_message(
        &mut self,
        scanned: GooseScannedMessage,
        page: &mut GooseNativePage,
    ) -> Result<()> {
        match scanned.disposition {
            GooseMessageCellDisposition::Retained => {
                self.metrics.retained_content_cells_transferred = self
                    .metrics
                    .retained_content_cells_transferred
                    .saturating_add(1);
                self.metrics.retained_content_bytes_transferred = self
                    .metrics
                    .retained_content_bytes_transferred
                    .saturating_add(scanned.content_bytes);
                let raw_content =
                    scanned
                        .content_json
                        .as_deref()
                        .ok_or(CaptureError::SystemInvariant(
                            "Goose retained SQLite row omitted content_json",
                        ))?;
                self.hash_message_header(&scanned, b"retained");
                goose_hash_bytes(&mut self.semantic_hasher, raw_content.as_bytes());
                let rejection_rowid = scanned.sqlite_rowid;
                let rejection_order = scanned.native_order;
                let rejection_identity = scanned.native_identity.clone();
                let rejection_session = scanned.session_identity.clone();
                match normalize_goose_native_message(scanned.into_retained()?) {
                    Ok(event) => {
                        self.metrics.retained_events =
                            self.metrics.retained_events.saturating_add(1);
                        page.events.push(event);
                    }
                    Err(error) => {
                        let rejection = GooseNativeRejection {
                            sqlite_rowid: rejection_rowid,
                            native_order: Some(rejection_order),
                            native_identity: rejection_identity,
                            session_identity: Some(rejection_session),
                            kind: GooseNativeRejectionKind::RetainedParseMismatch,
                            reason: error.to_string(),
                        };
                        self.hash_rejection(&rejection);
                        self.metrics.rejected_records =
                            self.metrics.rejected_records.saturating_add(1);
                        page.rejections.push(rejection);
                    }
                }
            }
            GooseMessageCellDisposition::OutputSuccess
            | GooseMessageCellDisposition::OutputFailure
            | GooseMessageCellDisposition::OutputTimeout
            | GooseMessageCellDisposition::OutputUnknown => {
                let outcome = scanned.output_outcome.ok_or(CaptureError::SystemInvariant(
                    "Goose output row omitted its SQL-classified outcome",
                ))?;
                self.hash_message_header(&scanned, b"output");
                self.semantic_hasher
                    .update([goose_output_outcome_code(outcome) as u8]);
                self.metrics.excluded_outputs = self.metrics.excluded_outputs.saturating_add(1);
                self.metrics.excluded_output_bytes_observed = self
                    .metrics
                    .excluded_output_bytes_observed
                    .saturating_add(scanned.content_bytes);
                match outcome {
                    OutputOutcome::Success => {
                        self.metrics.outputs_success =
                            self.metrics.outputs_success.saturating_add(1)
                    }
                    OutputOutcome::Failure => {
                        self.metrics.outputs_failure =
                            self.metrics.outputs_failure.saturating_add(1)
                    }
                    OutputOutcome::Timeout => {
                        self.metrics.outputs_timeout =
                            self.metrics.outputs_timeout.saturating_add(1)
                    }
                    OutputOutcome::Unknown => {
                        self.metrics.outputs_unknown =
                            self.metrics.outputs_unknown.saturating_add(1)
                    }
                }
                if matches!(outcome, OutputOutcome::Failure | OutputOutcome::Timeout) {
                    if scanned.content_json.is_some() {
                        self.metrics.output_content_cells_transferred = self
                            .metrics
                            .output_content_cells_transferred
                            .saturating_add(1);
                        self.metrics.output_content_bytes_transferred = self
                            .metrics
                            .output_content_bytes_transferred
                            .saturating_add(scanned.content_bytes);
                    }
                    let event = normalize_goose_native_output_diagnostic(&scanned)?;
                    let digest = goose_event_content_digest(&event);
                    self.metrics.output_hashes_built =
                        self.metrics.output_hashes_built.saturating_add(1);
                    self.metrics.output_previews_built =
                        self.metrics.output_previews_built.saturating_add(1);
                    self.metrics.retained_events = self.metrics.retained_events.saturating_add(1);
                    self.semantic_hasher.update(b"output-diagnostic");
                    goose_hash_str(&mut self.semantic_hasher, &digest);
                    page.events.push(event);
                }
            }
            disposition => {
                let kind = goose_rejection_kind(disposition)?;
                let rejection = GooseNativeRejection {
                    sqlite_rowid: scanned.sqlite_rowid,
                    native_order: Some(scanned.native_order),
                    native_identity: scanned.native_identity,
                    session_identity: (!scanned.session_identity.is_empty())
                        .then_some(scanned.session_identity),
                    kind,
                    reason: format!(
                        "Goose message row {} rejected as {}",
                        scanned.sqlite_rowid,
                        kind.as_str()
                    ),
                };
                self.hash_rejection(&rejection);
                self.metrics.rejected_records = self.metrics.rejected_records.saturating_add(1);
                page.rejections.push(rejection);
            }
        }
        Ok(())
    }

    pub(super) fn empty_page(&self, frontier: GooseNativeScanPosition) -> GooseNativePage {
        GooseNativePage {
            identity: GooseNativePageIdentity::default(),
            source_authority: self.authority.clone(),
            expected_frontier: frontier,
            next_frontier: frontier,
            terminal: false,
            accounting: GooseNativePageAccounting::default(),
            position: self.position,
            sessions: Vec::new(),
            events: Vec::new(),
            excluded_outputs: Vec::new(),
            rejections: Vec::new(),
        }
    }

    pub(super) fn hash_session(&mut self, session: &GooseNativeSession) {
        self.semantic_hasher.update(b"session");
        goose_hash_str(&mut self.semantic_hasher, &session.native_identity);
        goose_hash_session_row(&mut self.semantic_hasher, &session.row);
    }

    pub(super) fn hash_session_inventory(&mut self, sqlite_rowid: i64, native_identity: &str) {
        goose_hash_i64(&mut self.session_inventory_hasher, sqlite_rowid);
        goose_hash_str(&mut self.session_inventory_hasher, native_identity);
        if self.session_identity_samples.len() < GOOSE_SESSION_IDENTITY_SAMPLE_LIMIT {
            let mut sample_hasher = Sha256::new();
            sample_hasher.update(GOOSE_SESSION_SAMPLE_DIGEST_DOMAIN);
            goose_hash_str(&mut sample_hasher, native_identity);
            self.session_identity_samples
                .push(goose_hex_digest(sample_hasher.finalize().into()));
        }
    }

    pub(super) fn hash_message_header(
        &mut self,
        message: &GooseScannedMessage,
        disposition: &[u8],
    ) {
        self.semantic_hasher.update(b"message");
        goose_hash_bytes(&mut self.semantic_hasher, disposition);
        goose_hash_i64(&mut self.semantic_hasher, message.sqlite_rowid);
        goose_hash_i64(&mut self.semantic_hasher, message.native_order);
        goose_hash_str(&mut self.semantic_hasher, &message.native_identity);
        goose_hash_str(&mut self.semantic_hasher, &message.session_identity);
        goose_hash_str(&mut self.semantic_hasher, &message.role);
    }

    pub(super) fn hash_rejection(&mut self, rejection: &GooseNativeRejection) {
        self.semantic_hasher.update(b"rejection");
        goose_hash_i64(&mut self.semantic_hasher, rejection.sqlite_rowid);
        goose_hash_str(&mut self.semantic_hasher, &rejection.native_identity);
        goose_hash_str(&mut self.semantic_hasher, rejection.kind.as_str());
    }
}

pub(super) fn goose_rejection_kind(
    disposition: GooseMessageCellDisposition,
) -> Result<GooseNativeRejectionKind> {
    match disposition {
        GooseMessageCellDisposition::MalformedJson => Ok(GooseNativeRejectionKind::MalformedJson),
        GooseMessageCellDisposition::UnsupportedJsonRoot => {
            Ok(GooseNativeRejectionKind::UnsupportedJsonRoot)
        }
        GooseMessageCellDisposition::NonObjectBlock => Ok(GooseNativeRejectionKind::NonObjectBlock),
        GooseMessageCellDisposition::UnknownBlockType => {
            Ok(GooseNativeRejectionKind::UnknownBlockType)
        }
        GooseMessageCellDisposition::DuplicateBlockType => {
            Ok(GooseNativeRejectionKind::DuplicateBlockType)
        }
        GooseMessageCellDisposition::OversizedRetainedContent => {
            Ok(GooseNativeRejectionKind::OversizedRetainedContent)
        }
        GooseMessageCellDisposition::MissingSession => Ok(GooseNativeRejectionKind::MissingSession),
        GooseMessageCellDisposition::UnsupportedStorageClass => {
            Ok(GooseNativeRejectionKind::UnsupportedStorageClass)
        }
        GooseMessageCellDisposition::Retained
        | GooseMessageCellDisposition::OutputSuccess
        | GooseMessageCellDisposition::OutputFailure
        | GooseMessageCellDisposition::OutputTimeout
        | GooseMessageCellDisposition::OutputUnknown => Err(CaptureError::SystemInvariant(
            "Goose retained/output disposition is not a rejection",
        )),
    }
}

pub(super) fn project_goose_pro_output(
    output: GooseScannedOutput,
    locator: OutputSourceLocator,
) -> std::result::Result<ProOutputObservation, (GooseNativeProRejectionKind, String)> {
    let Some(raw_content) = output.content_json.as_deref() else {
        return Err((
            GooseNativeProRejectionKind::OversizedOutput,
            format!(
                "Goose output {} exceeds the bounded Pro replay page",
                output.native_identity
            ),
        ));
    };
    let content: serde_json::Value = serde_json::from_str(raw_content).map_err(|error| {
        (
            GooseNativeProRejectionKind::MalformedOutput,
            format!(
                "Goose output {} changed classification while parsing: {error}",
                output.native_identity
            ),
        )
    })?;
    let projection = goose_output_projection(&content);
    if projection.outcome.outcome != output.outcome {
        return Err((
            GooseNativeProRejectionKind::MalformedOutput,
            format!(
                "Goose output {} disagrees between SQLite and Rust outcome classification",
                output.native_identity
            ),
        ));
    }
    let occurred_at_unix_ms = output
        .created_timestamp
        .and_then(|seconds| seconds.checked_mul(1_000))
        .or_else(|| {
            output.timestamp.as_deref().and_then(|timestamp| {
                let timestamp = timestamp.trim();
                (!timestamp.is_empty()).then(|| {
                    goose_timestamp(Some(timestamp), DateTime::<Utc>::UNIX_EPOCH).timestamp_millis()
                })
            })
        });
    let native_record_identity = output.provider_message_identity;
    Ok(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "goose:{}:{}:output",
                output.session_identity, native_record_identity
            ),
            native_sequence: output.source_record_ordinal,
            native_record_id: Some(native_record_identity),
            source_record_ordinal: Some(output.source_record_ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms,
        associations: OutputAssociations {
            direct_session_id: output.session_identity.clone(),
            root_session_id: output.session_identity.clone(),
            parent_session_id: None,
            provider_session_id: Some(output.session_identity),
            agent_id: None,
            repository: None,
        },
        call_id: projection.call_id,
        command: None,
        outcome: projection.outcome,
        locator,
        content: goose_normalized_result_content(&content)
            .unwrap_or_default()
            .into_bytes(),
    })
}

pub(super) fn goose_output_locator(sqlite_rowid: i64) -> Result<OutputSourceLocator> {
    let (kind, payload) = goose_message_locator(sqlite_rowid);
    Ok(OutputSourceLocator {
        version: 1,
        kind: kind.to_owned(),
        payload,
    })
}

pub(super) fn goose_output_outcome_code(outcome: OutputOutcome) -> i64 {
    match outcome {
        OutputOutcome::Success => 1,
        OutputOutcome::Failure => 2,
        OutputOutcome::Timeout => 3,
        OutputOutcome::Unknown => 4,
    }
}

pub(super) fn goose_session_content_digest(session: &GooseNativeSession) -> String {
    let mut hasher = Sha256::new();
    hasher.update(GOOSE_SESSION_DIGEST_DOMAIN);
    goose_hash_str(&mut hasher, &session.native_identity);
    goose_hash_session_row(&mut hasher, &session.row);
    goose_hex_digest(hasher.finalize().into())
}

pub(super) fn goose_event_content_digest(event: &GooseNativeEvent) -> String {
    let mut hasher = Sha256::new();
    hasher.update(GOOSE_EVENT_DIGEST_DOMAIN);
    goose_hash_i64(&mut hasher, event.native_order);
    goose_hash_str(&mut hasher, &event.native_identity);
    goose_hash_str(&mut hasher, &event.provider_message_identity);
    goose_hash_str(&mut hasher, &event.session_identity);
    goose_hash_str(&mut hasher, &event.role);
    goose_hash_str(&mut hasher, &event.content.to_string());
    goose_hash_str(&mut hasher, &event.searchable_text);
    goose_hash_optional_i64(&mut hasher, event.created_timestamp);
    goose_hash_optional_str(&mut hasher, event.timestamp.as_deref());
    goose_hash_optional_str(&mut hasher, event.tokens_json.as_deref());
    goose_hash_optional_str(&mut hasher, event.metadata_json.as_deref());
    for touch in &event.file_touches {
        goose_hash_str(&mut hasher, &touch.path);
        goose_hash_optional_str(&mut hasher, touch.old_path.as_deref());
        goose_hash_str(&mut hasher, touch.evidence);
    }
    goose_hex_digest(hasher.finalize().into())
}

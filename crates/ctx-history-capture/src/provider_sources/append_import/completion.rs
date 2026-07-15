fn unexpected_post_commit_checkpoint_failure(
    error: &CaptureError,
) -> ProviderJsonlReplacementReason {
    match error {
        CaptureError::Io(_) | CaptureError::SystemIo { .. } => {
            ProviderJsonlReplacementReason::CheckpointUnexpectedIo
        }
        _ => ProviderJsonlReplacementReason::CheckpointUnexpectedPermanentFailure,
    }
}

#[derive(Debug)]
enum ValidatedAdapterResumeState {
    None,
    Claude(ClaudeProjectsJsonlResumeState),
    Codex(CodexSessionJsonlResumeState),
    Tabnine(TabnineJsonlResumeState),
}

fn validate_adapter_resume_state(
    provider: CaptureProvider,
    inventory_source_format: &str,
    is_replacement: bool,
    resume_state: Option<&ProviderJsonlResumeState>,
) -> std::result::Result<ValidatedAdapterResumeState, ProviderJsonlReplacementReason> {
    if is_replacement {
        return Ok(ValidatedAdapterResumeState::None);
    }
    match (provider, inventory_source_format, resume_state) {
        (CaptureProvider::Pi, "pi_session_jsonl", None) => Ok(ValidatedAdapterResumeState::None),
        (CaptureProvider::Pi, "pi_session_jsonl", Some(_)) => {
            Err(ProviderJsonlReplacementReason::AdapterResumeStateIncompatible)
        }
        (CaptureProvider::Codex, "codex_session_jsonl_tree" | "codex_session_jsonl", None) => {
            Err(ProviderJsonlReplacementReason::AdapterResumeStateMissing)
        }
        (
            CaptureProvider::Codex,
            "codex_session_jsonl_tree" | "codex_session_jsonl",
            Some(ProviderJsonlResumeState::CodexSession(state)),
        ) => {
            ProviderJsonlResumeState::CodexSession(state.clone()).validate()?;
            Ok(ValidatedAdapterResumeState::Codex(state.clone()))
        }
        (CaptureProvider::Codex, "codex_session_jsonl_tree" | "codex_session_jsonl", Some(_)) => {
            Err(ProviderJsonlReplacementReason::AdapterResumeStateIncompatible)
        }
        (CaptureProvider::Claude, "claude_projects_jsonl_tree", None)
        | (CaptureProvider::Tabnine, "tabnine_cli_chat_recording_jsonl", None) => {
            Err(ProviderJsonlReplacementReason::AdapterResumeStateMissing)
        }
        (
            CaptureProvider::Claude,
            "claude_projects_jsonl_tree",
            Some(ProviderJsonlResumeState::ClaudeProjects(state)),
        ) => {
            ProviderJsonlResumeState::ClaudeProjects(state.clone()).validate()?;
            Ok(ValidatedAdapterResumeState::Claude(state.clone()))
        }
        (
            CaptureProvider::Tabnine,
            "tabnine_cli_chat_recording_jsonl",
            Some(ProviderJsonlResumeState::TabnineCli(state)),
        ) => {
            ProviderJsonlResumeState::TabnineCli(state.clone()).validate()?;
            Ok(ValidatedAdapterResumeState::Tabnine(state.clone()))
        }
        (CaptureProvider::Claude, "claude_projects_jsonl_tree", Some(_))
        | (CaptureProvider::Tabnine, "tabnine_cli_chat_recording_jsonl", Some(_)) => {
            Err(ProviderJsonlReplacementReason::AdapterResumeStateIncompatible)
        }
        _ => Err(ProviderJsonlReplacementReason::AdapterResumeStateIncompatible),
    }
}

#[derive(Debug, Clone)]
struct AuthoritativeSessionScan {
    identity_changed: bool,
    deferred_partial: bool,
    has_real_message: bool,
    normalization_header: Option<Value>,
    earliest_started_at: Option<DateTime<Utc>>,
}

fn scan_authoritative_session_ids(
    reader: &mut ProviderJsonlReader,
    provider: CaptureProvider,
    expected_session_id: Option<&str>,
    context: &ProviderAdapterContext,
) -> Result<AuthoritativeSessionScan> {
    let mut identity_changed = false;
    let mut has_real_message = false;
    let mut normalization_header = None;
    let mut earliest_started_at: Option<DateTime<Utc>> = None;
    let mut line = Vec::new();
    loop {
        let line_number = match reader.read_record(&mut line)? {
            ProviderJsonlRecordRead::Eof | ProviderJsonlRecordRead::DeferredPartial { .. } => break,
            ProviderJsonlRecordRead::Oversized { .. } => continue,
            ProviderJsonlRecordRead::Record { line_number, .. } => {
                usize::try_from(line_number).unwrap_or(usize::MAX)
            }
        };
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let row_session_id = match provider {
            CaptureProvider::Claude => {
                if normalization_header.is_none() {
                    normalization_header = Some(value.clone());
                }
                let parsed_timestamp = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
                    .map(|timestamp| timestamp.with_timezone(&Utc));
                let occurred_at = parsed_timestamp.unwrap_or(context.imported_at);
                earliest_started_at = Some(
                    earliest_started_at.map_or(occurred_at, |earliest| earliest.min(occurred_at)),
                );
                has_real_message |= claude_event(&value, line_number, occurred_at)
                    .as_ref()
                    .is_some_and(provider_event_is_real_conversation_message);
                claude_header_session_id(&value)
            }
            CaptureProvider::Tabnine => {
                if normalization_header.is_none()
                    && native_jsonl_header_session_id(CaptureProvider::Tabnine, &value).is_some()
                {
                    normalization_header = Some(value.clone());
                }
                let occurred_at = native_jsonl_timestamp(&value).unwrap_or(context.imported_at);
                has_real_message |= native_jsonl_event(
                    CaptureProvider::Tabnine,
                    TABNINE_CLI_SOURCE_FORMAT,
                    &value,
                    line_number,
                    occurred_at,
                )
                .as_ref()
                .is_some_and(provider_event_is_real_conversation_message);
                native_jsonl_header_session_id(CaptureProvider::Tabnine, &value)
            }
            _ => {
                return Err(CaptureError::SystemInvariant(
                    "authoritative session scan used for an unsupported provider",
                ));
            }
        };
        if let (Some(expected), Some(actual)) = (expected_session_id, row_session_id.as_deref()) {
            if actual != expected {
                identity_changed = true;
            }
        }
    }
    let deferred_partial = reader.has_deferred_partial();
    reader.freeze_at_current_complete_boundary();
    reader.restart_import_position()?;
    Ok(AuthoritativeSessionScan {
        identity_changed,
        deferred_partial,
        has_real_message,
        normalization_header,
        earliest_started_at,
    })
}

fn claude_header_session_id(value: &Value) -> Option<String> {
    value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}

struct PiSessionScan {
    additional_session_header: bool,
    deferred_partial: bool,
    has_real_message: bool,
    admission: PiSessionAdmission,
}

struct PiSessionAdmission {
    connection: Connection,
    _scratch: CaptureScratchSpace,
}

impl PiSessionAdmission {
    fn new() -> Result<Self> {
        let scratch = CaptureScratchSpace::create("pi-append-admission")?;
        drop(scratch.create_file("pi-session-admission.sqlite")?);
        let connection = Connection::open(scratch.path().join("pi-session-admission.sqlite"))?;
        connection.execute_batch(
            "CREATE TABLE session_admission (
                 provider_session_id TEXT PRIMARY KEY NOT NULL,
                 has_capture INTEGER NOT NULL DEFAULT 0 CHECK (has_capture IN (0, 1)),
                 has_real_message INTEGER NOT NULL DEFAULT 0 CHECK (has_real_message IN (0, 1))
             ) WITHOUT ROWID;",
        )?;
        Ok(Self {
            connection,
            _scratch: scratch,
        })
    }

    fn observe_session(&self, provider_session_id: &str) -> Result<()> {
        self.connection.execute(
            "INSERT OR IGNORE INTO session_admission (provider_session_id) VALUES (?1)",
            params![provider_session_id],
        )?;
        Ok(())
    }

    fn mark_real(&self, provider_session_id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE session_admission SET has_real_message = 1 WHERE provider_session_id = ?1",
            params![provider_session_id],
        )?;
        Ok(())
    }

    fn mark_capture(&self, provider_session_id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE session_admission SET has_capture = 1 WHERE provider_session_id = ?1",
            params![provider_session_id],
        )?;
        Ok(())
    }

    fn admits(&self, provider_session_id: &str) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT has_real_message FROM session_admission WHERE provider_session_id = ?1",
            params![provider_session_id],
            |row| row.get(0),
        )?)
    }

    fn rejected_session_count(&self) -> Result<usize> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM session_admission
             WHERE has_capture = 1 AND has_real_message = 0",
            [],
            |row| row.get(0),
        )?)
    }
}

fn scan_pi_session(
    reader: &mut ProviderJsonlReader,
    context: &ProviderAdapterContext,
    is_replacement: bool,
) -> Result<std::result::Result<PiSessionScan, ProviderJsonlReplacementReason>> {
    let admission = PiSessionAdmission::new()?;
    let mut header = None;
    let mut header_seen = !is_replacement;
    let mut additional_session_header = false;
    let mut has_real_message = false;
    let mut line = Vec::new();
    loop {
        let line_number = match reader.read_record(&mut line)? {
            ProviderJsonlRecordRead::Eof | ProviderJsonlRecordRead::DeferredPartial { .. } => break,
            ProviderJsonlRecordRead::Oversized { .. } => continue,
            ProviderJsonlRecordRead::Record { line_number, .. } => {
                usize::try_from(line_number).unwrap_or(usize::MAX)
            }
        };
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("session") {
            if header_seen {
                if !is_replacement {
                    return Ok(Err(ProviderJsonlReplacementReason::AdditionalSessionHeader));
                }
                additional_session_header = true;
            }
            if let Ok(parsed) = pi_session_header(value) {
                admission.observe_session(&parsed.id)?;
                header_seen = true;
                header = Some(parsed);
            }
            continue;
        }
        if let Some(header) = header.as_ref() {
            if let Ok(capture) = pi_session_capture(header, Some(value), line_number, context) {
                admission.mark_capture(&header.id)?;
                if capture
                    .event
                    .as_ref()
                    .is_some_and(pi_event_has_real_message_content)
                {
                    has_real_message = true;
                    admission.mark_real(&header.id)?;
                }
            }
        }
    }
    let deferred_partial = reader.has_deferred_partial();
    reader.freeze_at_current_complete_boundary();
    reader.restart_import_position()?;
    Ok(Ok(PiSessionScan {
        additional_session_header,
        deferred_partial,
        has_real_message,
        admission,
    }))
}

pub fn provider_canonical_material_source_format(
    provider: CaptureProvider,
    inventory_source_format: &str,
) -> Option<&'static str> {
    canonical_provider_material_source_format(provider, inventory_source_format)
}

fn finish_import(
    summary: ProviderImportSummary,
    checkpoint_decision: std::result::Result<
        ProviderJsonlAppendCheckpoint,
        ProviderJsonlReplacementReason,
    >,
    resume_state: Option<ProviderJsonlResumeState>,
    certification_failure: Option<ProviderJsonlReplacementReason>,
) -> ProviderAppendFileImportDecision {
    let mut checkpoint = match checkpoint_decision {
        Ok(checkpoint) => checkpoint,
        Err(reason) => {
            return ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(
                ProviderAppendFileImportWithoutCheckpoint { summary, reason },
            );
        }
    };
    if let Some(reason) = certification_failure {
        return ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(
            ProviderAppendFileImportWithoutCheckpoint { summary, reason },
        );
    }
    checkpoint.resume_state = resume_state;
    ProviderAppendFileImportDecision::Imported(ProviderAppendFileImportResult {
        summary,
        checkpoint,
    })
}

fn normalized_import_options(history_record_id: Option<Uuid>) -> NormalizedProviderImportOptions {
    NormalizedProviderImportOptions {
        history_record_id,
        persist_cursors: true,
        wrap_transaction: true,
        fast_event_inserts: true,
    }
}

fn discard_pi_no_real_batch(
    normalization: crate::ProviderNormalizationResult,
    summary: &mut ProviderImportSummary,
) {
    summary.merge(normalization.summary);
}

fn filter_pi_replacement_batch(
    mut normalization: crate::ProviderNormalizationResult,
    admission: &PiSessionAdmission,
) -> Result<crate::ProviderNormalizationResult> {
    let mut captures = Vec::with_capacity(normalization.captures.len());
    for capture in normalization.captures {
        if admission.admits(&capture.1.session.provider_session_id)? {
            captures.push(capture);
        } else {
            normalization.summary.skipped += 1;
            if capture.1.event.is_some() {
                normalization.summary.skipped_events += 1;
            }
        }
    }
    normalization.captures = captures;

    let mut files_touched = Vec::with_capacity(normalization.files_touched.len());
    for file in normalization.files_touched {
        if admission.admits(&file.1.provider_session_id)? {
            files_touched.push(file);
        } else {
            normalization.summary.skipped += 1;
        }
    }
    normalization.files_touched = files_touched;
    Ok(normalization)
}

fn discard_single_session_no_real_batch(
    mut normalization: crate::ProviderNormalizationResult,
    summary: &mut ProviderImportSummary,
    saw_capture: &mut bool,
) {
    if !normalization.captures.is_empty() {
        *saw_capture = true;
    }
    normalization.summary.skipped += normalization.captures.len();
    normalization.summary.skipped_events += normalization
        .captures
        .iter()
        .filter(|(_, capture)| capture.event.is_some())
        .count();
    normalization.summary.skipped += normalization.files_touched.len();
    summary.merge(normalization.summary);
}

fn finish_single_session_no_real_summary(summary: &mut ProviderImportSummary, saw_capture: bool) {
    if saw_capture {
        summary.skipped_sessions += 1;
    }
    if summary.failed == 0 {
        summary.failed += 1;
        summary.sample_failure(ProviderImportFailure {
            line: 0,
            error: "provider source contained no real conversation message".to_owned(),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn import_codex_session_file(
    reader: &mut ProviderJsonlReader,
    source_format: &str,
    store: &mut Store,
    context: &ProviderAdapterContext,
    history_record_id: Option<Uuid>,
    is_replacement: bool,
    append_bootstrap: Option<CodexSessionHeader>,
    resume_state: CodexSessionJsonlResumeState,
) -> Result<CodexSessionFileImport> {
    let bootstrap = if is_replacement {
        let scan = codex_session_reader_conversation_scan(reader)?;
        if !scan.has_real_conversation && reader.has_deferred_partial() {
            return Ok(CodexSessionFileImport::DeferredPartial);
        }
        if !scan.has_real_conversation
            && !scan.has_malformed_header
            && !scan.has_malformed_relevant_line
        {
            let mut summary = ProviderImportSummary::default();
            if scan.oversized_required_header {
                summary.skipped += 1;
                summary.skipped_sessions += 1;
            } else if scan.oversized_events > 0 {
                summary.skipped += scan.oversized_events;
                summary.skipped_events += scan.oversized_events;
            } else {
                summary.failed += 1;
                summary.sample_failure(ProviderImportFailure {
                    line: 0,
                    error: "codex session JSONL contained no real message content".to_owned(),
                });
            }
            return Ok(CodexSessionFileImport::Imported {
                summary,
                boundary: CodexSessionSemanticBoundary {
                    committed_offset: reader.committed_offset(),
                    complete_line_count: reader.complete_line_count(),
                    additional_session_header: scan.has_additional_session_header,
                    resume_state: CodexSessionJsonlResumeState::default(),
                },
            });
        }
        reader.restart_append_capable_replacement()?;
        None
    } else {
        let header = append_bootstrap.ok_or(CaptureError::SystemInvariant(
            "append Codex import is missing its certified row-one header",
        ))?;
        if codex_session_reader_has_additional_header(reader)? {
            return Ok(CodexSessionFileImport::ReplacementRequired(
                ProviderJsonlReplacementReason::AdditionalSessionHeader,
            ));
        }
        reader.freeze_at_current_complete_boundary();
        reader.restart_import_position()?;
        Some(header)
    };

    let path = reader.path().to_path_buf();
    match import_codex_session_reader_bounded(
        &path,
        reader,
        bootstrap,
        resume_state,
        source_format,
        store,
        history_record_id,
        context,
        !is_replacement,
    )? {
        CodexSessionBoundedImport::Imported { summary, boundary } => {
            Ok(CodexSessionFileImport::Imported { summary, boundary })
        }
        CodexSessionBoundedImport::ReplacementRequired(reason) => {
            Ok(CodexSessionFileImport::ReplacementRequired(reason))
        }
    }
}

enum CodexSessionFileImport {
    Imported {
        summary: ProviderImportSummary,
        boundary: CodexSessionSemanticBoundary,
    },
    DeferredPartial,
    ReplacementRequired(ProviderJsonlReplacementReason),
}

fn read_authoritative_codex_header(
    reader: &mut ProviderJsonlReader,
) -> Result<std::result::Result<CodexSessionHeader, ProviderJsonlReplacementReason>> {
    let value = match read_authoritative_first_json_record(reader)? {
        Ok(value) => value,
        Err(reason) => return Ok(Err(reason)),
    };
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return Ok(Err(
            ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid,
        ));
    }
    Ok(codex_session_header(value)
        .map_err(|_| ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid))
}

fn read_authoritative_pi_header(
    reader: &mut ProviderJsonlReader,
) -> Result<std::result::Result<PiSessionHeader, ProviderJsonlReplacementReason>> {
    let value = match read_authoritative_first_json_record(reader)? {
        Ok(value) => value,
        Err(reason) => return Ok(Err(reason)),
    };
    if value.get("type").and_then(Value::as_str) != Some("session") {
        return Ok(Err(
            ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid,
        ));
    }
    Ok(pi_session_header(value)
        .map_err(|_| ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid))
}

fn read_authoritative_first_json_record(
    reader: &mut ProviderJsonlReader,
) -> Result<std::result::Result<Value, ProviderJsonlReplacementReason>> {
    let first_record = match reader.read_first_complete_record() {
        Ok(record) => record,
        Err(CaptureError::InvalidPayload(_)) => {
            return Ok(Err(
                ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid,
            ));
        }
        Err(error) => return Err(error),
    };
    let Some(line) = first_record else {
        return Ok(Err(
            ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid,
        ));
    };
    match serde_json::from_slice(&line) {
        Ok(value) => Ok(Ok(value)),
        Err(_) => Ok(Err(
            ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid,
        )),
    }
}

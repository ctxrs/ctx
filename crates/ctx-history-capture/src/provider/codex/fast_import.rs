use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, Event, EventType, ProviderEventEnvelope, ProviderSourceTrust,
};
use ctx_history_store::{EventSearchBulkMaintenanceOutcome, Store};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::compute_payload_hash;
use crate::provider::importer::{
    provider_command_run_from_event, provider_event_import_identity, provider_import_session_uuid,
    provider_session_uuid, provider_source_identity, provider_source_root,
    resolve_pending_provider_edges_batched, validate_provider_event_for_import,
    ProviderCommandRunInput, ProviderImportTransaction, ProviderImportTransactionStep,
};

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::provider::importer::{
    import_provider_capture_line, import_provider_file_touched_line, provider_scoped_source_uuid,
    provider_sync_metadata, ProviderImportCaches,
};
use crate::{
    CaptureError, CodexSessionImportOptions, CodexSessionImportProgress,
    CodexSessionJsonlResumeState, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportSummary, ProviderJsonlReader, ProviderJsonlRecordRead,
    ProviderJsonlReplacementReason, Result, CODEX_SESSION_SOURCE_FORMAT,
};

use crate::provider::codex::events::{
    codex_close_matching_tool_output_context, codex_session_capture_with_source_format,
    codex_session_header, codex_session_line_capture, codex_session_line_timestamp,
    CodexSessionHeader, CodexSessionLineContext, CodexToolCallContexts,
};
use crate::provider::codex::session::{
    codex_session_reader_conversation_scan, should_parse_codex_session_line,
    should_skip_codex_tool_output_line,
};

pub(crate) fn import_codex_session_paths_fast(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: CodexSessionImportOptions,
    skipped_by_bounds: usize,
) -> Result<ProviderImportSummary> {
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let import_result =
        import_codex_session_paths_fast_bounded(paths, store, &options, skipped_by_bounds);
    let finish_result = store.finish_event_search_bulk_mode(&bulk_guard);
    match (import_result, finish_result) {
        (Ok(summary), Ok(EventSearchBulkMaintenanceOutcome::Complete)) => Ok(summary),
        (Ok(mut summary), Ok(EventSearchBulkMaintenanceOutcome::Pending)) => {
            summary.push_maintenance_warning(
                crate::ProviderImportMaintenanceKind::EventSearchFinalizationPending,
                "event search maintenance remains queued",
            );
            Ok(summary)
        }
        (Ok(mut summary), Err(error)) => {
            summary.push_maintenance_warning(
                crate::ProviderImportMaintenanceKind::EventSearchFinalization,
                error.to_string(),
            );
            Ok(summary)
        }
        (Err(err), _) => Err(err),
    }
}

fn import_codex_session_paths_fast_bounded(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: &CodexSessionImportOptions,
    skipped_by_bounds: usize,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    summary.skipped_sessions += skipped_by_bounds;
    summary.skipped += skipped_by_bounds;
    let mut caches = ProviderImportCaches::default();
    let total_files = paths.len();
    let total_bytes = codex_session_paths_total_bytes(&paths);
    let mut completed_files = 0usize;
    let mut completed_bytes = 0u64;
    report_codex_import_progress(
        options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &summary,
        false,
    );

    let mut transaction = ProviderImportTransaction::begin_bounded(store, !paths.is_empty())?;
    let import_result = (|| -> Result<()> {
        for path in paths {
            let file_bytes = fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            import_codex_session_path_fast(
                &path,
                store,
                options,
                &mut summary,
                &mut caches,
                &mut transaction,
            )?;
            completed_files += 1;
            completed_bytes = completed_bytes.saturating_add(file_bytes);
            report_codex_import_progress(
                options,
                total_files,
                total_bytes,
                completed_files,
                completed_bytes,
                &summary,
                false,
            );
            if summary.requires_maintenance() {
                break;
            }
        }

        resolve_pending_provider_edges_batched(store, &mut summary, &mut caches, &mut transaction)?;
        transaction.commit(store)?;
        Ok(())
    })();
    if let Err(err) = import_result {
        transaction.rollback(store);
        if matches!(err, crate::CaptureError::CommittedImportMaintenance)
            || transaction.record_interruption_after_commit(&err)
        {
            transaction.apply_maintenance_warning(&mut summary);
            return Ok(summary);
        }
        return Err(err);
    }
    report_codex_import_progress(
        options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &summary,
        true,
    );
    Ok(summary)
}
pub(crate) fn codex_session_paths_total_bytes(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|path| fs::metadata(path).ok())
        .fold(0u64, |total, metadata| total.saturating_add(metadata.len()))
}
pub(crate) fn report_codex_import_progress(
    options: &CodexSessionImportOptions,
    total_files: usize,
    total_bytes: u64,
    completed_files: usize,
    completed_bytes: u64,
    summary: &ProviderImportSummary,
    done: bool,
) {
    let Some(callback) = &options.progress else {
        return;
    };
    callback(CodexSessionImportProgress {
        source_path: options.source_path.clone(),
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        imported_sessions: summary.imported_sessions,
        imported_events: summary.imported_events,
        imported_edges: summary.imported_edges,
        skipped: summary.skipped,
        failed: summary.failed,
        done,
    });
}
pub(crate) fn import_codex_session_path_fast(
    path: &Path,
    store: &mut Store,
    options: &CodexSessionImportOptions,
    summary: &mut ProviderImportSummary,
    caches: &mut ProviderImportCaches,
    transaction: &mut ProviderImportTransaction,
) -> Result<()> {
    ensure_regular_provider_transcript_file(path)?;
    let mut reader = ProviderJsonlReader::open_replacement(path)?;
    let conversation_scan = codex_session_reader_conversation_scan(&mut reader)?;
    if !conversation_scan.has_real_conversation
        && !conversation_scan.has_malformed_header
        && !conversation_scan.has_malformed_relevant_line
    {
        if conversation_scan.oversized_required_header {
            summary.skipped += 1;
            summary.skipped_sessions += 1;
            return Ok(());
        }
        if conversation_scan.oversized_events > 0 {
            summary.skipped = summary
                .skipped
                .saturating_add(conversation_scan.oversized_events);
            summary.skipped_events = summary
                .skipped_events
                .saturating_add(conversation_scan.oversized_events);
            return Ok(());
        }
        summary.failed += 1;
        summary.sample_failure(ProviderImportFailure {
            line: 0,
            error: codex_session_file_failure(
                path,
                "codex session JSONL contained no real message content",
            ),
        });
        return Ok(());
    }
    reader.restart_import_position()?;
    let context = ProviderAdapterContext {
        machine_id: options.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        source_root: options.source_path.clone(),
        imported_at: options.imported_at,
    };
    match import_codex_session_reader_fast(
        path,
        &mut reader,
        None,
        CodexSessionJsonlResumeState::default(),
        CODEX_SESSION_SOURCE_FORMAT,
        store,
        options.history_record_id,
        &context,
        summary,
        caches,
        transaction,
        false,
    )? {
        CodexSessionReaderDecision::Imported(_) => Ok(()),
        CodexSessionReaderDecision::ReplacementRequired(_) => {
            Err(crate::CaptureError::SystemInvariant(
                "whole-replacement Codex import requested replacement",
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn import_codex_session_reader_fast(
    path: &Path,
    reader: &mut ProviderJsonlReader,
    bootstrap_header: Option<CodexSessionHeader>,
    resume_state: CodexSessionJsonlResumeState,
    source_format: &str,
    store: &mut Store,
    history_record_id: Option<Uuid>,
    context: &ProviderAdapterContext,
    summary: &mut ProviderImportSummary,
    caches: &mut ProviderImportCaches,
    transaction: &mut ProviderImportTransaction,
    reject_additional_session_header: bool,
) -> Result<CodexSessionReaderDecision> {
    let import_options = NormalizedProviderImportOptions {
        history_record_id,
        persist_cursors: false,
        wrap_transaction: false,
        fast_event_inserts: true,
    };
    let raw_source_path = context
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());

    let mut header = bootstrap_header;
    let mut header_persisted = false;
    let mut call_contexts = CodexToolCallContexts::from_resume_state(resume_state);
    let mut semantic_boundary = CodexSessionSemanticBoundary {
        committed_offset: reader.committed_offset(),
        complete_line_count: reader.complete_line_count(),
        additional_session_header: false,
        resume_state: call_contexts.resume_state(),
    };
    let mut line = Vec::new();
    loop {
        let line_number = match reader.read_record(&mut line)? {
            ProviderJsonlRecordRead::Eof => break,
            ProviderJsonlRecordRead::Record {
                line_number: current,
                ..
            } => usize::try_from(current).unwrap_or(usize::MAX),
            ProviderJsonlRecordRead::Oversized { .. } => {
                summary.skipped += 1;
                if header.is_none() {
                    summary.skipped_sessions += 1;
                    advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
                    return Ok(CodexSessionReaderDecision::Imported(semantic_boundary));
                }
                summary.skipped_events += 1;
                advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
                continue;
            }
            ProviderJsonlRecordRead::DeferredPartial { .. } => break,
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
            continue;
        }
        if !should_parse_codex_session_line(&line) {
            advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
            continue;
        }
        let skip_tool_output = should_skip_codex_tool_output_line(&line);

        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(err) => {
                summary.failed += 1;
                summary.sample_failure(ProviderImportFailure {
                    line: line_number,
                    error: codex_session_file_failure(path, err.to_string()),
                });
                advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
                continue;
            }
        };
        let entry_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if entry_type == "session_meta" {
            if reject_additional_session_header && header.is_some() {
                return Ok(CodexSessionReaderDecision::ReplacementRequired(
                    ProviderJsonlReplacementReason::AdditionalSessionHeader,
                ));
            }
            if header.is_some() {
                semantic_boundary.additional_session_header = true;
            }
            match codex_session_header(value) {
                Ok(parsed) => {
                    call_contexts.clear();
                    header = Some(parsed);
                    header_persisted = false;
                }
                Err(err) => {
                    summary.failed += 1;
                    summary.sample_failure(ProviderImportFailure {
                        line: line_number,
                        error: codex_session_file_failure(path, err.to_string()),
                    });
                }
            }
            advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
            continue;
        }

        let Some(header) = header.as_ref() else {
            summary.failed += 1;
            summary.sample_failure(ProviderImportFailure {
                line: line_number,
                error: codex_session_file_failure(
                    path,
                    "codex session entry appeared before session_meta",
                ),
            });
            advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
            continue;
        };
        let occurred_at = match codex_session_line_timestamp(&value, header.timestamp) {
            Ok(occurred_at) => occurred_at,
            Err(err) => {
                codex_close_matching_tool_output_context(&value, &mut call_contexts);
                summary.failed += 1;
                summary.sample_failure(ProviderImportFailure {
                    line: line_number,
                    error: codex_session_file_failure(path, err.to_string()),
                });
                advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
                continue;
            }
        };
        let mut line_capture = codex_session_line_capture(
            header,
            &value,
            &mut call_contexts,
            CodexSessionLineContext {
                line_number,
                occurred_at,
                raw_source_path: raw_source_path.as_deref(),
                source_root: context.source_root_display().as_deref(),
                source_format,
            },
        );
        if skip_tool_output {
            summary.skipped += 1;
            summary.skipped_events += 1;
            line_capture.event = None;
        }
        let event = match line_capture.event.take() {
            Some(event) if event.event_type == EventType::Notice => {
                summary.skipped += 1;
                summary.skipped_events += 1;
                None
            }
            Some(event) => {
                if let Err(err) = validate_provider_event_for_import(&event) {
                    summary.failed += 1;
                    summary.sample_failure(ProviderImportFailure {
                        line: line_number,
                        error: codex_session_file_failure(path, err.to_string()),
                    });
                    advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
                    continue;
                }
                Some(event)
            }
            None => None,
        };
        let has_content = event.is_some() || !line_capture.files_touched.is_empty();
        if has_content
            && transaction.prepare_unit(store, line.len())? == ProviderImportTransactionStep::Halted
        {
            transaction.apply_maintenance_warning(summary);
            return Ok(CodexSessionReaderDecision::Imported(semantic_boundary));
        }
        if let Some(event) = event {
            if !header_persisted {
                summary.merge(import_codex_session_header_fast(
                    store,
                    header,
                    context,
                    &import_options,
                    line_number,
                    caches,
                    source_format,
                )?);
                header_persisted = true;
            }
            let source_root = context.source_root_display();
            let line_summary = import_codex_provider_event_fast(
                store,
                header,
                &event,
                history_record_id,
                line_number,
                context.imported_at,
                raw_source_path.as_deref(),
                source_root.as_deref(),
                source_format,
            )?;
            summary.merge(line_summary);
        }
        if !line_capture.files_touched.is_empty() && !header_persisted {
            summary.merge(import_codex_session_header_fast(
                store,
                header,
                context,
                &import_options,
                line_number,
                caches,
                source_format,
            )?);
            header_persisted = true;
        }
        for (_, file) in line_capture.files_touched {
            import_provider_file_touched_line(store, &file, &import_options)?;
            summary.accepted_content_records += 1;
        }
        if has_content {
            let step = transaction.record_unit(store, line.len())?;
            advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
            if step == ProviderImportTransactionStep::Halted {
                transaction.apply_maintenance_warning(summary);
                return Ok(CodexSessionReaderDecision::Imported(semantic_boundary));
            }
        } else {
            advance_codex_semantic_boundary(reader, &call_contexts, &mut semantic_boundary);
        }
    }
    Ok(CodexSessionReaderDecision::Imported(semantic_boundary))
}

pub(crate) enum CodexSessionReaderDecision {
    Imported(CodexSessionSemanticBoundary),
    ReplacementRequired(ProviderJsonlReplacementReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexSessionSemanticBoundary {
    pub(crate) committed_offset: u64,
    pub(crate) complete_line_count: u64,
    pub(crate) additional_session_header: bool,
    pub(crate) resume_state: CodexSessionJsonlResumeState,
}

fn advance_codex_semantic_boundary(
    reader: &ProviderJsonlReader,
    call_contexts: &CodexToolCallContexts,
    boundary: &mut CodexSessionSemanticBoundary,
) {
    boundary.committed_offset = reader.committed_offset();
    boundary.complete_line_count = reader.complete_line_count();
    boundary.resume_state = call_contexts.resume_state();
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn import_codex_session_reader_bounded(
    path: &Path,
    reader: &mut ProviderJsonlReader,
    bootstrap_header: Option<CodexSessionHeader>,
    resume_state: CodexSessionJsonlResumeState,
    source_format: &str,
    store: &mut Store,
    history_record_id: Option<Uuid>,
    context: &ProviderAdapterContext,
    reject_additional_session_header: bool,
) -> Result<CodexSessionBoundedImport> {
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let import_result = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut caches = ProviderImportCaches::default();
        let mut transaction = ProviderImportTransaction::begin_bounded(store, true)?;
        let result = (|| {
            let semantic_boundary = match import_codex_session_reader_fast(
                path,
                reader,
                bootstrap_header,
                resume_state,
                source_format,
                store,
                history_record_id,
                context,
                &mut summary,
                &mut caches,
                &mut transaction,
                reject_additional_session_header,
            )? {
                CodexSessionReaderDecision::Imported(boundary) => boundary,
                CodexSessionReaderDecision::ReplacementRequired(reason) => {
                    transaction.rollback(store);
                    return Ok(CodexSessionBoundedImport::ReplacementRequired(reason));
                }
            };
            let finish_materialization = (|| -> Result<()> {
                resolve_pending_provider_edges_batched(
                    store,
                    &mut summary,
                    &mut caches,
                    &mut transaction,
                )?;
                transaction.commit(store)?;
                Ok(())
            })();
            if let Err(error) = finish_materialization {
                transaction.rollback(store);
                if matches!(error, CaptureError::CommittedImportMaintenance)
                    || transaction.record_interruption_after_commit(&error)
                {
                    transaction.apply_maintenance_warning(&mut summary);
                    return Ok(CodexSessionBoundedImport::Imported {
                        summary,
                        boundary: semantic_boundary,
                    });
                }
                return Err(error);
            }
            transaction.apply_maintenance_warning(&mut summary);
            Ok(CodexSessionBoundedImport::Imported {
                summary,
                boundary: semantic_boundary,
            })
        })();
        if result.is_err() {
            transaction.rollback(store);
        }
        result
    })();
    let finish_result = store.finish_event_search_bulk_mode(&bulk_guard);
    match (import_result, finish_result) {
        (Ok(summary), Ok(EventSearchBulkMaintenanceOutcome::Complete)) => Ok(summary),
        (
            Ok(CodexSessionBoundedImport::Imported {
                mut summary,
                boundary,
            }),
            Ok(EventSearchBulkMaintenanceOutcome::Pending),
        ) => {
            summary.push_maintenance_warning(
                crate::ProviderImportMaintenanceKind::EventSearchFinalizationPending,
                "event search maintenance remains queued",
            );
            Ok(CodexSessionBoundedImport::Imported { summary, boundary })
        }
        (
            Ok(CodexSessionBoundedImport::ReplacementRequired(reason)),
            Ok(EventSearchBulkMaintenanceOutcome::Pending),
        ) => Ok(CodexSessionBoundedImport::ReplacementRequired(reason)),
        (
            Ok(CodexSessionBoundedImport::Imported {
                mut summary,
                boundary,
            }),
            Err(error),
        ) => {
            summary.push_maintenance_warning(
                crate::ProviderImportMaintenanceKind::EventSearchFinalization,
                error.to_string(),
            );
            Ok(CodexSessionBoundedImport::Imported { summary, boundary })
        }
        (Ok(CodexSessionBoundedImport::ReplacementRequired(reason)), Err(_)) => {
            Ok(CodexSessionBoundedImport::ReplacementRequired(reason))
        }
        (Err(err), _) => Err(err),
    }
}

pub(crate) enum CodexSessionBoundedImport {
    Imported {
        summary: ProviderImportSummary,
        boundary: CodexSessionSemanticBoundary,
    },
    ReplacementRequired(ProviderJsonlReplacementReason),
}

fn import_codex_session_header_fast(
    store: &mut Store,
    header: &CodexSessionHeader,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
    line_number: usize,
    caches: &mut ProviderImportCaches,
    source_format: &str,
) -> Result<ProviderImportSummary> {
    let capture = codex_session_capture_with_source_format(
        header,
        None,
        line_number,
        header.timestamp,
        context,
        source_format,
    );
    import_provider_capture_line(store, &capture, import_options, line_number, caches)
}

fn codex_session_file_failure(path: &Path, reason: impl AsRef<str>) -> String {
    format!("{}: {}", path.display(), reason.as_ref())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn import_codex_provider_event_fast(
    store: &mut Store,
    header: &CodexSessionHeader,
    event: &ProviderEventEnvelope,
    history_record_id: Option<Uuid>,
    line_number: usize,
    imported_at: DateTime<Utc>,
    raw_source_path: Option<&str>,
    source_root: Option<&str>,
    source_format: &str,
) -> Result<ProviderImportSummary> {
    validate_provider_event_for_import(event)?;
    let mut summary = ProviderImportSummary::default();
    let provider = CaptureProvider::Codex;
    let source_id =
        provider_scoped_source_uuid(provider, &header.id, source_format, raw_source_path);
    let source_root = provider_source_root(source_root, raw_source_path);
    let source_identity = provider_source_identity(
        provider,
        source_format,
        source_root.as_deref(),
        raw_source_path,
        None,
        &Value::Null,
    );
    let session_id = provider_import_session_uuid(
        store,
        provider,
        &header.id,
        source_id,
        source_identity.as_deref(),
    )?;
    let payload = event.payload.clone();
    let event_metadata = event.metadata.clone();
    let event_hash = event
        .provider_event_hash
        .clone()
        .unwrap_or(compute_payload_hash(&payload)?);
    let event_identity = provider_event_import_identity(
        store,
        provider,
        &header.id,
        source_id,
        event.provider_event_index,
        event.provider_event_index,
        &event_hash,
        None,
        session_id == provider_session_uuid(provider, &header.id),
    )?;
    let command_run = provider_command_run_from_event(ProviderCommandRunInput {
        provider,
        provider_session_id: &header.id,
        session_id,
        source_id,
        run_source_id: event_identity.run_source_id,
        history_record_id,
        event,
        payload: &payload,
        event_hash: &event_hash,
    })?;
    let normalized_event = Event {
        id: event_identity.id,
        seq: event_identity.seq,
        history_record_id,
        session_id: Some(session_id),
        run_id: command_run.as_ref().map(|run| run.id),
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": provider.as_str(),
            "provider_session_id": header.id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": event.artifacts,
            "body": payload,
        }),
        payload_blob_id: None,
        dedupe_key: Some(event_identity.dedupe_key),
        sync: provider_sync_metadata(
            event.fidelity,
            json!({
                "provider_session_id": header.id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "cursor": event.cursor,
                "source_format": source_format,
                "source_trust": ProviderSourceTrust::ProviderExport,
                "fixture_line": line_number,
                "imported_at": imported_at,
                "event_idempotency_key": event.idempotency_key,
                "metadata": event_metadata,
            }),
        ),
    };

    if let Some(run) = &command_run {
        store.insert_run_if_absent(run)?;
    }
    let inserted = store.insert_event_if_absent(&normalized_event)?;
    if inserted {
        summary.imported_events += 1;
        summary.imported += 1;
    } else {
        summary.skipped_events += 1;
        summary.skipped += 1;
    }
    summary.accepted_content_records += 1;
    Ok(summary)
}

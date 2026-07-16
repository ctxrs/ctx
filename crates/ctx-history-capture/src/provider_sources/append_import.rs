use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::{canonical_provider_material_source_format, CaptureProvider};
use ctx_history_store::Store;
use rusqlite::{params, Connection};
use serde_json::Value;
use uuid::Uuid;

use crate::common::scratch::CaptureScratchSpace;
use crate::provider::codex::events::{codex_session_header, CodexSessionHeader};
use crate::provider::codex::fast_import::{
    import_codex_session_reader_bounded, CodexSessionBoundedImport, CodexSessionSemanticBoundary,
};
use crate::provider::codex::session::{
    codex_session_reader_conversation_scan, codex_session_reader_has_additional_header,
};
use crate::provider::importer::{
    import_normalized_provider_capture_stream, provider_event_is_real_conversation_message,
};
use crate::provider::providers::claude::{claude_event, stream_claude_projects_jsonl_reader};
use crate::provider::providers::native_jsonl::{
    native_jsonl_event, native_jsonl_header_session_id, native_jsonl_header_start_time,
    native_jsonl_timestamp, stream_native_jsonl_session_reader, NativeJsonlStreamOptions,
};
use crate::provider::providers::pi::{
    pi_event_has_real_message_content, pi_session_capture, pi_session_header,
    stream_pi_session_jsonl_reader, PiSessionHeader,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderFileMutationContract, ProviderImportFailure, ProviderImportSummary,
    ProviderJsonlAppendCheckpoint, ProviderJsonlOpenDecision, ProviderJsonlOpenMode,
    ProviderJsonlReader, ProviderJsonlRecordRead, ProviderJsonlReplacementReason,
    ProviderJsonlResumeState, Result, TabnineJsonlResumeState, TABNINE_CLI_SOURCE_FORMAT,
};

use super::{ClaudeProjectsJsonlResumeState, CodexSessionJsonlResumeState};

use super::provider_file_mutation_contract;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAppendFileImportMode {
    AppendCapableReplacement,
    Append(ProviderAdmittedJsonlAppendCheckpoint),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAdmittedJsonlAppendCheckpoint {
    checkpoint: ProviderJsonlAppendCheckpoint,
}

impl ProviderAdmittedJsonlAppendCheckpoint {
    /// Reconstitutes a checkpoint only after the catalog confirms that it came
    /// from a successfully admitted replacement import.
    pub fn from_persisted_admitted_replacement(checkpoint: ProviderJsonlAppendCheckpoint) -> Self {
        Self { checkpoint }
    }

    pub fn checkpoint(&self) -> &ProviderJsonlAppendCheckpoint {
        &self.checkpoint
    }

    pub fn into_checkpoint(self) -> ProviderJsonlAppendCheckpoint {
        self.checkpoint
    }
}

#[derive(Debug, Clone)]
pub struct ProviderAppendFileImportOptions {
    pub machine_id: String,
    pub inventory_source_format: String,
    pub material_source_format: String,
    pub source_path: PathBuf,
    pub source_root: PathBuf,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub observed_size: u64,
    pub mode: ProviderAppendFileImportMode,
}

#[derive(Debug, Clone)]
pub struct ProviderAppendFileImportResult {
    pub summary: ProviderImportSummary,
    pub checkpoint: ProviderJsonlAppendCheckpoint,
}

#[derive(Debug, Clone)]
pub struct ProviderAppendFileImportWithoutCheckpoint {
    pub summary: ProviderImportSummary,
    pub reason: ProviderJsonlReplacementReason,
    pub source_prefix_sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ProviderAppendFileImportDecision {
    Imported(ProviderAppendFileImportResult),
    /// Materialization committed, but safe-checkpoint certification failed.
    /// Callers must persist no new checkpoint: append retains its prior one,
    /// while replacement remains uncheckpointed.
    ImportedWithoutCheckpoint(ProviderAppendFileImportWithoutCheckpoint),
    DeferredPartial,
    ReplacementRequired(ProviderJsonlReplacementReason),
}

pub fn import_append_capable_provider_file(
    provider: CaptureProvider,
    store: &mut Store,
    options: ProviderAppendFileImportOptions,
) -> Result<ProviderAppendFileImportDecision> {
    import_append_capable_provider_file_with_post_materialization(provider, store, options, |_| {})
}

fn import_append_capable_provider_file_with_post_materialization(
    provider: CaptureProvider,
    store: &mut Store,
    options: ProviderAppendFileImportOptions,
    post_materialization: impl FnOnce(&mut ProviderJsonlReader),
) -> Result<ProviderAppendFileImportDecision> {
    let inventory_source_format = options.inventory_source_format.as_str();
    let material_source_format = options.material_source_format.as_str();
    if provider_file_mutation_contract(provider, inventory_source_format)
        != ProviderFileMutationContract::AppendOnlyNewlineDelimited
    {
        return Err(CaptureError::InvalidPayload(format!(
            "provider inventory format {}:{inventory_source_format} is not append-capable",
            provider.as_str()
        )));
    }
    let expected_material_source_format =
        provider_canonical_material_source_format(provider, inventory_source_format).ok_or(
            CaptureError::SystemInvariant(
                "append-capable provider contract has no canonical material format",
            ),
        )?;
    if material_source_format != expected_material_source_format {
        return Err(CaptureError::InvalidPayload(format!(
            "provider inventory format {}:{inventory_source_format} requires material format {expected_material_source_format}, not {material_source_format}",
            provider.as_str()
        )));
    }

    let is_replacement = matches!(
        &options.mode,
        ProviderAppendFileImportMode::AppendCapableReplacement
    );
    let prior_resume_state = match &options.mode {
        ProviderAppendFileImportMode::AppendCapableReplacement => None,
        ProviderAppendFileImportMode::Append(checkpoint) => {
            checkpoint.checkpoint().resume_state.clone()
        }
    };
    let validated_resume_state = match validate_adapter_resume_state(
        provider,
        inventory_source_format,
        is_replacement,
        prior_resume_state.as_ref(),
    ) {
        Ok(state) => state,
        Err(reason) => {
            return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                reason,
            ));
        }
    };
    let open_mode = match options.mode {
        ProviderAppendFileImportMode::AppendCapableReplacement => {
            ProviderJsonlOpenMode::AppendCapableReplacement
        }
        ProviderAppendFileImportMode::Append(checkpoint) => {
            ProviderJsonlOpenMode::Append(checkpoint.into_checkpoint())
        }
    };
    let mut reader = match super::open_provider_jsonl(&options.source_path, open_mode)? {
        ProviderJsonlOpenDecision::Ready(reader) => reader,
        ProviderJsonlOpenDecision::ReplacementRequired(reason) => {
            return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                reason,
            ));
        }
    };
    reader.limit_to_observed_size(options.observed_size)?;
    let context = ProviderAdapterContext {
        machine_id: options.machine_id,
        source_path: Some(options.source_path.clone()),
        source_root: Some(options.source_root),
        imported_at: options.imported_at,
    };
    let mut semantic_boundary = None;
    let mut checkpoint_resume_state = prior_resume_state;
    let mut certification_failure = None;
    let summary = match (provider, inventory_source_format) {
        (CaptureProvider::Codex, "codex_session_jsonl_tree" | "codex_session_jsonl") => {
            let resume_state = match &validated_resume_state {
                ValidatedAdapterResumeState::Codex(state) => state.clone(),
                ValidatedAdapterResumeState::None if is_replacement => {
                    CodexSessionJsonlResumeState::default()
                }
                _ => {
                    return Err(CaptureError::SystemInvariant(
                        "validated Codex resume state has the wrong provider",
                    ));
                }
            };
            let append_bootstrap = match read_authoritative_codex_header(&mut reader)? {
                Ok(header) if !is_replacement => Some(header),
                Ok(_) => None,
                Err(reason) if is_replacement => {
                    certification_failure.get_or_insert(reason);
                    None
                }
                Err(reason) => {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        reason,
                    ));
                }
            };
            match import_codex_session_file(
                &mut reader,
                material_source_format,
                store,
                &context,
                options.history_record_id,
                is_replacement,
                append_bootstrap,
                resume_state,
            )? {
                CodexSessionFileImport::Imported { summary, boundary } => {
                    if is_replacement && boundary.additional_session_header {
                        certification_failure
                            .get_or_insert(ProviderJsonlReplacementReason::AdditionalSessionHeader);
                    }
                    checkpoint_resume_state = Some(ProviderJsonlResumeState::CodexSession(
                        boundary.resume_state.clone(),
                    ));
                    semantic_boundary = Some(boundary);
                    summary
                }
                CodexSessionFileImport::DeferredPartial => {
                    return Ok(ProviderAppendFileImportDecision::DeferredPartial);
                }
                CodexSessionFileImport::ReplacementRequired(reason) => {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        reason,
                    ));
                }
            }
        }
        (CaptureProvider::Pi, "pi_session_jsonl") => {
            let bootstrap = match read_authoritative_pi_header(&mut reader)? {
                Ok(header) if !is_replacement => Some(header),
                Ok(_) => None,
                Err(reason) if is_replacement => {
                    certification_failure.get_or_insert(reason);
                    None
                }
                Err(reason) => {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        reason,
                    ));
                }
            };
            let scan = match scan_pi_session(&mut reader, &context, is_replacement)? {
                Ok(scan) => scan,
                Err(reason) => {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        reason,
                    ));
                }
            };
            if is_replacement && scan.additional_session_header {
                certification_failure
                    .get_or_insert(ProviderJsonlReplacementReason::AdditionalSessionHeader);
            }
            if is_replacement && scan.deferred_partial && !scan.has_real_message {
                return Ok(ProviderAppendFileImportDecision::DeferredPartial);
            }
            let mut streamed_summary = ProviderImportSummary::default();
            if is_replacement && !scan.has_real_message {
                stream_pi_session_jsonl_reader(&mut reader, &context, bootstrap, |batch| {
                    discard_pi_no_real_batch(batch, &mut streamed_summary);
                    Ok(())
                })?;
                if streamed_summary.failed == 0 {
                    streamed_summary.failed += 1;
                    streamed_summary.sample_failure(ProviderImportFailure {
                        line: usize::try_from(reader.complete_line_count()).unwrap_or(usize::MAX),
                        error: "pi session JSONL contained no real message content".to_owned(),
                    });
                }
            } else {
                streamed_summary = import_normalized_provider_capture_stream(
                    store,
                    normalized_import_options(options.history_record_id),
                    |emit| {
                        stream_pi_session_jsonl_reader(&mut reader, &context, bootstrap, |batch| {
                            let batch = if is_replacement {
                                filter_pi_replacement_batch(batch, &scan.admission)?
                            } else {
                                batch
                            };
                            emit(batch)
                        })
                    },
                )?;
                if is_replacement {
                    streamed_summary.skipped_sessions += scan.admission.rejected_session_count()?;
                }
            }
            streamed_summary
        }
        (CaptureProvider::Claude, "claude_projects_jsonl_tree") => {
            let first_value = match read_authoritative_first_json_record(&mut reader)? {
                Ok(value) => Some(value),
                Err(reason) if is_replacement => {
                    certification_failure.get_or_insert(reason);
                    None
                }
                Err(reason) => {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        reason,
                    ));
                }
            };
            let first_session_id = first_value.as_ref().and_then(claude_header_session_id);
            let first_timestamp_valid = first_value.as_ref().is_some_and(|value| {
                value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
                    .is_some()
            });
            if first_session_id.is_none() || !first_timestamp_valid {
                if is_replacement {
                    certification_failure
                        .get_or_insert(ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid);
                } else {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid,
                    ));
                }
            }
            let authoritative = match &validated_resume_state {
                ValidatedAdapterResumeState::Claude(state) => Some(state),
                ValidatedAdapterResumeState::None if is_replacement => None,
                _ => {
                    return Err(CaptureError::SystemInvariant(
                        "validated Claude resume state has the wrong provider",
                    ));
                }
            };
            if let (Some(first_session_id), Some(authoritative)) =
                (first_session_id.as_deref(), authoritative)
            {
                if first_session_id != authoritative.authoritative_session_id {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        ProviderJsonlReplacementReason::AuthoritativeSessionChanged,
                    ));
                }
            }
            let expected_session_id = authoritative
                .map(|state| state.authoritative_session_id.as_str())
                .or(first_session_id.as_deref());
            let scan = scan_authoritative_session_ids(
                &mut reader,
                CaptureProvider::Claude,
                expected_session_id,
                &context,
            )?;
            if scan.identity_changed {
                if is_replacement {
                    certification_failure
                        .get_or_insert(ProviderJsonlReplacementReason::AuthoritativeSessionChanged);
                } else {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        ProviderJsonlReplacementReason::AuthoritativeSessionChanged,
                    ));
                }
            }
            if is_replacement && scan.deferred_partial && !scan.has_real_message {
                return Ok(ProviderAppendFileImportDecision::DeferredPartial);
            }
            let normalization_header = if is_replacement {
                scan.normalization_header.as_ref()
            } else {
                first_value.as_ref()
            };
            let started_at = authoritative
                .map(|state| {
                    scan.earliest_started_at
                        .map_or(state.authoritative_started_at, |delta_started_at| {
                            state.authoritative_started_at.min(delta_started_at)
                        })
                })
                .or(scan.earliest_started_at)
                .unwrap_or(context.imported_at);
            if let Some(authoritative_session_id) = authoritative
                .map(|state| state.authoritative_session_id.clone())
                .or(first_session_id)
            {
                checkpoint_resume_state = Some(ProviderJsonlResumeState::ClaudeProjects(
                    ClaudeProjectsJsonlResumeState::new(authoritative_session_id, started_at),
                ));
            }
            let fallback_header = Value::Null;
            let header = normalization_header.unwrap_or(&fallback_header);
            let mut streamed_summary = ProviderImportSummary::default();
            if is_replacement && !scan.has_real_message {
                let mut saw_capture = false;
                stream_claude_projects_jsonl_reader(
                    &options.source_path,
                    &mut reader,
                    &context,
                    header,
                    started_at,
                    |batch| {
                        discard_single_session_no_real_batch(
                            batch,
                            &mut streamed_summary,
                            &mut saw_capture,
                        );
                        Ok(())
                    },
                )?;
                finish_single_session_no_real_summary(&mut streamed_summary, saw_capture);
            } else {
                streamed_summary = import_normalized_provider_capture_stream(
                    store,
                    normalized_import_options(options.history_record_id),
                    |emit| {
                        stream_claude_projects_jsonl_reader(
                            &options.source_path,
                            &mut reader,
                            &context,
                            header,
                            started_at,
                            emit,
                        )
                    },
                )?;
            }
            streamed_summary
        }
        (CaptureProvider::Tabnine, "tabnine_cli_chat_recording_jsonl") => {
            let first_value = match read_authoritative_first_json_record(&mut reader)? {
                Ok(value) => Some(value),
                Err(reason) if is_replacement => {
                    certification_failure.get_or_insert(reason);
                    None
                }
                Err(reason) => {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        reason,
                    ));
                }
            };
            let first_session_id = first_value
                .as_ref()
                .and_then(|value| native_jsonl_header_session_id(CaptureProvider::Tabnine, value));
            let first_started_at = first_value
                .as_ref()
                .and_then(|value| native_jsonl_header_start_time(CaptureProvider::Tabnine, value));
            if first_session_id.is_none() || first_started_at.is_none() {
                if is_replacement {
                    certification_failure
                        .get_or_insert(ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid);
                } else {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid,
                    ));
                }
            }
            let authoritative = match &validated_resume_state {
                ValidatedAdapterResumeState::Tabnine(state) => Some(state),
                ValidatedAdapterResumeState::None if is_replacement => None,
                _ => {
                    return Err(CaptureError::SystemInvariant(
                        "validated Tabnine resume state has the wrong provider",
                    ));
                }
            };
            if let (Some(first_session_id), Some(authoritative)) =
                (first_session_id.as_deref(), authoritative)
            {
                if first_session_id != authoritative.authoritative_session_id {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        ProviderJsonlReplacementReason::AuthoritativeSessionChanged,
                    ));
                }
            }
            let expected_session_id = authoritative
                .map(|state| state.authoritative_session_id.as_str())
                .or(first_session_id.as_deref());
            let scan = scan_authoritative_session_ids(
                &mut reader,
                CaptureProvider::Tabnine,
                expected_session_id,
                &context,
            )?;
            if scan.identity_changed {
                if is_replacement {
                    certification_failure
                        .get_or_insert(ProviderJsonlReplacementReason::AuthoritativeSessionChanged);
                } else {
                    return Ok(ProviderAppendFileImportDecision::ReplacementRequired(
                        ProviderJsonlReplacementReason::AuthoritativeSessionChanged,
                    ));
                }
            }
            if is_replacement && scan.deferred_partial && !scan.has_real_message {
                return Ok(ProviderAppendFileImportDecision::DeferredPartial);
            }
            let normalization_header = if is_replacement {
                scan.normalization_header.clone()
            } else {
                first_value.clone()
            }
            .unwrap_or(Value::Null);
            let started_at = authoritative
                .map(|state| state.authoritative_started_at)
                .or_else(|| {
                    native_jsonl_header_start_time(CaptureProvider::Tabnine, &normalization_header)
                })
                .or_else(|| native_jsonl_timestamp(&normalization_header))
                .unwrap_or(context.imported_at);
            if is_replacement {
                if let (Some(authoritative_session_id), Some(authoritative_started_at)) =
                    (first_session_id, first_started_at)
                {
                    checkpoint_resume_state = Some(ProviderJsonlResumeState::TabnineCli(
                        TabnineJsonlResumeState::new(
                            authoritative_session_id,
                            authoritative_started_at,
                        ),
                    ));
                }
            }
            let mut streamed_summary = ProviderImportSummary::default();
            if is_replacement && !scan.has_real_message {
                let mut saw_capture = false;
                stream_native_jsonl_session_reader(
                    &options.source_path,
                    &mut reader,
                    &context,
                    NativeJsonlStreamOptions {
                        provider,
                        source_format: material_source_format,
                        header: normalization_header,
                        started_at,
                    },
                    |batch| {
                        discard_single_session_no_real_batch(
                            batch,
                            &mut streamed_summary,
                            &mut saw_capture,
                        );
                        Ok(())
                    },
                )?;
                finish_single_session_no_real_summary(&mut streamed_summary, saw_capture);
            } else {
                streamed_summary = import_normalized_provider_capture_stream(
                    store,
                    normalized_import_options(options.history_record_id),
                    |emit| {
                        stream_native_jsonl_session_reader(
                            &options.source_path,
                            &mut reader,
                            &context,
                            NativeJsonlStreamOptions {
                                provider,
                                source_format: material_source_format,
                                header: normalization_header,
                                started_at,
                            },
                            emit,
                        )
                    },
                )?;
            }
            streamed_summary
        }
        _ => {
            return Err(CaptureError::SystemInvariant(
                "append-capable provider contract has no per-file importer",
            ));
        }
    };

    if summary.has_checkpoint_blocking_maintenance() {
        let mut decision = ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(
            ProviderAppendFileImportWithoutCheckpoint {
                summary,
                reason: ProviderJsonlReplacementReason::CommittedMaintenanceIncomplete,
                source_prefix_sha256: None,
            },
        );
        certify_uncheckpointed_replacement(
            &mut reader,
            options.observed_size,
            is_replacement,
            &mut decision,
        );
        return Ok(decision);
    }
    post_materialization(&mut reader);
    let checkpoint_result = match semantic_boundary {
        Some(boundary) => {
            reader.checkpoint_at(boundary.committed_offset, boundary.complete_line_count)
        }
        None => reader.safe_checkpoint(),
    };
    let checkpoint_decision = match checkpoint_result {
        Ok(decision) => decision,
        Err(error) => {
            let mut decision = ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(
                ProviderAppendFileImportWithoutCheckpoint {
                    summary,
                    reason: unexpected_post_commit_checkpoint_failure(&error),
                    source_prefix_sha256: None,
                },
            );
            certify_uncheckpointed_replacement(
                &mut reader,
                options.observed_size,
                is_replacement,
                &mut decision,
            );
            return Ok(decision);
        }
    };
    let mut decision = finish_import(
        summary,
        checkpoint_decision,
        checkpoint_resume_state,
        certification_failure,
    );
    certify_uncheckpointed_replacement(
        &mut reader,
        options.observed_size,
        is_replacement,
        &mut decision,
    );
    Ok(decision)
}

include!("append_import/completion.rs");

#[cfg(test)]
include!("append_import/tests.rs");

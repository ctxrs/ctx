use std::{collections::BTreeMap, fs::File, io::BufReader, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderRawRetention,
    ProviderRedactionBoundary, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, RedactionState, SessionStatus, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use ctx_history_store::Store;
use serde::Deserialize;
use serde_json::json;

use crate::CodexHistoryJsonlAdapter;

use crate::common::io::{ensure_regular_provider_transcript_file, read_provider_jsonl_line};
use crate::provider::importer::{import_normalized_provider_captures, provider_cursor_stream};
use crate::{
    CodexEventImportMode, CodexHistoryImportOptions, CodexToolOutputMode,
    NormalizedProviderImportOptions, ProviderAdapterContext, ProviderCaptureAdapter,
    ProviderImportFailure, ProviderImportSummary, ProviderNormalizationResult, Result,
};

#[derive(Debug, Deserialize)]
pub(crate) struct CodexHistoryLine {
    pub(crate) session_id: String,
    pub(crate) ts: i64,
    pub(crate) text: String,
}
impl ProviderCaptureAdapter for CodexHistoryJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Codex
    }

    fn source_format(&self) -> &str {
        "codex_history_jsonl"
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut result = ProviderNormalizationResult::default();
        let mut parsed = Vec::new();
        let mut first_seen = BTreeMap::new();
        let mut line = Vec::new();
        let mut line_number = 0usize;

        while read_provider_jsonl_line(&mut reader, &mut line)? {
            line_number += 1;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }

            let history: CodexHistoryLine = match serde_json::from_slice(&line) {
                Ok(history) => history,
                Err(err) => {
                    result.summary.failed += 1;
                    result.summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    continue;
                }
            };
            if history.session_id.trim().is_empty() {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: "codex history line has empty session_id".to_owned(),
                });
                continue;
            }
            let Some(occurred_at) = DateTime::from_timestamp(history.ts, 0) else {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: format!(
                        "codex history line has invalid unix timestamp {}",
                        history.ts
                    ),
                });
                continue;
            };
            first_seen
                .entry(history.session_id.clone())
                .and_modify(|existing: &mut DateTime<Utc>| {
                    if occurred_at < *existing {
                        *existing = occurred_at;
                    }
                })
                .or_insert(occurred_at);
            parsed.push((line_number, history, occurred_at));
        }

        result.captures = parsed
            .into_iter()
            .map(|(line_number, history, occurred_at)| {
                let started_at = first_seen
                    .get(&history.session_id)
                    .copied()
                    .unwrap_or(occurred_at);
                (
                    line_number,
                    ProviderCaptureEnvelope {
                        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                        provider: CaptureProvider::Codex,
                        source: ProviderSourceEnvelope {
                            source_format: self.source_format().to_owned(),
                            machine_id: context.machine_id.clone(),
                            observed_at: context.imported_at,
                            raw_source_path: context
                                .source_path
                                .as_ref()
                                .map(|path| path.display().to_string()),
                            source_root: context.source_root_display(),
                            raw_retention: ProviderRawRetention::PathReference,
                            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
                            trust: ProviderSourceTrust::ProviderExport,
                            fidelity: Fidelity::SummaryOnly,
                            cursor: Some(ProviderCursorRange {
                                before: None,
                                after: Some(ProviderCursorCheckpoint {
                                    stream: provider_cursor_stream(
                                        CaptureProvider::Codex,
                                        self.source_format(),
                                    ),
                                    cursor: format!("line:{line_number}"),
                                    observed_at: occurred_at,
                                }),
                            }),
                            idempotency_key: Some(format!(
                                "provider-source:{}:{}:{}",
                                CaptureProvider::Codex.as_str(),
                                self.source_format(),
                                history.session_id
                            )),
                            metadata: json!({
                                "adapter": "codex_history_jsonl",
                                "source_fidelity": "prompt_log_only",
                            }),
                        },
                        session: ProviderSessionEnvelope {
                            provider_session_id: history.session_id.clone(),
                            parent_provider_session_id: None,
                            root_provider_session_id: None,
                            external_agent_id: None,
                            agent_type: AgentType::Primary,
                            role_hint: Some("primary".to_owned()),
                            is_primary: true,
                            status: SessionStatus::Imported,
                            started_at,
                            ended_at: None,
                            cwd: None,
                            fidelity: Fidelity::SummaryOnly,
                            idempotency_key: Some(format!(
                                "provider-session:{}:{}",
                                CaptureProvider::Codex.as_str(),
                                history.session_id
                            )),
                            artifacts: Vec::new(),
                            metadata: json!({
                                "source_format": self.source_format(),
                                "source_fidelity": "prompt_log_only",
                                "limitations": [
                                    "user prompts only",
                                    "no assistant responses",
                                    "no tool calls",
                                    "no command output",
                                    "no child session relationships"
                                ],
                            }),
                        },
                        event: Some(ProviderEventEnvelope {
                            provider_event_index: (line_number - 1) as u64,
                            provider_event_hash: None,
                            cursor: Some(format!("line:{line_number}")),
                            event_type: EventType::Message,
                            role: Some(EventRole::User),
                            occurred_at,
                            fidelity: Fidelity::SummaryOnly,
                            redaction_state: RedactionState::LocalPreview,
                            idempotency_key: Some(format!(
                                "provider-event:{}:{}:{}",
                                CaptureProvider::Codex.as_str(),
                                history.session_id,
                                line_number - 1
                            )),
                            artifacts: Vec::new(),
                            payload: json!({
                                "text": history.text,
                                "source_format": self.source_format(),
                            }),
                            metadata: json!({
                                "source": "codex_history",
                                "source_format": self.source_format(),
                                "source_fidelity": "prompt_log_only",
                            }),
                        }),
                    },
                )
            })
            .collect();

        Ok(result)
    }
}
pub fn import_codex_history_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodexHistoryImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = CodexHistoryJsonlAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

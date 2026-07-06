use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, Confidence, CtxHistoryJsonlEdgeRecord, CtxHistoryJsonlEventRecord,
    CtxHistoryJsonlFileTouchRecord, CtxHistoryJsonlRecord, CtxHistoryJsonlSessionRecord,
    CtxHistoryJsonlSourceRecord, Fidelity, ProviderCaptureEnvelope, ProviderCursorCheckpoint,
    ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, SessionEdgeType, CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use crate::stable_capture_uuid;

use crate::common::io::{ensure_regular_provider_transcript_file, read_provider_jsonl_line};
use crate::{
    ProviderAdapterContext, ProviderFileTouchedEnvelope, ProviderImportFailure,
    ProviderImportSummary, ProviderNormalizationResult, Result,
};

mod persistence;

pub(crate) use persistence::{import_custom_history_edges, import_custom_history_source_cursors};

pub(crate) struct CustomHistoryJsonlV1NormalizationResult {
    pub(crate) provider: ProviderNormalizationResult,
    pub(crate) edges: Vec<(usize, CustomHistoryJsonlV1EdgeImport)>,
    pub(crate) source_cursors: Vec<CustomHistoryJsonlV1SourceCursorImport>,
}

#[derive(Debug, Clone)]
pub(crate) struct CustomHistoryJsonlV1SourceCursorImport {
    pub(crate) machine_id: String,
    pub(crate) checkpoint: ProviderCursorCheckpoint,
}

#[derive(Debug, Clone)]
pub(crate) struct CustomHistoryJsonlV1EdgeImport {
    pub(crate) provider_key: String,
    pub(crate) source_id: String,
    pub(crate) source_format: String,
    pub(crate) raw_source_path: Option<String>,
    pub(crate) from_provider_session_id: String,
    pub(crate) to_provider_session_id: String,
    pub(crate) edge_id: Option<String>,
    pub(crate) edge_type: SessionEdgeType,
    pub(crate) confidence: Confidence,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) fidelity: Fidelity,
    pub(crate) metadata: Value,
}

pub(crate) fn normalize_custom_history_jsonl_v1(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<CustomHistoryJsonlV1NormalizationResult> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    normalize_custom_history_jsonl_v1_reader(reader, context)
}

pub(crate) fn normalize_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
    context: &ProviderAdapterContext,
) -> Result<CustomHistoryJsonlV1NormalizationResult> {
    let mut reader = reader;
    let mut summary = ProviderImportSummary::default();
    let mut records = Vec::new();
    let mut line = Vec::new();
    let mut line_number = 0usize;

    while read_provider_jsonl_line(&mut reader, &mut line)? {
        line_number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<CtxHistoryJsonlRecord>(&line) {
            Ok(record) => records.push((line_number, record)),
            Err(err) => push_provider_import_failure(&mut summary, line_number, err.to_string()),
        }
    }

    if summary.failed > 0 {
        return Ok(custom_history_failed_normalization(summary));
    }

    let mut manifest_line = None;
    let mut sources = BTreeMap::<String, (usize, CtxHistoryJsonlSourceRecord)>::new();
    let mut sessions = BTreeMap::<(String, String), (usize, CtxHistoryJsonlSessionRecord)>::new();
    let mut events = Vec::<(usize, CtxHistoryJsonlEventRecord)>::new();
    let mut event_keys = BTreeSet::<(String, String, u64)>::new();
    let mut file_touches = Vec::<(usize, CtxHistoryJsonlFileTouchRecord)>::new();
    let mut touch_keys = BTreeSet::<(String, String, u64)>::new();
    let mut edges = Vec::<(usize, CtxHistoryJsonlEdgeRecord)>::new();
    let mut edge_keys = BTreeSet::<(String, String, String, String)>::new();

    for (line_number, record) in records {
        match record {
            CtxHistoryJsonlRecord::Manifest(manifest) => {
                if manifest.schema_version != CTX_HISTORY_JSONL_V1_SCHEMA_VERSION {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        format!(
                            "unsupported custom history schema version `{}`",
                            manifest.schema_version
                        ),
                    );
                }
                if manifest_line.replace(line_number).is_some() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate manifest record".to_owned(),
                    );
                }
            }
            CtxHistoryJsonlRecord::Source(source) => {
                validate_custom_source_record(&mut summary, line_number, &source);
                if sources
                    .insert(source.source_id.clone(), (line_number, source))
                    .is_some()
                {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate source_id".to_owned(),
                    );
                }
            }
            CtxHistoryJsonlRecord::Session(session) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &session.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &session.session_id,
                );
                let key = (session.source_id.clone(), session.session_id.clone());
                if sessions.insert(key, (line_number, session)).is_some() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate session record".to_owned(),
                    );
                }
            }
            CtxHistoryJsonlRecord::Event(event) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &event.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &event.session_id,
                );
                let key = (
                    event.source_id.clone(),
                    event.session_id.clone(),
                    event.event_index,
                );
                if !event_keys.insert(key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate event_index for session".to_owned(),
                    );
                }
                events.push((line_number, event));
            }
            CtxHistoryJsonlRecord::FileTouch(file_touch) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &file_touch.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &file_touch.session_id,
                );
                if file_touch.path.trim().is_empty() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "file_touch path must not be empty".to_owned(),
                    );
                }
                let key = (
                    file_touch.source_id.clone(),
                    file_touch.session_id.clone(),
                    file_touch.touch_index,
                );
                if !touch_keys.insert(key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate touch_index for session".to_owned(),
                    );
                }
                file_touches.push((line_number, file_touch));
            }
            CtxHistoryJsonlRecord::Edge(edge) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &edge.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "from_session_id",
                    &edge.from_session_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "to_session_id",
                    &edge.to_session_id,
                );
                let edge_key = edge.edge_id.clone().unwrap_or_else(|| {
                    format!(
                        "{}:{}:{}",
                        edge.from_session_id,
                        edge.to_session_id,
                        edge.edge_type.as_str()
                    )
                });
                let key = (
                    edge.source_id.clone(),
                    edge.from_session_id.clone(),
                    edge.to_session_id.clone(),
                    edge_key,
                );
                if !edge_keys.insert(key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate edge record".to_owned(),
                    );
                }
                edges.push((line_number, edge));
            }
        }
    }

    let reference_index = CustomHistoryReferenceIndex {
        manifest_line,
        sources: &sources,
        sessions: &sessions,
        events: &events,
        event_keys: &event_keys,
        file_touches: &file_touches,
        edges: &edges,
    };
    validate_custom_history_references(&mut summary, reference_index);
    if summary.failed > 0 {
        return Ok(custom_history_failed_normalization(summary));
    }

    let mut result = ProviderNormalizationResult {
        summary,
        ..ProviderNormalizationResult::default()
    };
    let mut source_cursors = Vec::new();
    for (_, source) in sources.values() {
        let machine_id = source
            .machine_id
            .clone()
            .unwrap_or_else(|| context.machine_id.clone());
        if let Some(after) = source
            .cursor
            .as_ref()
            .and_then(|cursor| custom_history_normalized_cursor_range(source, cursor).after)
        {
            source_cursors.push(CustomHistoryJsonlV1SourceCursorImport {
                machine_id,
                checkpoint: after,
            });
        }
    }
    for (line_number, session) in sessions.values() {
        let source = &sources
            .get(&session.source_id)
            .expect("session source already validated")
            .1;
        result.captures.push((
            *line_number,
            custom_history_session_capture(source, session, None, context),
        ));
    }
    for (line_number, event) in events {
        let (_, session) = sessions
            .get(&(event.source_id.clone(), event.session_id.clone()))
            .expect("event session already validated");
        let source = &sources
            .get(&event.source_id)
            .expect("event source already validated")
            .1;
        let envelope = custom_history_event_envelope(source, &event);
        result.captures.push((
            line_number,
            custom_history_session_capture(source, session, Some(envelope), context),
        ));
    }
    for (line_number, file_touch) in file_touches {
        let source = &sources
            .get(&file_touch.source_id)
            .expect("file_touch source already validated")
            .1;
        result.files_touched.push((
            line_number,
            custom_history_file_touch_envelope(source, &file_touch, context),
        ));
    }

    let mut custom_edges = Vec::new();
    for (line_number, edge) in edges {
        let source = &sources
            .get(&edge.source_id)
            .expect("edge source already validated")
            .1;
        custom_edges.push((
            line_number,
            custom_history_edge_import(source, &edge, context),
        ));
    }

    Ok(CustomHistoryJsonlV1NormalizationResult {
        provider: result,
        edges: custom_edges,
        source_cursors,
    })
}

pub(crate) fn custom_history_failed_normalization(
    summary: ProviderImportSummary,
) -> CustomHistoryJsonlV1NormalizationResult {
    CustomHistoryJsonlV1NormalizationResult {
        provider: ProviderNormalizationResult {
            summary,
            ..ProviderNormalizationResult::default()
        },
        edges: Vec::new(),
        source_cursors: Vec::new(),
    }
}

pub(crate) fn push_provider_import_failure(
    summary: &mut ProviderImportSummary,
    line: usize,
    error: String,
) {
    summary.failed += 1;
    summary.failures.push(ProviderImportFailure { line, error });
}

pub(crate) fn validate_custom_source_record(
    summary: &mut ProviderImportSummary,
    line_number: usize,
    source: &CtxHistoryJsonlSourceRecord,
) {
    validate_custom_history_identifier(summary, line_number, "source_id", &source.source_id);
    validate_custom_history_identifier(
        summary,
        line_number,
        "source_format",
        &source.source_format,
    );
    let valid = !source.provider_key.is_empty()
        && source.provider_key.len() <= 128
        && source.provider_key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && source
            .provider_key
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if !valid {
        push_provider_import_failure(
            summary,
            line_number,
            "provider_key must be 1 to 128 bytes, start with a lowercase ASCII letter or digit, and use only lowercase ASCII letters, digits, '.', '_', or '-'".to_owned(),
        );
    }
}

pub(crate) fn validate_custom_history_identifier(
    summary: &mut ProviderImportSummary,
    line_number: usize,
    field: &str,
    value: &str,
) {
    let error = if value.trim().is_empty() {
        Some(format!("{field} must not be empty"))
    } else if value.len() > 512 {
        Some(format!("{field} must be at most 512 bytes"))
    } else if value.chars().any(char::is_control) {
        Some(format!("{field} must not contain control characters"))
    } else {
        None
    };
    if let Some(error) = error {
        push_provider_import_failure(summary, line_number, error);
    }
}

pub(crate) struct CustomHistoryReferenceIndex<'a> {
    pub(crate) manifest_line: Option<usize>,
    pub(crate) sources: &'a BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    pub(crate) sessions: &'a BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    pub(crate) events: &'a [(usize, CtxHistoryJsonlEventRecord)],
    pub(crate) event_keys: &'a BTreeSet<(String, String, u64)>,
    pub(crate) file_touches: &'a [(usize, CtxHistoryJsonlFileTouchRecord)],
    pub(crate) edges: &'a [(usize, CtxHistoryJsonlEdgeRecord)],
}

pub(crate) fn validate_custom_history_references(
    summary: &mut ProviderImportSummary,
    references: CustomHistoryReferenceIndex<'_>,
) {
    if references.manifest_line.is_none() {
        push_provider_import_failure(
            summary,
            0,
            "missing manifest record for ctx-history-jsonl-v1".to_owned(),
        );
    }

    for (line_number, session) in references.sessions.values() {
        if !references.sources.contains_key(&session.source_id) {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "session references unknown source_id `{}`",
                    session.source_id
                ),
            );
        }
        if let Some(parent) = &session.parent_session_id {
            let key = (session.source_id.clone(), parent.clone());
            if !references.sessions.contains_key(&key) {
                push_provider_import_failure(
                    summary,
                    *line_number,
                    format!("session references unknown parent_session_id `{parent}`"),
                );
            }
        }
        if let Some(root) = &session.root_session_id {
            let key = (session.source_id.clone(), root.clone());
            if root != &session.session_id && !references.sessions.contains_key(&key) {
                push_provider_import_failure(
                    summary,
                    *line_number,
                    format!("session references unknown root_session_id `{root}`"),
                );
            }
        }
    }

    for (line_number, event) in references.events {
        if !references
            .sessions
            .contains_key(&(event.source_id.clone(), event.session_id.clone()))
        {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "event references unknown session `{}` in source `{}`",
                    event.session_id, event.source_id
                ),
            );
        }
    }

    for (line_number, file_touch) in references.file_touches {
        if !references
            .sessions
            .contains_key(&(file_touch.source_id.clone(), file_touch.session_id.clone()))
        {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "file_touch references unknown session `{}` in source `{}`",
                    file_touch.session_id, file_touch.source_id
                ),
            );
        }
        if let Some(event_index) = file_touch.event_index {
            let key = (
                file_touch.source_id.clone(),
                file_touch.session_id.clone(),
                event_index,
            );
            if !references.event_keys.contains(&key) {
                push_provider_import_failure(
                    summary,
                    *line_number,
                    format!("file_touch references unknown event_index `{event_index}`"),
                );
            }
        }
    }

    for (line_number, edge) in references.edges {
        let from_key = (edge.source_id.clone(), edge.from_session_id.clone());
        let to_key = (edge.source_id.clone(), edge.to_session_id.clone());
        if !references.sessions.contains_key(&from_key) {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "edge references unknown from_session_id `{}`",
                    edge.from_session_id
                ),
            );
        }
        if !references.sessions.contains_key(&to_key) {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "edge references unknown to_session_id `{}`",
                    edge.to_session_id
                ),
            );
        }
        if edge.edge_type == SessionEdgeType::ParentChild {
            let Some((_, child)) = references.sessions.get(&to_key) else {
                continue;
            };
            if let Some(parent) = &child.parent_session_id {
                if parent != &edge.from_session_id {
                    push_provider_import_failure(
                        summary,
                        *line_number,
                        format!(
                            "parent_child edge from_session_id `{}` conflicts with session parent_session_id `{parent}`",
                            edge.from_session_id
                        ),
                    );
                }
            }
        }
    }
}

pub(crate) fn custom_history_session_capture(
    source: &CtxHistoryJsonlSourceRecord,
    session: &CtxHistoryJsonlSessionRecord,
    event: Option<ProviderEventEnvelope>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let provider_session_id = custom_history_internal_session_id(
        &source.provider_key,
        &source.source_id,
        &session.session_id,
    );
    let event_cursor = event.as_ref().and_then(|event| {
        event.cursor.as_ref().map(|cursor| ProviderCursorRange {
            before: None,
            after: Some(ProviderCursorCheckpoint {
                stream: custom_history_cursor_stream(source),
                cursor: cursor.clone(),
                observed_at: event.occurred_at,
            }),
        })
    });
    let source_cursor = source
        .cursor
        .as_ref()
        .map(|cursor| custom_history_normalized_cursor_range(source, cursor))
        .or(event_cursor);
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::Custom,
        source: ProviderSourceEnvelope {
            source_format: source.source_format.clone(),
            machine_id: source
                .machine_id
                .clone()
                .unwrap_or_else(|| context.machine_id.clone()),
            observed_at: source.observed_at.unwrap_or(context.imported_at),
            raw_source_path: custom_history_effective_raw_source_path(source, context),
            raw_retention: source.raw_retention,
            redaction_boundary: source.redaction_boundary,
            trust: match source.trust {
                ProviderSourceTrust::Unknown => ProviderSourceTrust::ProviderExport,
                other => other,
            },
            fidelity: source.fidelity,
            cursor: source_cursor,
            idempotency_key: Some(format!(
                "ctx-history-jsonl-v1:{}:{}",
                source.provider_key, source.source_id
            )),
            metadata: custom_history_metadata(
                source.metadata.clone(),
                json!({
                    "provider_key": source.provider_key,
                    "source_id": source.source_id,
                    "source_format": source.source_format,
                    "raw_uri": source.raw_uri,
                    "raw_source_path": source.raw_source_path,
                    "fingerprint": source.fingerprint,
                    "importer_version": source.importer_version,
                    "cursor": source.cursor,
                }),
            ),
        },
        session: ProviderSessionEnvelope {
            provider_session_id,
            parent_provider_session_id: session.parent_session_id.as_ref().map(|parent| {
                custom_history_internal_session_id(&source.provider_key, &source.source_id, parent)
            }),
            root_provider_session_id: session.root_session_id.as_ref().map(|root| {
                custom_history_internal_session_id(&source.provider_key, &source.source_id, root)
            }),
            external_agent_id: session.external_agent_id.clone(),
            agent_type: session.agent_type,
            role_hint: session.role_hint.clone(),
            is_primary: session.is_primary,
            status: session.status,
            started_at: session.started_at,
            ended_at: session.ended_at,
            cwd: session.cwd.clone(),
            fidelity: session.fidelity,
            idempotency_key: session.idempotency_key.clone().or_else(|| {
                Some(format!(
                    "ctx-history-jsonl-v1:{}:{}:{}",
                    source.provider_key, source.source_id, session.session_id
                ))
            }),
            artifacts: session.artifacts.clone(),
            metadata: custom_history_metadata(
                session.metadata.clone(),
                json!({
                    "provider_key": source.provider_key,
                    "source_id": source.source_id,
                    "session_id": session.session_id,
                    "native_session_id": session.native_session_id,
                    "parent_session_id": session.parent_session_id,
                    "root_session_id": session.root_session_id,
                }),
            ),
        },
        event,
    }
}

pub(crate) fn custom_history_event_envelope(
    source: &CtxHistoryJsonlSourceRecord,
    event: &CtxHistoryJsonlEventRecord,
) -> ProviderEventEnvelope {
    let payload = if let Some(preview) = &event.preview {
        json!({ "text": preview })
    } else {
        event.payload.clone()
    };
    let raw_payload = event
        .preview
        .as_ref()
        .map(|_| event.payload.clone())
        .filter(|payload| payload != &json!({}));
    ProviderEventEnvelope {
        provider_event_index: event.event_index,
        provider_event_hash: event.event_hash.clone(),
        cursor: event.native_cursor.clone(),
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        fidelity: event.fidelity,
        redaction_state: event.redaction_state,
        idempotency_key: event.idempotency_key.clone(),
        artifacts: event.artifacts.clone(),
        payload,
        metadata: custom_history_metadata(
            event.metadata.clone(),
            json!({
                "provider_key": source.provider_key,
                "source_id": event.source_id,
                "session_id": event.session_id,
                "event_id": event.event_id,
                "native_cursor": event.native_cursor,
                "preview": event.preview,
                "raw_payload": raw_payload,
            }),
        ),
    }
}

pub(crate) fn custom_history_file_touch_envelope(
    source: &CtxHistoryJsonlSourceRecord,
    file_touch: &CtxHistoryJsonlFileTouchRecord,
    context: &ProviderAdapterContext,
) -> ProviderFileTouchedEnvelope {
    ProviderFileTouchedEnvelope {
        provider: CaptureProvider::Custom,
        provider_session_id: custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &file_touch.session_id,
        ),
        provider_touch_index: file_touch.touch_index,
        provider_event_index: file_touch.event_index,
        raw_source_path: custom_history_effective_raw_source_path(source, context),
        path: file_touch.path.clone(),
        change_kind: file_touch.change_kind,
        old_path: file_touch.old_path.clone(),
        line_count_delta: file_touch.line_count_delta,
        confidence: file_touch.confidence,
        occurred_at: file_touch.occurred_at,
        source_format: source.source_format.clone(),
        metadata: custom_history_metadata(
            file_touch.metadata.clone(),
            json!({
                "provider_key": source.provider_key,
                "source_id": file_touch.source_id,
                "session_id": file_touch.session_id,
            }),
        ),
    }
}

pub(crate) fn custom_history_edge_import(
    source: &CtxHistoryJsonlSourceRecord,
    edge: &CtxHistoryJsonlEdgeRecord,
    context: &ProviderAdapterContext,
) -> CustomHistoryJsonlV1EdgeImport {
    CustomHistoryJsonlV1EdgeImport {
        provider_key: source.provider_key.clone(),
        source_id: source.source_id.clone(),
        source_format: source.source_format.clone(),
        raw_source_path: custom_history_effective_raw_source_path(source, context),
        from_provider_session_id: custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &edge.from_session_id,
        ),
        to_provider_session_id: custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &edge.to_session_id,
        ),
        edge_id: edge.edge_id.clone(),
        edge_type: edge.edge_type,
        confidence: edge.confidence,
        occurred_at: edge.occurred_at.unwrap_or(context.imported_at),
        fidelity: edge.fidelity,
        metadata: custom_history_metadata(
            edge.metadata.clone(),
            json!({
                "provider_key": source.provider_key,
                "source_id": edge.source_id,
                "from_session_id": edge.from_session_id,
                "to_session_id": edge.to_session_id,
                "edge_id": edge.edge_id,
            }),
        ),
    }
}

pub(crate) fn custom_history_effective_raw_source_path(
    source: &CtxHistoryJsonlSourceRecord,
    context: &ProviderAdapterContext,
) -> Option<String> {
    source.raw_source_path.clone().or_else(|| {
        context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string())
    })
}

pub(crate) fn custom_history_internal_session_id(
    provider_key: &str,
    source_id: &str,
    session_id: &str,
) -> String {
    let key = custom_history_key(json!({
        "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
        "kind": "session",
        "provider_key": provider_key,
        "source_id": source_id,
        "session_id": session_id,
    }));
    let id = stable_capture_uuid(&key, "custom-provider-session-id");
    format!("ctx-history-jsonl-v1-{id}")
}

pub(crate) fn custom_history_cursor_stream(source: &CtxHistoryJsonlSourceRecord) -> String {
    custom_history_jsonl_v1_cursor_stream(
        &source.provider_key,
        &source.source_id,
        &source.source_format,
    )
}

pub fn custom_history_jsonl_v1_cursor_stream(
    provider_key: &str,
    source_id: &str,
    source_format: &str,
) -> String {
    let key = custom_history_key(json!({
        "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
        "kind": "cursor_stream",
        "provider_key": provider_key,
        "source_id": source_id,
        "source_format": source_format,
    }));
    let stream_id = stable_capture_uuid(&key, "custom-cursor-stream");
    format!("provider:custom:{provider_key}:{stream_id}")
}

pub(crate) fn custom_history_normalized_cursor_range(
    source: &CtxHistoryJsonlSourceRecord,
    cursor: &ProviderCursorRange,
) -> ProviderCursorRange {
    ProviderCursorRange {
        before: cursor
            .before
            .as_ref()
            .map(|checkpoint| custom_history_normalized_cursor_checkpoint(source, checkpoint)),
        after: cursor
            .after
            .as_ref()
            .map(|checkpoint| custom_history_normalized_cursor_checkpoint(source, checkpoint)),
    }
}

pub(crate) fn custom_history_normalized_cursor_checkpoint(
    source: &CtxHistoryJsonlSourceRecord,
    checkpoint: &ProviderCursorCheckpoint,
) -> ProviderCursorCheckpoint {
    ProviderCursorCheckpoint {
        stream: custom_history_cursor_stream(source),
        cursor: checkpoint.cursor.clone(),
        observed_at: checkpoint.observed_at,
    }
}

pub(crate) fn custom_history_key(value: Value) -> String {
    serde_json::to_string(&value).expect("custom history identity key is serializable")
}

pub(crate) fn custom_history_metadata(base: Value, custom: Value) -> Value {
    let mut map = match base {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("metadata".to_owned(), other);
            map
        }
    };
    map.insert("ctx_history_jsonl_v1".to_owned(), custom);
    Value::Object(map)
}

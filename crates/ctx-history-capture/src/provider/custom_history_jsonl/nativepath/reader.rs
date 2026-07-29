use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, Cursor, Read},
    path::Path,
};

use ctx_history_core::{
    CtxHistoryJsonlEdgeRecord, CtxHistoryJsonlEventRecord, CtxHistoryJsonlFileTouchRecord,
    CtxHistoryJsonlRecord, CtxHistoryJsonlSessionRecord, CtxHistoryJsonlSourceRecord,
    CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
};

use crate::{
    common::io::{open_provider_source_file, read_provider_jsonl_record_or_skip_oversized},
    ProviderImportSummary, Result,
};

use super::super::{
    push_provider_import_failure, reject_invalid_custom_history_references,
    retain_custom_history_content_sessions, validate_custom_history_identifier,
    validate_custom_source_record,
};

#[derive(Debug)]
pub(super) struct ParsedCustomHistory {
    pub(super) summary: ProviderImportSummary,
    pub(super) sources: BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    pub(super) sessions: BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    pub(super) events: Vec<(usize, CtxHistoryJsonlEventRecord)>,
    pub(super) file_touches: Vec<(usize, CtxHistoryJsonlFileTouchRecord)>,
    pub(super) edges: Vec<(usize, CtxHistoryJsonlEdgeRecord)>,
    pub(super) source_revision: String,
}

pub(crate) fn validate_custom_history_nativepath(path: &Path) -> Result<ProviderImportSummary> {
    let source = open_provider_source_file(path)?;
    let bytes = source.read_all_bounded(usize::MAX)?;
    source.revalidate()?;
    Ok(parse_custom_history(Cursor::new(bytes), "validation-only".to_owned())?.summary)
}

pub(crate) fn validate_custom_history_nativepath_reader(
    mut reader: impl BufRead,
) -> Result<ProviderImportSummary> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(parse_custom_history(Cursor::new(bytes), "validation-only".to_owned())?.summary)
}

pub(super) fn parse_custom_history(
    mut reader: impl BufRead,
    source_revision: String,
) -> Result<ParsedCustomHistory> {
    let mut summary = ProviderImportSummary::default();
    let mut manifest_line = None;
    let mut manifest_is_structurally_invalid = false;
    let mut sources = BTreeMap::<String, (usize, CtxHistoryJsonlSourceRecord)>::new();
    let mut sessions = BTreeMap::<(String, String), (usize, CtxHistoryJsonlSessionRecord)>::new();
    let mut events = Vec::<(usize, CtxHistoryJsonlEventRecord)>::new();
    let mut event_keys = BTreeSet::<(String, String, u64)>::new();
    let mut file_touches = Vec::<(usize, CtxHistoryJsonlFileTouchRecord)>::new();
    let mut touch_keys = BTreeSet::<(String, String, u64)>::new();
    let mut edges = Vec::<(usize, CtxHistoryJsonlEdgeRecord)>::new();
    let mut edge_keys = BTreeSet::<(String, String, String, String)>::new();
    let mut line = Vec::new();
    let mut line_number = 0_usize;

    while read_provider_jsonl_record_or_skip_oversized(
        &mut reader,
        &mut line,
        &mut line_number,
        &mut summary,
    )? {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let record = match serde_json::from_slice::<CtxHistoryJsonlRecord>(&line) {
            Ok(record) => record,
            Err(error) => {
                push_provider_import_failure(&mut summary, line_number, error.to_string());
                continue;
            }
        };
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
                    manifest_is_structurally_invalid = true;
                }
                if manifest_line.replace(line_number).is_some() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate manifest record".to_owned(),
                    );
                    manifest_is_structurally_invalid = true;
                }
            }
            CtxHistoryJsonlRecord::Source(source) => {
                let failures_before = summary.failed;
                validate_custom_source_record(&mut summary, line_number, &source);
                if sources.contains_key(&source.source_id) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate source_id".to_owned(),
                    );
                }
                if summary.failed == failures_before {
                    sources.insert(source.source_id.clone(), (line_number, source));
                }
            }
            CtxHistoryJsonlRecord::Session(session) => {
                let failures_before = summary.failed;
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
                if sessions.contains_key(&key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate session record".to_owned(),
                    );
                }
                if summary.failed == failures_before {
                    sessions.insert(key, (line_number, session));
                }
            }
            CtxHistoryJsonlRecord::Event(event) => {
                let failures_before = summary.failed;
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
                if event_keys.contains(&key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate event_index for session".to_owned(),
                    );
                }
                if summary.failed == failures_before {
                    event_keys.insert(key);
                    events.push((line_number, event));
                }
            }
            CtxHistoryJsonlRecord::FileTouch(file_touch) => {
                let failures_before = summary.failed;
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
                if touch_keys.contains(&key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate touch_index for session".to_owned(),
                    );
                }
                if summary.failed == failures_before {
                    touch_keys.insert(key);
                    file_touches.push((line_number, file_touch));
                }
            }
            CtxHistoryJsonlRecord::Edge(edge) => {
                let failures_before = summary.failed;
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
                if edge_keys.contains(&key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate edge record".to_owned(),
                    );
                }
                if summary.failed == failures_before {
                    edge_keys.insert(key);
                    edges.push((line_number, edge));
                }
            }
        }
    }

    if manifest_line.is_none() {
        push_provider_import_failure(
            &mut summary,
            0,
            "missing manifest record for ctx-history-jsonl-v1".to_owned(),
        );
        manifest_is_structurally_invalid = true;
    }
    if manifest_is_structurally_invalid {
        sources.clear();
        sessions.clear();
        events.clear();
        file_touches.clear();
        edges.clear();
        return Ok(ParsedCustomHistory {
            summary,
            sources,
            sessions,
            events,
            file_touches,
            edges,
            source_revision,
        });
    }

    reject_invalid_custom_history_references(
        &mut summary,
        &sources,
        &mut sessions,
        &mut events,
        &mut event_keys,
        &mut file_touches,
        &mut edges,
    );
    retain_custom_history_content_sessions(&mut sessions, &events, &file_touches, &edges);
    Ok(ParsedCustomHistory {
        summary,
        sources,
        sessions,
        events,
        file_touches,
        edges,
        source_revision,
    })
}

use std::collections::{BTreeMap, BTreeSet};

use ctx_history_core::{
    CtxHistoryJsonlEdgeRecord, CtxHistoryJsonlEventRecord, CtxHistoryJsonlFileTouchRecord,
    CtxHistoryJsonlSessionRecord, CtxHistoryJsonlSourceRecord, SessionEdgeType,
    CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use crate::stable_capture_uuid;

use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderImportSummary, Result,
};

mod nativepath;

// Registration is intentionally owned by the shared provider registry follow-up.
#[allow(unused_imports)]
pub(crate) use nativepath::{
    observe_custom_history_source_backed_explicit, revalidate_custom_history_source_backed,
    scan_custom_history_source_backed_explicit, validate_custom_history_nativepath,
    validate_custom_history_nativepath_reader, CustomHistoryReplacementEvidence,
    CustomHistoryReplacementReason, CustomHistorySourceBackedDisposition,
    CustomHistorySourceBackedError, CustomHistorySourceBackedInput,
    CustomHistorySourceBackedInventory, CustomHistorySourceBackedOutcome,
    CustomHistorySourceBackedPage, CustomHistorySourceBackedReceipt,
    CustomHistorySourceBackedResolver, CustomHistorySourceBackedResult,
    CustomHistorySourceBackedRoute,
};

pub fn decode_custom_history_jsonl_v1_cursor(encoded: &str) -> Result<String> {
    let _ = encoded;
    Err(CaptureError::UnsupportedSchema(
        "Custom History Store cursors were removed with legacy Store ingestion".to_owned(),
    ))
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

pub(crate) fn reject_invalid_custom_history_references(
    summary: &mut ProviderImportSummary,
    sources: &BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    sessions: &mut BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    events: &mut Vec<(usize, CtxHistoryJsonlEventRecord)>,
    event_keys: &mut BTreeSet<(String, String, u64)>,
    file_touches: &mut Vec<(usize, CtxHistoryJsonlFileTouchRecord)>,
    edges: &mut Vec<(usize, CtxHistoryJsonlEdgeRecord)>,
) {
    loop {
        let invalid = sessions
            .iter()
            .filter_map(|(key, (line_number, session))| {
                custom_history_session_reference_error(sources, sessions, session)
                    .map(|error| (key.clone(), *line_number, error))
            })
            .collect::<Vec<_>>();
        if invalid.is_empty() {
            break;
        }
        for (key, line_number, error) in invalid {
            sessions.remove(&key);
            push_provider_import_failure(summary, line_number, error);
        }
    }

    let mut valid_events = Vec::with_capacity(events.len());
    for (line_number, event) in events.drain(..) {
        if sessions.contains_key(&(event.source_id.clone(), event.session_id.clone())) {
            valid_events.push((line_number, event));
        } else {
            push_provider_import_failure(
                summary,
                line_number,
                format!(
                    "event references unknown session `{}` in source `{}`",
                    event.session_id, event.source_id
                ),
            );
        }
    }
    *events = valid_events;
    *event_keys = events
        .iter()
        .map(|(_, event)| {
            (
                event.source_id.clone(),
                event.session_id.clone(),
                event.event_index,
            )
        })
        .collect();

    let mut valid_file_touches = Vec::with_capacity(file_touches.len());
    for (line_number, file_touch) in file_touches.drain(..) {
        let session_key = (file_touch.source_id.clone(), file_touch.session_id.clone());
        let error = if !sessions.contains_key(&session_key) {
            Some(format!(
                "file_touch references unknown session `{}` in source `{}`",
                file_touch.session_id, file_touch.source_id
            ))
        } else if let Some(event_index) = file_touch.event_index {
            let key = (
                file_touch.source_id.clone(),
                file_touch.session_id.clone(),
                event_index,
            );
            (!event_keys.contains(&key))
                .then(|| format!("file_touch references unknown event_index `{event_index}`"))
        } else {
            None
        };
        if let Some(error) = error {
            push_provider_import_failure(summary, line_number, error);
        } else {
            valid_file_touches.push((line_number, file_touch));
        }
    }
    *file_touches = valid_file_touches;

    let mut valid_edges = Vec::with_capacity(edges.len());
    for (line_number, edge) in edges.drain(..) {
        let from_key = (edge.source_id.clone(), edge.from_session_id.clone());
        let to_key = (edge.source_id.clone(), edge.to_session_id.clone());
        let error = if !sessions.contains_key(&from_key) {
            Some(format!(
                "edge references unknown from_session_id `{}`",
                edge.from_session_id
            ))
        } else if !sessions.contains_key(&to_key) {
            Some(format!(
                "edge references unknown to_session_id `{}`",
                edge.to_session_id
            ))
        } else if edge.edge_type == SessionEdgeType::ParentChild {
            sessions.get(&to_key).and_then(|(_, child)| {
                child.parent_session_id.as_ref().and_then(|parent| {
                    (parent != &edge.from_session_id).then(|| {
                        format!(
                            "parent_child edge from_session_id `{}` conflicts with session parent_session_id `{parent}`",
                            edge.from_session_id
                        )
                    })
                })
            })
        } else {
            None
        };
        if let Some(error) = error {
            push_provider_import_failure(summary, line_number, error);
        } else {
            valid_edges.push((line_number, edge));
        }
    }
    *edges = valid_edges;
}

fn custom_history_session_reference_error(
    sources: &BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    sessions: &BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    session: &CtxHistoryJsonlSessionRecord,
) -> Option<String> {
    if !sources.contains_key(&session.source_id) {
        return Some(format!(
            "session references unknown source_id `{}`",
            session.source_id
        ));
    }
    if let Some(parent) = &session.parent_session_id {
        let key = (session.source_id.clone(), parent.clone());
        if !sessions.contains_key(&key) {
            return Some(format!(
                "session references unknown parent_session_id `{parent}`"
            ));
        }
    }
    if let Some(root) = &session.root_session_id {
        let key = (session.source_id.clone(), root.clone());
        if root != &session.session_id && !sessions.contains_key(&key) {
            return Some(format!(
                "session references unknown root_session_id `{root}`"
            ));
        }
    }
    None
}

pub(crate) fn retain_custom_history_content_sessions(
    sessions: &mut BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    events: &[(usize, CtxHistoryJsonlEventRecord)],
    file_touches: &[(usize, CtxHistoryJsonlFileTouchRecord)],
    edges: &[(usize, CtxHistoryJsonlEdgeRecord)],
) {
    let mut required = events
        .iter()
        .map(|(_, event)| (event.source_id.clone(), event.session_id.clone()))
        .chain(
            file_touches
                .iter()
                .map(|(_, touch)| (touch.source_id.clone(), touch.session_id.clone())),
        )
        .chain(edges.iter().flat_map(|(_, edge)| {
            [
                (edge.source_id.clone(), edge.from_session_id.clone()),
                (edge.source_id.clone(), edge.to_session_id.clone()),
            ]
        }))
        .collect::<BTreeSet<_>>();

    loop {
        let dependencies = required
            .iter()
            .filter_map(|key| sessions.get(key).map(|(_, session)| session))
            .flat_map(|session| {
                [
                    session
                        .parent_session_id
                        .as_ref()
                        .map(|id| (session.source_id.clone(), id.clone())),
                    session
                        .root_session_id
                        .as_ref()
                        .map(|id| (session.source_id.clone(), id.clone())),
                ]
            })
            .flatten()
            .collect::<Vec<_>>();
        let before = required.len();
        required.extend(dependencies);
        if required.len() == before {
            break;
        }
    }
    sessions.retain(|key, _| required.contains(key));
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

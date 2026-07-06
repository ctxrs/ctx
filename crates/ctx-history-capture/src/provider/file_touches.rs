use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, Confidence, EventType, FileChangeKind, ProviderEventEnvelope,
};
use serde_json::{json, Value};

use crate::ProviderFileTouchedEnvelope;

pub(crate) struct FileTouchDraft {
    pub(crate) path: String,
    pub(crate) old_path: Option<String>,
    pub(crate) change_kind: Option<FileChangeKind>,
    pub(crate) confidence: Confidence,
    pub(crate) metadata: Value,
}

pub(crate) fn provider_file_touches_from_event(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
    source_root: Option<&str>,
    event: &ProviderEventEnvelope,
    line_number: usize,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    if !matches!(
        event.event_type,
        EventType::ToolCall
            | EventType::ToolOutput
            | EventType::CommandOutput
            | EventType::FileTouched
    ) {
        return Vec::new();
    }

    let mut drafts = Vec::new();
    collect_patch_file_touches(&event.payload, &mut drafts);
    if drafts.is_empty() && event_type_supports_structured_file_touches(event.event_type) {
        collect_structured_file_touches(&event.payload, &mut drafts);
    }

    provider_file_touch_envelopes(
        ProviderFileTouchEnvelopeContext {
            provider,
            provider_session_id,
            source_format,
            raw_source_path,
            source_root,
            occurred_at: event.occurred_at,
            provider_event_index: Some(event.provider_event_index),
            provider_touch_base_index: event.provider_event_index << 16,
            line_number,
        },
        drafts,
    )
}

pub(crate) fn provider_file_touches_from_raw_value(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
    raw_value: &Value,
    event: &ProviderEventEnvelope,
    line_number: usize,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    if !matches!(
        event.event_type,
        EventType::ToolCall
            | EventType::ToolOutput
            | EventType::CommandOutput
            | EventType::FileTouched
    ) {
        return Vec::new();
    }

    let mut drafts = Vec::new();
    collect_patch_file_touches(raw_value, &mut drafts);
    if drafts.is_empty() && (event_type_supports_structured_file_touches(event.event_type)) {
        collect_structured_file_touches(raw_value, &mut drafts);
    }

    provider_file_touch_envelopes(
        ProviderFileTouchEnvelopeContext {
            provider,
            provider_session_id,
            source_format,
            raw_source_path,
            source_root: raw_source_path,
            occurred_at: event.occurred_at,
            provider_event_index: Some(event.provider_event_index),
            provider_touch_base_index: event.provider_event_index << 16,
            line_number,
        },
        drafts,
    )
}

pub(crate) fn event_type_supports_structured_file_touches(event_type: EventType) -> bool {
    matches!(event_type, EventType::ToolCall | EventType::FileTouched)
}

pub(crate) struct ProviderFileTouchEnvelopeContext<'a> {
    pub(crate) provider: CaptureProvider,
    pub(crate) provider_session_id: &'a str,
    pub(crate) source_format: &'a str,
    pub(crate) raw_source_path: Option<&'a str>,
    pub(crate) source_root: Option<&'a str>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) provider_event_index: Option<u64>,
    pub(crate) provider_touch_base_index: u64,
    pub(crate) line_number: usize,
}

pub(crate) fn provider_file_touch_envelopes(
    context: ProviderFileTouchEnvelopeContext<'_>,
    drafts: Vec<FileTouchDraft>,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for draft in drafts {
        let key = (
            draft.path.clone(),
            draft.old_path.clone(),
            draft.change_kind.map(|kind| kind.as_str().to_owned()),
        );
        if !seen.insert(key) {
            continue;
        }
        let provider_touch_index = context.provider_touch_base_index | (out.len() as u64);
        out.push((
            context.line_number,
            ProviderFileTouchedEnvelope {
                provider: context.provider,
                provider_session_id: context.provider_session_id.to_owned(),
                provider_touch_index,
                provider_event_index: context.provider_event_index,
                raw_source_path: context.raw_source_path.map(str::to_owned),
                source_root: context.source_root.map(str::to_owned),
                path: draft.path,
                change_kind: draft.change_kind,
                old_path: draft.old_path,
                line_count_delta: None,
                confidence: draft.confidence,
                occurred_at: context.occurred_at,
                source_format: context.source_format.to_owned(),
                metadata: draft.metadata,
            },
        ));
    }
    out
}

pub(crate) fn collect_patch_file_touches(value: &Value, out: &mut Vec<FileTouchDraft>) {
    match value {
        Value::String(text) => {
            if text.contains("*** Begin Patch") {
                out.extend(parse_apply_patch_file_touches(text));
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_patch_file_touches(item, out);
            }
        }
        Value::Object(object) => {
            for value in object.values() {
                collect_patch_file_touches(value, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn parse_apply_patch_file_touches(patch: &str) -> Vec<FileTouchDraft> {
    let mut out = Vec::new();
    let mut pending_update: Option<String> = None;
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            flush_pending_patch_update(&mut out, &mut pending_update);
            if let Some(path) = normalize_file_path(path) {
                out.push(file_touch_draft(
                    path,
                    None,
                    FileChangeKind::Created,
                    Confidence::Explicit,
                    "apply_patch_add",
                ));
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            flush_pending_patch_update(&mut out, &mut pending_update);
            pending_update = normalize_file_path(path);
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            flush_pending_patch_update(&mut out, &mut pending_update);
            if let Some(path) = normalize_file_path(path) {
                out.push(file_touch_draft(
                    path,
                    None,
                    FileChangeKind::Deleted,
                    Confidence::Explicit,
                    "apply_patch_delete",
                ));
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Move to: ") {
            let old_path = pending_update.take();
            if let Some(path) = normalize_file_path(path) {
                out.push(file_touch_draft(
                    path,
                    old_path,
                    FileChangeKind::Renamed,
                    Confidence::Explicit,
                    "apply_patch_move",
                ));
            }
        }
    }
    flush_pending_patch_update(&mut out, &mut pending_update);
    out
}

pub(crate) fn flush_pending_patch_update(
    out: &mut Vec<FileTouchDraft>,
    pending_update: &mut Option<String>,
) {
    if let Some(path) = pending_update.take() {
        out.push(file_touch_draft(
            path,
            None,
            FileChangeKind::Modified,
            Confidence::Explicit,
            "apply_patch_update",
        ));
    }
}

pub(crate) fn collect_structured_file_touches(value: &Value, out: &mut Vec<FileTouchDraft>) {
    collect_structured_file_touches_with_context(value, out, None);
}

pub(crate) fn collect_structured_file_touches_with_context(
    value: &Value,
    out: &mut Vec<FileTouchDraft>,
    inherited_kind: Option<FileChangeKind>,
) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_structured_file_touches_with_context(item, out, inherited_kind);
            }
        }
        Value::Object(object) => {
            let operation_kind = object_operation_hint_kind(object);
            let object_kind = operation_kind.or(inherited_kind);
            collect_structured_file_touch_object(object, out, object_kind);
            for value in object.values() {
                collect_structured_file_touches_with_context(value, out, object_kind);
            }
        }
        _ => {}
    }
}

pub(crate) fn collect_structured_file_touch_object(
    object: &serde_json::Map<String, Value>,
    out: &mut Vec<FileTouchDraft>,
    inherited_kind: Option<FileChangeKind>,
) {
    let inferred_kind = inferred_file_change_kind(object);
    let change_kind = inherited_kind.unwrap_or(inferred_kind);
    let old_path = object.iter().find_map(|(key, value)| {
        is_old_file_path_key(key)
            .then(|| value.as_str())
            .flatten()
            .and_then(normalize_file_path)
    });
    for (key, value) in object {
        if !is_file_path_key(key) {
            continue;
        }
        let Some(raw_path) = value.as_str() else {
            continue;
        };
        if normalized_key(key) == "uri" && !raw_path.trim().starts_with("file://") {
            continue;
        }
        let Some(path) = normalize_file_path(raw_path) else {
            continue;
        };
        out.push(FileTouchDraft {
            path,
            old_path: old_path.clone(),
            change_kind: Some(change_kind),
            confidence: Confidence::High,
            metadata: json!({
                "source": "structured_provider_payload",
                "path_key": key,
            }),
        });
    }
}

pub(crate) fn object_operation_hint_kind(
    object: &serde_json::Map<String, Value>,
) -> Option<FileChangeKind> {
    object
        .iter()
        .any(|(key, value)| {
            matches!(
                normalized_key(key).as_str(),
                "tool" | "name" | "action" | "command" | "operation" | "type"
            ) && value.as_str().is_some_and(|text| !text.trim().is_empty())
        })
        .then(|| inferred_file_change_kind(object))
        .filter(|kind| *kind != FileChangeKind::Unknown)
}

pub(crate) fn inferred_file_change_kind(object: &serde_json::Map<String, Value>) -> FileChangeKind {
    let mut haystack = String::new();
    for (key, value) in object {
        haystack.push_str(&key.to_ascii_lowercase());
        haystack.push(' ');
        if matches!(
            key.to_ascii_lowercase().as_str(),
            "tool" | "name" | "action" | "command" | "operation" | "type"
        ) {
            if let Some(text) = value.as_str() {
                haystack.push_str(&text.to_ascii_lowercase());
                haystack.push(' ');
            }
        }
    }
    if haystack.contains("rename") || haystack.contains("move") {
        FileChangeKind::Renamed
    } else if haystack.contains("delete") || haystack.contains("remove") {
        FileChangeKind::Deleted
    } else if haystack.contains("create") || haystack.contains("write") || haystack.contains("add")
    {
        FileChangeKind::Created
    } else if haystack.contains("read") || haystack.contains("view") || haystack.contains("open") {
        FileChangeKind::Read
    } else if object.values().any(value_looks_like_file_content)
        || haystack.contains("edit")
        || haystack.contains("patch")
        || haystack.contains("replace")
        || haystack.contains("update")
    {
        FileChangeKind::Modified
    } else {
        FileChangeKind::Unknown
    }
}

pub(crate) fn value_looks_like_file_content(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.contains('\n')
            || text.len() > 120
            || text.contains("*** Begin Patch")
            || text.contains("@@")
    })
}

pub(crate) fn is_file_path_key(key: &str) -> bool {
    matches!(
        normalized_key(key).as_str(),
        "path"
            | "file"
            | "filepath"
            | "filename"
            | "targetfile"
            | "targetpath"
            | "relativepath"
            | "absolutepath"
            | "uri"
            | "destinationfile"
            | "destinationpath"
    )
}

pub(crate) fn is_old_file_path_key(key: &str) -> bool {
    matches!(
        normalized_key(key).as_str(),
        "oldpath" | "frompath" | "sourcepath" | "originalpath" | "previouspath"
    )
}

pub(crate) fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

pub(crate) fn normalize_file_path(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    let trimmed = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    if !looks_like_file_path(trimmed) {
        return None;
    }
    Some(trimmed.to_owned())
}

pub(crate) fn looks_like_file_path(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 512
        || value.contains('\n')
        || value.contains('\r')
        || value.contains("://")
        || value.contains("[REDACTED")
        || value.starts_with('{')
        || value.starts_with('[')
    {
        return false;
    }
    value.contains('/')
        || value.contains('\\')
        || value.starts_with('.')
        || value.rsplit(['/', '\\']).next().is_some_and(|name| {
            name.rsplit_once('.').is_some_and(|(stem, ext)| {
                !stem.is_empty()
                    && !ext.is_empty()
                    && ext.len() <= 12
                    && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
            })
        })
}

pub(crate) fn file_touch_draft(
    path: String,
    old_path: Option<String>,
    change_kind: FileChangeKind,
    confidence: Confidence,
    source: &'static str,
) -> FileTouchDraft {
    FileTouchDraft {
        path,
        old_path,
        change_kind: Some(change_kind),
        confidence,
        metadata: json!({ "source": source }),
    }
}

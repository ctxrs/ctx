use std::collections::BTreeSet;

use ctx_history_core::{Confidence, EventType, FileChangeKind};
use serde_json::{json, Value};

// Legacy packed provider touch identity reserves the low 16 bits for a touch within one event.
// The same bound keeps exact per-event deduplication independent of source cardinality; full-width
// event identities retain this per-event ordinal separately in the imported UUID key.
pub(crate) const MAX_PROVIDER_FILE_TOUCHES_PER_EVENT: usize = 1 << 16;
pub(crate) const MAX_PACKED_PROVIDER_EVENT_INDEX: u64 = u64::MAX >> 16;
const MAX_PROVIDER_FILE_TOUCH_FIELD_NAME_BYTES: usize = 256;
pub(crate) const PROVIDER_FILE_TOUCH_LIMIT_REJECTION: &str =
    "provider event exceeds the 65,536 unique file-touch limit";

pub(crate) struct FileTouchDraft {
    pub(crate) path: String,
    pub(crate) old_path: Option<String>,
    pub(crate) change_kind: Option<FileChangeKind>,
    pub(crate) confidence: Confidence,
    pub(crate) metadata: Value,
}

enum ProviderFileTouchTraversalError<E> {
    Sink(E),
    EventTouchLimitExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderFileTouchVisitOutcome {
    emitted: usize,
    limit_exceeded: bool,
}

impl ProviderFileTouchVisitOutcome {
    #[allow(dead_code)]
    pub(crate) fn emitted(self) -> usize {
        self.emitted
    }

    pub(crate) fn limit_exceeded(self) -> bool {
        self.limit_exceeded
    }
}

pub(crate) fn visit_all_file_touch_drafts<E>(
    raw_value: &Value,
    mut visit: impl FnMut(FileTouchDraft) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    visit_patch_file_touch_drafts(raw_value, &mut visit)?;
    visit_structured_file_touch_drafts(raw_value, &mut visit)
}

pub(crate) fn visit_provider_file_touch_drafts_with_limit<E>(
    raw_value: &Value,
    include_structured_touches: bool,
    touch_limit: usize,
    mut visit: impl FnMut((u64, FileTouchDraft)) -> std::result::Result<(), E>,
) -> std::result::Result<ProviderFileTouchVisitOutcome, E> {
    let mut seen = BTreeSet::new();
    let mut emitted = 0_usize;
    let mut limit_exceeded = false;
    let mut emit = |draft: FileTouchDraft| {
        let key = (
            draft.path.clone(),
            draft.old_path.clone(),
            draft.change_kind.map(|kind| kind.as_str().to_owned()),
        );
        if seen.contains(&key) {
            return Ok(());
        }
        if emitted == touch_limit {
            return Err(ProviderFileTouchTraversalError::EventTouchLimitExceeded);
        }
        seen.insert(key);
        visit((emitted as u64, draft)).map_err(ProviderFileTouchTraversalError::Sink)?;
        emitted += 1;
        Ok(())
    };
    let found_patch = match visit_patch_file_touch_drafts(raw_value, &mut emit) {
        Ok(found_patch) => found_patch,
        Err(ProviderFileTouchTraversalError::Sink(error)) => return Err(error),
        Err(ProviderFileTouchTraversalError::EventTouchLimitExceeded) => {
            limit_exceeded = true;
            true
        }
    };
    if !limit_exceeded && !found_patch && include_structured_touches {
        match visit_structured_file_touch_drafts(raw_value, &mut emit) {
            Ok(()) => {}
            Err(ProviderFileTouchTraversalError::Sink(error)) => return Err(error),
            Err(ProviderFileTouchTraversalError::EventTouchLimitExceeded) => {
                limit_exceeded = true;
            }
        }
    }
    Ok(ProviderFileTouchVisitOutcome {
        emitted,
        limit_exceeded,
    })
}

pub(crate) fn event_type_supports_structured_file_touches(event_type: EventType) -> bool {
    matches!(event_type, EventType::ToolCall | EventType::FileTouched)
}

fn visit_patch_file_touch_drafts<E>(
    value: &Value,
    visit: &mut impl FnMut(FileTouchDraft) -> std::result::Result<(), E>,
) -> std::result::Result<bool, E> {
    match value {
        Value::String(text) if text.contains("*** Begin Patch") => {
            visit_apply_patch_file_touch_drafts(text, visit)
        }
        Value::String(_) | Value::Null | Value::Bool(_) | Value::Number(_) => Ok(false),
        Value::Array(items) => {
            let mut found = false;
            for item in items {
                found |= visit_patch_file_touch_drafts(item, visit)?;
            }
            Ok(found)
        }
        Value::Object(object) => {
            let mut found = false;
            for value in object.values() {
                found |= visit_patch_file_touch_drafts(value, visit)?;
            }
            Ok(found)
        }
    }
}

fn visit_apply_patch_file_touch_drafts<E>(
    patch: &str,
    visit: &mut impl FnMut(FileTouchDraft) -> std::result::Result<(), E>,
) -> std::result::Result<bool, E> {
    let mut found = false;
    let mut pending_update: Option<String> = None;
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            found |= visit_pending_patch_update(&mut pending_update, visit)?;
            if let Some(path) = normalize_file_path(path) {
                visit(file_touch_draft(
                    path,
                    None,
                    FileChangeKind::Created,
                    Confidence::Explicit,
                    "apply_patch_add",
                ))?;
                found = true;
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            found |= visit_pending_patch_update(&mut pending_update, visit)?;
            pending_update = normalize_file_path(path);
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            found |= visit_pending_patch_update(&mut pending_update, visit)?;
            if let Some(path) = normalize_file_path(path) {
                visit(file_touch_draft(
                    path,
                    None,
                    FileChangeKind::Deleted,
                    Confidence::Explicit,
                    "apply_patch_delete",
                ))?;
                found = true;
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Move to: ") {
            let old_path = pending_update.take();
            if let Some(path) = normalize_file_path(path) {
                visit(file_touch_draft(
                    path,
                    old_path,
                    FileChangeKind::Renamed,
                    Confidence::Explicit,
                    "apply_patch_move",
                ))?;
                found = true;
            }
        }
    }
    found |= visit_pending_patch_update(&mut pending_update, visit)?;
    Ok(found)
}

fn visit_pending_patch_update<E>(
    pending_update: &mut Option<String>,
    visit: &mut impl FnMut(FileTouchDraft) -> std::result::Result<(), E>,
) -> std::result::Result<bool, E> {
    let Some(path) = pending_update.take() else {
        return Ok(false);
    };
    visit(file_touch_draft(
        path,
        None,
        FileChangeKind::Modified,
        Confidence::Explicit,
        "apply_patch_update",
    ))?;
    Ok(true)
}

fn visit_structured_file_touch_drafts<E>(
    value: &Value,
    visit: &mut impl FnMut(FileTouchDraft) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    visit_structured_file_touch_drafts_with_context(value, visit, None)
}

fn visit_structured_file_touch_drafts_with_context<E>(
    value: &Value,
    visit: &mut impl FnMut(FileTouchDraft) -> std::result::Result<(), E>,
    inherited_kind: Option<FileChangeKind>,
) -> std::result::Result<(), E> {
    match value {
        Value::Array(items) => {
            for item in items {
                visit_structured_file_touch_drafts_with_context(item, visit, inherited_kind)?;
            }
        }
        Value::Object(object) => {
            let operation_kind = object_operation_hint_kind(object);
            let object_kind = operation_kind.or(inherited_kind);
            visit_structured_file_touch_object(object, visit, object_kind)?;
            for value in object.values() {
                visit_structured_file_touch_drafts_with_context(value, visit, object_kind)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn visit_structured_file_touch_object<E>(
    object: &serde_json::Map<String, Value>,
    visit: &mut impl FnMut(FileTouchDraft) -> std::result::Result<(), E>,
    inherited_kind: Option<FileChangeKind>,
) -> std::result::Result<(), E> {
    let inferred_kind = inferred_file_change_kind(object);
    let change_kind = inherited_kind.unwrap_or(inferred_kind);
    let old_path = object.iter().find_map(|(key, value)| {
        canonical_old_file_path_key(key)
            .and(value.as_str())
            .and_then(normalize_file_path)
    });
    for (key, value) in object {
        let Some(path_key) = canonical_file_path_key(key) else {
            continue;
        };
        let Some(raw_path) = value.as_str() else {
            continue;
        };
        if path_key == "uri" && !raw_path.trim().starts_with("file://") {
            continue;
        }
        let Some(path) = normalize_file_path(raw_path) else {
            continue;
        };
        visit(FileTouchDraft {
            path,
            old_path: old_path.clone(),
            change_kind: Some(change_kind),
            confidence: Confidence::High,
            metadata: json!({
                "source": "structured_provider_payload",
                "path_key": path_key,
            }),
        })?;
    }
    Ok(())
}

pub(crate) fn object_operation_hint_kind(
    object: &serde_json::Map<String, Value>,
) -> Option<FileChangeKind> {
    object
        .iter()
        .any(|(key, value)| {
            matches!(
                bounded_normalized_key(key, 64).as_deref(),
                Some("tool" | "name" | "action" | "command" | "operation" | "type")
            ) && value.as_str().is_some_and(|text| !text.trim().is_empty())
        })
        .then(|| inferred_file_change_kind(object))
        .filter(|kind| *kind != FileChangeKind::Unknown)
}

pub(crate) fn inferred_file_change_kind(object: &serde_json::Map<String, Value>) -> FileChangeKind {
    let mut haystack = String::new();
    for (key, value) in object {
        let Some(normalized_key) = bounded_normalized_key(key, 64) else {
            continue;
        };
        haystack.push_str(&normalized_key);
        haystack.push(' ');
        if matches!(
            normalized_key.as_str(),
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

fn canonical_file_path_key(key: &str) -> Option<&'static str> {
    match bounded_normalized_key(key, "destinationpath".len())?.as_str() {
        "path" => Some("path"),
        "file" => Some("file"),
        "filepath" => Some("filepath"),
        "filename" => Some("filename"),
        "targetfile" => Some("targetfile"),
        "targetpath" => Some("targetpath"),
        "relativepath" => Some("relativepath"),
        "absolutepath" => Some("absolutepath"),
        "uri" => Some("uri"),
        "destinationfile" => Some("destinationfile"),
        "destinationpath" => Some("destinationpath"),
        _ => None,
    }
}

fn canonical_old_file_path_key(key: &str) -> Option<&'static str> {
    match bounded_normalized_key(key, "originalpath".len())?.as_str() {
        "oldpath" => Some("oldpath"),
        "frompath" => Some("frompath"),
        "sourcepath" => Some("sourcepath"),
        "originalpath" => Some("originalpath"),
        "previouspath" => Some("previouspath"),
        _ => None,
    }
}

fn bounded_normalized_key(key: &str, max_bytes: usize) -> Option<String> {
    if key.len() > MAX_PROVIDER_FILE_TOUCH_FIELD_NAME_BYTES {
        return None;
    }
    let mut normalized = String::with_capacity(key.len().min(max_bytes));
    for byte in key.bytes().filter(u8::is_ascii_alphanumeric) {
        if normalized.len() == max_bytes {
            return None;
        }
        normalized.push(char::from(byte.to_ascii_lowercase()));
    }
    Some(normalized)
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

#[cfg(test)]
#[path = "file_touches_tests.rs"]
mod tests;

use std::{collections::BTreeSet, convert::Infallible};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, Confidence, EventType, FileChangeKind, ProviderEventEnvelope,
};
use serde_json::{json, Value};

use crate::ProviderFileTouchedEnvelope;

// Legacy packed provider touch identity reserves the low 16 bits for a touch within one event.
// The same bound keeps exact per-event deduplication independent of source cardinality; full-width
// event identities retain this per-event ordinal separately in the imported UUID key.
pub(crate) const MAX_PROVIDER_FILE_TOUCHES_PER_EVENT: usize = 1 << 16;
pub(crate) const MAX_PACKED_PROVIDER_EVENT_INDEX: u64 = u64::MAX >> 16;
pub(crate) const PROVIDER_FILE_TOUCH_LIMIT_REJECTION: &str =
    "provider event exceeds the 65,536 unique file-touch limit";

pub(crate) struct ProviderFileTouchCollection {
    touches: Vec<(usize, ProviderFileTouchedEnvelope)>,
    outcome: ProviderFileTouchVisitOutcome,
}

impl ProviderFileTouchCollection {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<(usize, ProviderFileTouchedEnvelope)>,
        ProviderFileTouchVisitOutcome,
    ) {
        (self.touches, self.outcome)
    }
}

pub(crate) struct FileTouchDraft {
    pub(crate) path: String,
    pub(crate) old_path: Option<String>,
    pub(crate) change_kind: Option<FileChangeKind>,
    pub(crate) confidence: Confidence,
    pub(crate) metadata: Value,
}

pub(crate) struct ProviderFileTouchSourceContext<'a> {
    provider: CaptureProvider,
    provider_session_id: &'a str,
    source_format: &'a str,
    raw_source_path: Option<&'a str>,
    source_root: Option<&'a str>,
}

impl<'a> ProviderFileTouchSourceContext<'a> {
    pub(crate) fn new(
        provider: CaptureProvider,
        provider_session_id: &'a str,
        source_format: &'a str,
        raw_source_path: Option<&'a str>,
        source_root: Option<&'a str>,
    ) -> Self {
        Self {
            provider,
            provider_session_id,
            source_format,
            raw_source_path,
            source_root,
        }
    }

    fn for_event(
        self,
        event: &ProviderEventEnvelope,
        line_number: usize,
    ) -> ProviderFileTouchEnvelopeContext<'a> {
        ProviderFileTouchEnvelopeContext {
            provider: self.provider,
            provider_session_id: self.provider_session_id,
            source_format: self.source_format,
            raw_source_path: self.raw_source_path,
            source_root: self.source_root,
            occurred_at: event.occurred_at,
            provider_event_index: Some(event.provider_event_index),
            provider_touch_base_index: event.provider_event_index << 16,
            line_number,
        }
    }
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
    pub(crate) fn empty() -> Self {
        Self {
            emitted: 0,
            limit_exceeded: false,
        }
    }

    pub(crate) fn emitted(self) -> usize {
        self.emitted
    }

    pub(crate) fn limit_exceeded(self) -> bool {
        self.limit_exceeded
    }
}

pub(crate) fn provider_file_touches_from_event(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
    source_root: Option<&str>,
    event: &ProviderEventEnvelope,
    line_number: usize,
) -> ProviderFileTouchCollection {
    let mut touches = Vec::new();
    let outcome = visit_provider_file_touches_from_event(
        ProviderFileTouchSourceContext::new(
            provider,
            provider_session_id,
            source_format,
            raw_source_path,
            source_root,
        ),
        event,
        line_number,
        |touch| {
            touches.push(touch);
            Ok::<(), Infallible>(())
        },
    )
    .expect("an infallible file-touch sink cannot fail");
    ProviderFileTouchCollection { touches, outcome }
}

pub(crate) fn visit_provider_file_touches_from_event<E>(
    source: ProviderFileTouchSourceContext<'_>,
    event: &ProviderEventEnvelope,
    line_number: usize,
    visit: impl FnMut((usize, ProviderFileTouchedEnvelope)) -> std::result::Result<(), E>,
) -> std::result::Result<ProviderFileTouchVisitOutcome, E> {
    visit_provider_file_touches_from_raw_value(source, &event.payload, event, line_number, visit)
}

#[cfg(test)]
pub(crate) fn provider_file_touches_from_raw_value(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
    raw_value: &Value,
    event: &ProviderEventEnvelope,
    line_number: usize,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    provider_file_touches_from_raw_value_with_source_root(
        provider,
        provider_session_id,
        source_format,
        (raw_source_path, raw_source_path),
        raw_value,
        event,
        line_number,
    )
}

#[cfg(test)]
pub(crate) fn provider_file_touches_from_raw_value_with_source_root(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    source_paths: (Option<&str>, Option<&str>),
    raw_value: &Value,
    event: &ProviderEventEnvelope,
    line_number: usize,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    let mut touches = Vec::new();
    let outcome = visit_provider_file_touches_from_raw_value(
        ProviderFileTouchSourceContext::new(
            provider,
            provider_session_id,
            source_format,
            source_paths.0,
            source_paths.1,
        ),
        raw_value,
        event,
        line_number,
        |touch| {
            touches.push(touch);
            Ok::<(), Infallible>(())
        },
    )
    .expect("an infallible file-touch sink cannot fail");
    assert!(
        !outcome.limit_exceeded(),
        "test helper encountered {PROVIDER_FILE_TOUCH_LIMIT_REJECTION}"
    );
    touches
}

pub(crate) fn visit_provider_file_touches_from_raw_value<E>(
    source: ProviderFileTouchSourceContext<'_>,
    raw_value: &Value,
    event: &ProviderEventEnvelope,
    line_number: usize,
    visit: impl FnMut((usize, ProviderFileTouchedEnvelope)) -> std::result::Result<(), E>,
) -> std::result::Result<ProviderFileTouchVisitOutcome, E> {
    if !matches!(
        event.event_type,
        EventType::ToolCall
            | EventType::ToolOutput
            | EventType::CommandOutput
            | EventType::FileTouched
    ) {
        return Ok(ProviderFileTouchVisitOutcome {
            emitted: 0,
            limit_exceeded: false,
        });
    }

    visit_provider_file_touches_with_context(
        source.for_event(event, line_number),
        raw_value,
        event_type_supports_structured_file_touches(event.event_type),
        visit,
    )
}

pub(crate) fn visit_provider_file_touches_with_context<E>(
    context: ProviderFileTouchEnvelopeContext<'_>,
    raw_value: &Value,
    include_structured_touches: bool,
    visit: impl FnMut((usize, ProviderFileTouchedEnvelope)) -> std::result::Result<(), E>,
) -> std::result::Result<ProviderFileTouchVisitOutcome, E> {
    let mut visitor = ProviderFileTouchVisitor::new(context, visit);
    visitor.visit_raw_value(raw_value, include_structured_touches)?;
    Ok(visitor.finish())
}

pub(crate) fn visit_all_file_touch_drafts<E>(
    raw_value: &Value,
    mut visit: impl FnMut(FileTouchDraft) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    visit_patch_file_touch_drafts(raw_value, &mut visit)?;
    visit_structured_file_touch_drafts(raw_value, &mut visit)
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

struct ProviderFileTouchEmitter<'a, F> {
    context: ProviderFileTouchEnvelopeContext<'a>,
    seen: BTreeSet<(String, Option<String>, Option<String>)>,
    emitted: usize,
    visit: F,
}

pub(crate) struct ProviderFileTouchVisitor<'a, F> {
    emitter: ProviderFileTouchEmitter<'a, F>,
    limit_exceeded: bool,
}

impl<'a, F> ProviderFileTouchVisitor<'a, F> {
    pub(crate) fn new(context: ProviderFileTouchEnvelopeContext<'a>, visit: F) -> Self {
        Self {
            emitter: ProviderFileTouchEmitter::new(context, visit),
            limit_exceeded: false,
        }
    }

    pub(crate) fn finish(self) -> ProviderFileTouchVisitOutcome {
        ProviderFileTouchVisitOutcome {
            emitted: self.emitter.emitted,
            limit_exceeded: self.limit_exceeded,
        }
    }
}

impl<F, E> ProviderFileTouchVisitor<'_, F>
where
    F: FnMut((usize, ProviderFileTouchedEnvelope)) -> std::result::Result<(), E>,
{
    pub(crate) fn visit_raw_value(
        &mut self,
        raw_value: &Value,
        include_structured_touches: bool,
    ) -> std::result::Result<(), E> {
        if self.limit_exceeded {
            return Ok(());
        }
        let found_patch =
            match visit_patch_file_touch_drafts(raw_value, &mut |draft| self.emitter.emit(draft)) {
                Ok(found_patch) => found_patch,
                Err(ProviderFileTouchTraversalError::Sink(error)) => return Err(error),
                Err(ProviderFileTouchTraversalError::EventTouchLimitExceeded) => {
                    self.limit_exceeded = true;
                    return Ok(());
                }
            };
        if !found_patch && include_structured_touches {
            match visit_structured_file_touch_drafts(raw_value, &mut |draft| {
                self.emitter.emit(draft)
            }) {
                Ok(()) => {}
                Err(ProviderFileTouchTraversalError::Sink(error)) => return Err(error),
                Err(ProviderFileTouchTraversalError::EventTouchLimitExceeded) => {
                    self.limit_exceeded = true;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn visit_drafts(
        &mut self,
        drafts: impl IntoIterator<Item = FileTouchDraft>,
    ) -> std::result::Result<(), E> {
        if self.limit_exceeded {
            return Ok(());
        }
        for draft in drafts {
            match self.emitter.emit(draft) {
                Ok(()) => {}
                Err(ProviderFileTouchTraversalError::Sink(error)) => return Err(error),
                Err(ProviderFileTouchTraversalError::EventTouchLimitExceeded) => {
                    self.limit_exceeded = true;
                    break;
                }
            }
        }
        Ok(())
    }
}

impl<'a, F> ProviderFileTouchEmitter<'a, F> {
    fn new(context: ProviderFileTouchEnvelopeContext<'a>, visit: F) -> Self {
        Self {
            context,
            seen: BTreeSet::new(),
            emitted: 0,
            visit,
        }
    }
}

impl<F, E> ProviderFileTouchEmitter<'_, F>
where
    F: FnMut((usize, ProviderFileTouchedEnvelope)) -> std::result::Result<(), E>,
{
    fn emit(
        &mut self,
        draft: FileTouchDraft,
    ) -> std::result::Result<(), ProviderFileTouchTraversalError<E>> {
        let key = (
            draft.path.clone(),
            draft.old_path.clone(),
            draft.change_kind.map(|kind| kind.as_str().to_owned()),
        );
        if self.seen.contains(&key) {
            return Ok(());
        }
        if self.emitted == MAX_PROVIDER_FILE_TOUCHES_PER_EVENT {
            return Err(ProviderFileTouchTraversalError::EventTouchLimitExceeded);
        }
        self.seen.insert(key);
        // Preserve the historical packed identity for event indices that fit beside the 16-bit
        // touch ordinal. Hash-indexed providers can legitimately use the full u64 range; their
        // envelope carries the full event index and the ordinal separately, and the importer uses
        // both when deriving a collision-free touch UUID.
        let provider_touch_index = match self.context.provider_event_index {
            Some(index) if index > MAX_PACKED_PROVIDER_EVENT_INDEX => self.emitted as u64,
            _ => self.context.provider_touch_base_index | (self.emitted as u64),
        };
        (self.visit)((
            self.context.line_number,
            ProviderFileTouchedEnvelope {
                provider: self.context.provider,
                provider_session_id: self.context.provider_session_id.to_owned(),
                provider_touch_index,
                provider_event_index: self.context.provider_event_index,
                raw_source_path: self.context.raw_source_path.map(str::to_owned),
                source_root: self.context.source_root.map(str::to_owned),
                path: draft.path,
                change_kind: draft.change_kind,
                old_path: draft.old_path,
                line_count_delta: None,
                confidence: draft.confidence,
                occurred_at: self.context.occurred_at,
                source_format: self.context.source_format.to_owned(),
                metadata: draft.metadata,
            },
        ))
        .map_err(ProviderFileTouchTraversalError::Sink)?;
        self.emitted += 1;
        Ok(())
    }
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
        visit(FileTouchDraft {
            path,
            old_path: old_path.clone(),
            change_kind: Some(change_kind),
            confidence: Confidence::High,
            metadata: json!({
                "source": "structured_provider_payload",
                "path_key": key,
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

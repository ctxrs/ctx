#[allow(unused_imports)]
use super::*;

pub(crate) fn forgecode_metric_file_touches(
    row: &ForgeCodeConversationRow,
    metrics: &Value,
    raw_source_path: &str,
    fallback: DateTime<Utc>,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    let occurred_at = metrics
        .get("started_at")
        .map(|value| provider_timestamp_value(Some(value), fallback))
        .unwrap_or(fallback);
    let mut touches = Vec::new();
    let mut seen = BTreeSet::<(String, &'static str)>::new();

    if let Some(files_changed) = metrics.get("files_changed").and_then(Value::as_object) {
        let mut entries = files_changed.iter().collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(right.0));
        for (path, operation_value) in entries {
            let Some(operation) = forgecode_metric_operation(operation_value) else {
                continue;
            };
            let tool = operation
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("write");
            let change_kind = forgecode_metric_change_kind(tool);
            if !seen.insert((path.clone(), change_kind.as_str())) {
                continue;
            }
            let lines_added = operation.get("lines_added").and_then(forgecode_json_i64);
            let lines_removed = operation.get("lines_removed").and_then(forgecode_json_i64);
            let line_count_delta = match (lines_added, lines_removed) {
                (Some(added), Some(removed)) => Some(added.saturating_sub(removed)),
                (Some(added), None) => Some(added),
                (None, Some(removed)) => Some(removed.saturating_neg()),
                _ => None,
            };
            let touch_index = 0x0400_0000_0000_u64.saturating_add(touches.len() as u64);
            touches.push((
                provider_line_from_index(touch_index),
                ProviderFileTouchedEnvelope {
                    provider: CaptureProvider::ForgeCode,
                    provider_session_id: row.conversation_id.clone(),
                    provider_touch_index: touch_index,
                    provider_event_index: None,
                    raw_source_path: Some(raw_source_path.to_owned()),
                    path: path.clone(),
                    change_kind: Some(change_kind),
                    old_path: None,
                    line_count_delta,
                    confidence: Confidence::Explicit,
                    occurred_at,
                    source_format: FORGECODE_SQLITE_SOURCE_FORMAT.to_owned(),
                    metadata: json!({
                        "source": "forgecode_metrics_files_changed",
                        "tool": tool,
                        "lines_added": lines_added,
                        "lines_removed": lines_removed,
                        "content_hash": operation.get("content_hash").and_then(Value::as_str),
                    }),
                },
            ));
        }
    }

    if let Some(files_accessed) = metrics.get("files_accessed").and_then(Value::as_array) {
        let mut paths = files_accessed
            .iter()
            .filter_map(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        paths.dedup();
        for path in paths {
            if !seen.insert((path.to_owned(), FileChangeKind::Read.as_str())) {
                continue;
            }
            let touch_index = 0x0500_0000_0000_u64.saturating_add(touches.len() as u64);
            touches.push((
                provider_line_from_index(touch_index),
                ProviderFileTouchedEnvelope {
                    provider: CaptureProvider::ForgeCode,
                    provider_session_id: row.conversation_id.clone(),
                    provider_touch_index: touch_index,
                    provider_event_index: None,
                    raw_source_path: Some(raw_source_path.to_owned()),
                    path: path.to_owned(),
                    change_kind: Some(FileChangeKind::Read),
                    old_path: None,
                    line_count_delta: None,
                    confidence: Confidence::Explicit,
                    occurred_at,
                    source_format: FORGECODE_SQLITE_SOURCE_FORMAT.to_owned(),
                    metadata: json!({
                        "source": "forgecode_metrics_files_accessed",
                    }),
                },
            ));
        }
    }

    touches
}

pub(crate) fn forgecode_metric_operation(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(_) => Some(value),
        Value::Array(items) => items.iter().rev().find(|item| item.is_object()),
        _ => None,
    }
}

pub(crate) fn forgecode_metric_change_kind(tool: &str) -> FileChangeKind {
    match tool.to_ascii_lowercase().as_str() {
        "read" => FileChangeKind::Read,
        "patch" | "edit" | "update" | "write" => FileChangeKind::Modified,
        "delete" | "remove" => FileChangeKind::Deleted,
        "create" | "add" => FileChangeKind::Created,
        _ => FileChangeKind::Unknown,
    }
}

pub(crate) fn forgecode_json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

pub(crate) fn forgecode_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    goose_timestamp(raw, fallback)
}

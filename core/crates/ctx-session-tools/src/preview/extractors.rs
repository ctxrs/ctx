use std::collections::HashSet;

use serde_json::Value;

use super::*;

#[derive(Default, Debug, Clone)]
pub(super) struct DiffStats {
    pub(super) added: usize,
    pub(super) removed: usize,
    pub(super) files: usize,
}

pub(super) fn push_preview_line(out: &mut Vec<String>, truncated: &mut bool, line: &str) {
    if line.chars().count() > TOOL_PREVIEW_MAX_LINE_CHARS {
        *truncated = true;
        out.push(line.chars().take(TOOL_PREVIEW_MAX_LINE_CHARS).collect());
    } else {
        out.push(line.to_string());
    }
}

pub(super) fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

pub(super) fn diff_stats_from_patch(patch: &str) -> Option<DiffStats> {
    let mut stats = DiffStats::default();
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            stats.files += 1;
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
            continue;
        }
        if line.starts_with('+') {
            stats.added += 1;
            continue;
        }
        if line.starts_with('-') {
            stats.removed += 1;
        }
    }
    if stats.files == 0 && (stats.added > 0 || stats.removed > 0) {
        stats.files = 1;
    }
    if stats.files == 0 && stats.added == 0 && stats.removed == 0 {
        None
    } else {
        Some(stats)
    }
}

pub(super) fn extract_old_new_text(
    obj: &serde_json::Map<String, Value>,
) -> (Option<&str>, Option<&str>) {
    let old_text = obj
        .get("oldText")
        .or_else(|| obj.get("old_text"))
        .or_else(|| obj.get("old"))
        .or_else(|| obj.get("before"))
        .and_then(|v| v.as_str());
    let new_text = obj
        .get("newText")
        .or_else(|| obj.get("new_text"))
        .or_else(|| obj.get("new"))
        .or_else(|| obj.get("text"))
        .or_else(|| obj.get("after"))
        .and_then(|v| v.as_str());
    (old_text, new_text)
}

pub(super) fn diff_stats_from_edits(edits: &[Value]) -> Option<DiffStats> {
    let mut stats = DiffStats::default();
    let mut files = HashSet::new();
    for edit in edits {
        let Some(obj) = edit.as_object() else {
            continue;
        };
        for key in [
            "path",
            "file",
            "file_path",
            "filePath",
            "filepath",
            "target",
        ] {
            if let Some(Value::String(path)) = obj.get(key) {
                files.insert(path.clone());
            }
        }
        let (old_text, new_text) = extract_old_new_text(obj);
        stats.removed += count_lines(old_text.unwrap_or(""));
        stats.added += count_lines(new_text.unwrap_or(""));
    }
    stats.files = files.len();
    if stats.files == 0 && (stats.added > 0 || stats.removed > 0) {
        stats.files = 1;
    }
    if stats.files == 0 && stats.added == 0 && stats.removed == 0 {
        None
    } else {
        Some(stats)
    }
}

pub(super) fn diff_stats_from_changes(changes: &Value) -> Option<DiffStats> {
    match changes {
        Value::Array(edits) => diff_stats_from_edits(edits),
        Value::String(text) => diff_stats_from_patch(text),
        Value::Object(map) => {
            let mut stats = DiffStats::default();
            let mut files = HashSet::new();
            for (path, entry) in map {
                if !path.trim().is_empty() {
                    files.insert(path.clone());
                }
                if let Some(patch) = extract_patch_text(entry) {
                    if let Some(patch_stats) = diff_stats_from_patch(patch) {
                        stats.added += patch_stats.added;
                        stats.removed += patch_stats.removed;
                        stats.files = stats.files.max(patch_stats.files);
                    }
                    continue;
                }
                if let Some(text) = entry.as_str() {
                    if let Some(patch_stats) = diff_stats_from_patch(text) {
                        stats.added += patch_stats.added;
                        stats.removed += patch_stats.removed;
                        stats.files = stats.files.max(patch_stats.files);
                        continue;
                    }
                }
                if let Some(obj) = entry.as_object() {
                    let (old_text, new_text) = extract_old_new_text(obj);
                    stats.removed += count_lines(old_text.unwrap_or(""));
                    stats.added += count_lines(new_text.unwrap_or(""));
                }
            }
            stats.files = stats.files.max(files.len());
            if stats.files == 0 && (stats.added > 0 || stats.removed > 0) {
                stats.files = 1;
            }
            if stats.files == 0 && stats.added == 0 && stats.removed == 0 {
                None
            } else {
                Some(stats)
            }
        }
        _ => None,
    }
}

pub(super) fn diff_stats_from_content_blocks(blocks: &[&Value]) -> Option<DiffStats> {
    let mut stats = DiffStats::default();
    let mut files = HashSet::new();
    for block in blocks {
        let candidate = block.get("content").unwrap_or(block);
        if let Some(obj) = candidate.as_object() {
            for key in [
                "path",
                "file",
                "file_path",
                "filePath",
                "filepath",
                "target",
            ] {
                if let Some(Value::String(path)) = obj.get(key) {
                    files.insert(path.clone());
                }
            }
            if let Some(patch) = extract_patch_text(candidate) {
                if let Some(patch_stats) = diff_stats_from_patch(patch) {
                    stats.added += patch_stats.added;
                    stats.removed += patch_stats.removed;
                    stats.files = stats.files.max(patch_stats.files);
                    continue;
                }
            }
            let (old_text, new_text) = extract_old_new_text(obj);
            stats.removed += count_lines(old_text.unwrap_or(""));
            stats.added += count_lines(new_text.unwrap_or(""));
        } else if let Some(text) = candidate.as_str() {
            if let Some(patch_stats) = diff_stats_from_patch(text) {
                stats.added += patch_stats.added;
                stats.removed += patch_stats.removed;
                stats.files = stats.files.max(patch_stats.files);
            }
        }
    }
    stats.files = stats.files.max(files.len());
    if stats.files == 0 && (stats.added > 0 || stats.removed > 0) {
        stats.files = 1;
    }
    if stats.files == 0 && stats.added == 0 && stats.removed == 0 {
        None
    } else {
        Some(stats)
    }
}

pub(super) fn extract_patch_text(input: &Value) -> Option<&str> {
    if let Some(text) = input.as_str() {
        return Some(text);
    }
    input
        .get("patch")
        .or_else(|| input.get("diff"))
        .or_else(|| input.get("patch_text"))
        .or_else(|| input.get("unified_diff"))
        .and_then(|v| v.as_str())
}

pub(super) fn extract_patch_text_from_changes(changes: &Value) -> Option<String> {
    match changes {
        Value::String(text) => Some(text.to_string()),
        Value::Array(items) => {
            for item in items {
                if let Some(patch) = extract_patch_text(item) {
                    return Some(patch.to_string());
                }
                if let Some(text) = item.as_str() {
                    return Some(text.to_string());
                }
            }
            None
        }
        Value::Object(map) => {
            for (_path, entry) in map {
                if let Some(patch) = extract_patch_text(entry) {
                    return Some(patch.to_string());
                }
                if let Some(text) = entry.as_str() {
                    return Some(text.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

pub(super) fn extract_content_blocks(update: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    for candidate in [update.get("content"), update.pointer("/toolCall/content")] {
        if let Some(items) = candidate.and_then(|v| v.as_array()) {
            for item in items {
                out.push(item);
            }
        }
    }
    out
}

pub(super) fn collect_paths_from_value(value: &Value, paths: &mut Vec<String>) {
    match value {
        Value::String(path) => {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                paths.push(trimmed.to_string());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_paths_from_value(item, paths);
            }
        }
        Value::Object(obj) => {
            for key in [
                "path",
                "file",
                "filename",
                "file_path",
                "filePath",
                "filepath",
                "target",
                "uri",
            ] {
                if let Some(Value::String(path)) = obj.get(key) {
                    let trimmed = path.trim();
                    if !trimmed.is_empty() {
                        paths.push(trimmed.to_string());
                    }
                }
            }
            for key in ["paths", "files", "file_paths", "filePaths"] {
                if let Some(value) = obj.get(key) {
                    collect_paths_from_value(value, paths);
                }
            }
            if let Some(Value::Array(cmds)) = obj.get("parsed_cmd") {
                for cmd in cmds {
                    if let Some(path) = cmd.get("path") {
                        collect_paths_from_value(path, paths);
                    }
                }
            }
        }
        _ => {}
    }
}

pub(super) fn collect_paths_from_changes(value: &Value, paths: &mut Vec<String>) {
    let Some(changes) = value.as_object() else {
        return;
    };
    for (path, _) in changes {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            paths.push(trimmed.to_string());
        }
    }
}

pub(super) fn dedupe_paths(paths: &mut Vec<String>) {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in paths.drain(..) {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    *paths = out;
}

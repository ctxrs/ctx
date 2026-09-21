//! Exact, bounded interpretation of provider-native file tool schemas.

use ctx_history_core::{ActivityInvocation, ActivityJsonCapture, CoreRecord};
use serde_json::{Map, Value};

use crate::{
    LiteralRepositoryFileInvocation, RepositoryFileInvocationKind,
    RepositoryFileInvocationTextRange,
};

mod codex;

#[cfg(test)]
mod tests;

const MAX_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_EXACT_INVOCATIONS: usize = 64;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_TARGET_BYTES: usize = 64 * 1024;
const MAX_TOOL_NAME_BYTES: usize = 512;
const MAX_CURSOR_INVOCATIONS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FileInvocationExtraction {
    NotApplicable,
    Exact(Vec<LiteralRepositoryFileInvocation>),
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExactTarget {
    path: String,
    prior_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetExtraction {
    NotApplicable,
    Exact {
        kind: RepositoryFileInvocationKind,
        targets: Vec<ExactTarget>,
    },
    Rejected,
}

pub(super) fn exact_file_invocations(
    record: &CoreRecord,
    invocation: &ActivityInvocation,
) -> FileInvocationExtraction {
    if invocation.tool.trim().is_empty() || invocation.tool.len() > MAX_TOOL_NAME_BYTES {
        return FileInvocationExtraction::Rejected;
    }

    let provider = record.source.provider();
    let provider_is_supported = matches!(
        provider,
        "codex" | "claude" | "cursor" | "gemini" | "openclaw" | "opencode"
    );
    if !provider_is_supported {
        return FileInvocationExtraction::NotApplicable;
    }

    let arguments = match &invocation.arguments {
        ActivityJsonCapture::Present { value } => value,
        ActivityJsonCapture::Absent
        | ActivityJsonCapture::Unavailable
        | ActivityJsonCapture::Omitted { .. } => {
            return if recognized_file_tool(provider, &invocation.tool) {
                FileInvocationExtraction::Rejected
            } else {
                FileInvocationExtraction::NotApplicable
            };
        }
    };
    if encoded_json_len_within(arguments, MAX_ARGUMENT_BYTES).is_none() {
        return FileInvocationExtraction::Rejected;
    }

    if invocation.protocol.is_some() || invocation.server.is_some() {
        return if recognized_file_tool(provider, &invocation.tool)
            || target_bearing_value(arguments)
        {
            FileInvocationExtraction::Rejected
        } else {
            FileInvocationExtraction::NotApplicable
        };
    }

    if provider == "codex" {
        return codex::extract(record, invocation, arguments);
    }

    let extracted = match provider {
        "claude" => claude_targets(&invocation.tool, arguments),
        "cursor" => cursor_targets(&invocation.tool, arguments),
        "gemini" => gemini_targets(&invocation.tool, arguments),
        "openclaw" | "opencode" => common_native_targets(&invocation.tool, arguments),
        _ => TargetExtraction::NotApplicable,
    };
    let (kind, targets) = match extracted {
        TargetExtraction::Exact { kind, targets } => (kind, targets),
        TargetExtraction::NotApplicable => return FileInvocationExtraction::NotApplicable,
        TargetExtraction::Rejected => return FileInvocationExtraction::Rejected,
    };
    if targets.len() > MAX_EXACT_INVOCATIONS || total_target_bytes(&targets).is_none() {
        return FileInvocationExtraction::Rejected;
    }

    let Some(operation_ordinal) = provider_operation_ordinal(provider, record.event_sequence)
    else {
        return FileInvocationExtraction::Rejected;
    };
    let normalized_text_range =
        exact_argument_range(record.content.normalized_body.as_deref(), arguments);
    FileInvocationExtraction::Exact(
        targets
            .into_iter()
            .map(|target| LiteralRepositoryFileInvocation {
                operation_ordinal,
                path: target.path,
                prior_path: target.prior_path,
                kind,
                tool_name: Some(invocation.tool.clone()),
                normalized_text_range,
            })
            .collect(),
    )
}

fn provider_operation_ordinal(provider: &str, event_sequence: u64) -> Option<u32> {
    match provider {
        "claude" | "cursor" => u32::try_from(event_sequence & u64::from(u16::MAX)).ok(),
        "gemini" | "openclaw" => u32::try_from(event_sequence & u64::from(u32::MAX)).ok(),
        // Historical Codex native operations and OpenCode logical rows use
        // call-local ordinals. Codex patch extraction assigns its own members.
        "codex" | "opencode" => Some(0),
        _ => None,
    }
}

fn claude_targets(tool: &str, arguments: &Value) -> TargetExtraction {
    let kind = match tool {
        "Read" => RepositoryFileInvocationKind::Read,
        "Edit" => RepositoryFileInvocationKind::Modify,
        "Delete" => RepositoryFileInvocationKind::Delete,
        "Rename" => RepositoryFileInvocationKind::Rename,
        "Create" => RepositoryFileInvocationKind::Create,
        "Write" => RepositoryFileInvocationKind::Write,
        _ => return unknown_tool(arguments),
    };
    let Some(object) = arguments.as_object() else {
        return TargetExtraction::Rejected;
    };
    let targets = if kind == RepositoryFileInvocationKind::Rename {
        exact_rename_target(
            object,
            &["old_path", "oldPath"],
            &["new_path", "newPath"],
            &[
                "file_path",
                "filePath",
                "path",
                "file_paths",
                "filePaths",
                "paths",
            ],
        )
        .map(|target| vec![target])
    } else {
        if has_any(object, &["old_path", "oldPath", "new_path", "newPath"]) {
            return TargetExtraction::Rejected;
        }
        exact_direct_targets(
            object,
            &["file_path", "filePath", "path"],
            &["file_paths", "filePaths", "paths"],
            MAX_EXACT_INVOCATIONS,
        )
    };
    targets.map_or(TargetExtraction::Rejected, |targets| {
        TargetExtraction::Exact { kind, targets }
    })
}

fn cursor_targets(tool: &str, arguments: &Value) -> TargetExtraction {
    let kind = match tool {
        "read_file" => RepositoryFileInvocationKind::Read,
        "write_file" => RepositoryFileInvocationKind::Write,
        _ => return unknown_tool(arguments),
    };
    let Some(object) = arguments.as_object() else {
        return TargetExtraction::Rejected;
    };
    if has_any(
        object,
        &[
            "old_path",
            "oldPath",
            "new_path",
            "newPath",
            "files",
            "file_paths",
            "filePaths",
        ],
    ) {
        return TargetExtraction::Rejected;
    }
    exact_direct_targets(
        object,
        &["path", "file_path", "filePath"],
        &["paths"],
        MAX_CURSOR_INVOCATIONS,
    )
    .map_or(TargetExtraction::Rejected, |targets| {
        TargetExtraction::Exact { kind, targets }
    })
}

fn gemini_targets(tool: &str, arguments: &Value) -> TargetExtraction {
    let kind = match tool {
        "read_file" => RepositoryFileInvocationKind::Read,
        "write_file" => RepositoryFileInvocationKind::Write,
        "replace" => RepositoryFileInvocationKind::Modify,
        _ => return unknown_tool(arguments),
    };
    let Some(object) = arguments.as_object() else {
        return TargetExtraction::Rejected;
    };
    if has_any(
        object,
        &[
            "path", "filePath", "paths", "files", "old_path", "oldPath", "new_path", "newPath",
        ],
    ) {
        return TargetExtraction::Rejected;
    }
    let Some(path) = object.get("file_path").and_then(Value::as_str) else {
        return TargetExtraction::Rejected;
    };
    let Some(path) = bounded_path(path) else {
        return TargetExtraction::Rejected;
    };
    TargetExtraction::Exact {
        kind,
        targets: vec![ExactTarget {
            path,
            prior_path: None,
        }],
    }
}

fn common_native_targets(tool: &str, arguments: &Value) -> TargetExtraction {
    let kind = match tool {
        "read" | "read_file" => RepositoryFileInvocationKind::Read,
        "edit" | "edit_file" => RepositoryFileInvocationKind::Modify,
        "write" | "write_file" => RepositoryFileInvocationKind::Write,
        "create" | "create_file" => RepositoryFileInvocationKind::Create,
        "delete" | "delete_file" => RepositoryFileInvocationKind::Delete,
        "rename" | "rename_file" => RepositoryFileInvocationKind::Rename,
        _ => return unknown_tool(arguments),
    };
    let Some(object) = arguments.as_object() else {
        return TargetExtraction::Rejected;
    };
    let targets = if kind == RepositoryFileInvocationKind::Rename {
        exact_rename_target(
            object,
            &["old_path", "oldPath", "source", "from"],
            &["new_path", "newPath", "destination", "to", "path"],
            &["files", "paths", "file_path", "filePath"],
        )
        .map(|target| vec![target])
    } else {
        if has_any(
            object,
            &[
                "old_path",
                "oldPath",
                "source",
                "from",
                "new_path",
                "newPath",
                "destination",
                "to",
            ],
        ) {
            return TargetExtraction::Rejected;
        }
        exact_direct_targets(
            object,
            &["path", "file_path", "filePath"],
            &["files", "paths"],
            MAX_EXACT_INVOCATIONS,
        )
    };
    targets.map_or(TargetExtraction::Rejected, |targets| {
        TargetExtraction::Exact { kind, targets }
    })
}

fn unknown_tool(arguments: &Value) -> TargetExtraction {
    if target_bearing_value(arguments) {
        TargetExtraction::Rejected
    } else {
        TargetExtraction::NotApplicable
    }
}

fn exact_direct_targets(
    arguments: &Map<String, Value>,
    direct_fields: &[&str],
    multi_fields: &[&str],
    maximum: usize,
) -> Option<Vec<ExactTarget>> {
    let mut present = direct_fields
        .iter()
        .chain(multi_fields)
        .filter_map(|field| arguments.get_key_value(*field));
    let (field, value) = present.next()?;
    if present.next().is_some() {
        return None;
    }

    let paths = if direct_fields.contains(&field.as_str()) {
        vec![bounded_path(value.as_str()?)?]
    } else {
        let values = value.as_array()?;
        if values.is_empty() || values.len() > maximum {
            return None;
        }
        let mut paths = Vec::with_capacity(values.len());
        for value in values {
            let path = bounded_path(value.as_str()?)?;
            if paths.contains(&path) {
                return None;
            }
            paths.push(path);
        }
        paths
    };
    Some(
        paths
            .into_iter()
            .map(|path| ExactTarget {
                path,
                prior_path: None,
            })
            .collect(),
    )
}

fn exact_rename_target(
    arguments: &Map<String, Value>,
    prior_fields: &[&str],
    path_fields: &[&str],
    forbidden_fields: &[&str],
) -> Option<ExactTarget> {
    if has_any(arguments, forbidden_fields) {
        return None;
    }
    let prior_path = exactly_one_path(arguments, prior_fields)?;
    let path = exactly_one_path(arguments, path_fields)?;
    (prior_path != path).then_some(ExactTarget {
        path,
        prior_path: Some(prior_path),
    })
}

fn exactly_one_path(arguments: &Map<String, Value>, fields: &[&str]) -> Option<String> {
    let mut present = fields.iter().filter_map(|field| arguments.get(*field));
    let path = bounded_path(present.next()?.as_str()?)?;
    present.next().is_none().then_some(path)
}

fn bounded_path(path: &str) -> Option<String> {
    (!path.trim().is_empty() && path.len() <= MAX_PATH_BYTES && !path.contains('\0'))
        .then(|| path.to_owned())
}

fn total_target_bytes(targets: &[ExactTarget]) -> Option<usize> {
    let bytes = targets.iter().try_fold(0_usize, |total, target| {
        total
            .checked_add(target.path.len())?
            .checked_add(target.prior_path.as_deref().map_or(0, str::len))
    })?;
    (bytes <= MAX_TARGET_BYTES).then_some(bytes)
}

fn exact_argument_range(
    normalized_body: Option<&str>,
    arguments: &Value,
) -> Option<RepositoryFileInvocationTextRange> {
    let normalized_body = normalized_body?;
    let complete_unit = serde_json::to_string(arguments).ok()?;
    unique_complete_unit_range(normalized_body, &complete_unit)
}

fn unique_complete_unit_range(
    normalized_body: &str,
    complete_unit: &str,
) -> Option<RepositoryFileInvocationTextRange> {
    let mut matches = normalized_body.match_indices(complete_unit);
    let (start, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    checked_text_range(start, start.checked_add(complete_unit.len())?)
}

fn checked_text_range(start: usize, end: usize) -> Option<RepositoryFileInvocationTextRange> {
    (start < end).then_some(RepositoryFileInvocationTextRange {
        start: u32::try_from(start).ok()?,
        end: u32::try_from(end).ok()?,
    })
}

fn recognized_file_tool(provider: &str, tool: &str) -> bool {
    match provider {
        "codex" => matches!(
            tool,
            "read" | "create" | "modify" | "delete" | "rename" | "write" | "apply_patch" | "exec"
        ),
        "claude" => matches!(
            tool,
            "Read" | "Edit" | "Delete" | "Rename" | "Create" | "Write"
        ),
        "cursor" => matches!(tool, "read_file" | "write_file"),
        "gemini" => matches!(tool, "read_file" | "write_file" | "replace"),
        "openclaw" | "opencode" => matches!(
            tool,
            "read"
                | "read_file"
                | "edit"
                | "edit_file"
                | "write"
                | "write_file"
                | "create"
                | "create_file"
                | "delete"
                | "delete_file"
                | "rename"
                | "rename_file"
        ),
        _ => false,
    }
}

fn target_bearing_value(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        has_any(
            object,
            &[
                "path",
                "file_path",
                "filePath",
                "file_paths",
                "filePaths",
                "paths",
                "files",
                "prior_path",
                "old_path",
                "oldPath",
                "new_path",
                "newPath",
                "source",
                "from",
                "destination",
                "to",
                "patch",
            ],
        )
    })
}

fn has_any(arguments: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| arguments.contains_key(*key))
}

fn encoded_json_len_within(value: &Value, maximum: usize) -> Option<usize> {
    struct BoundedCounter {
        bytes: usize,
        maximum: usize,
    }

    impl std::io::Write for BoundedCounter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(buffer.len())
                .filter(|bytes| *bytes <= self.maximum)
                .ok_or_else(|| std::io::Error::other("bounded JSON size exceeded"))?;
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut counter = BoundedCounter { bytes: 0, maximum };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.bytes)
}

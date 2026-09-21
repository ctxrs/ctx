use std::ops::Range;

use ctx_history_core::{ActivityInvocation, CoreRecord};
use serde_json::{Map, Value};

use crate::{
    LiteralRepositoryFileInvocation, MAX_REPOSITORY_CANDIDATES, RepositoryFileInvocationKind,
    RepositoryFileInvocationTextRange,
};

use super::{
    FileInvocationExtraction, MAX_ARGUMENT_BYTES, MAX_PATH_BYTES, checked_text_range,
    encoded_json_len_within, target_bearing_value,
};

mod static_js;

use static_js::{StaticJsParser, StaticNestedToolCall};

const MAX_STATIC_PATCH_PATHS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchOperation {
    path: String,
    prior_path: Option<String>,
    kind: RepositoryFileInvocationKind,
    patch_text_range: Range<usize>,
}

pub(super) fn extract(
    record: &CoreRecord,
    invocation: &ActivityInvocation,
    arguments: &Value,
) -> FileInvocationExtraction {
    let tool = invocation.tool.as_str();
    if let Some(kind) = exact_file_operation(tool) {
        return exact_native_file(record, tool, kind, arguments);
    }
    match tool {
        "apply_patch" => direct_patch(record, arguments),
        "exec" => nested_exec(arguments),
        _ if target_bearing_value(arguments) => FileInvocationExtraction::Rejected,
        _ => FileInvocationExtraction::NotApplicable,
    }
}

fn exact_native_file(
    record: &CoreRecord,
    tool: &str,
    kind: RepositoryFileInvocationKind,
    arguments: &Value,
) -> FileInvocationExtraction {
    let Some(arguments) = decode_top_level_argument_object(arguments) else {
        return FileInvocationExtraction::Rejected;
    };
    if [
        "operation",
        "file_path",
        "filePath",
        "target_path",
        "targetPath",
        "old_path",
        "oldPath",
        "new_path",
        "newPath",
    ]
    .iter()
    .any(|field| arguments.contains_key(*field))
    {
        return FileInvocationExtraction::Rejected;
    }
    let Some(path) = arguments
        .get("path")
        .and_then(Value::as_str)
        .and_then(bounded_patch_path)
    else {
        return FileInvocationExtraction::Rejected;
    };
    let prior_path = match kind {
        RepositoryFileInvocationKind::Rename => {
            let Some(prior_path) = arguments
                .get("prior_path")
                .and_then(Value::as_str)
                .and_then(bounded_patch_path)
            else {
                return FileInvocationExtraction::Rejected;
            };
            if prior_path == path {
                return FileInvocationExtraction::Rejected;
            }
            Some(prior_path)
        }
        _ if arguments.contains_key("prior_path") => return FileInvocationExtraction::Rejected,
        _ => None,
    };
    let normalized_text_range = serde_json::to_string(&arguments).ok().and_then(|unit| {
        let body = record.content.normalized_body.as_deref()?;
        let prefix = format!("{tool}: ");
        (body.strip_prefix(&prefix) == Some(unit.as_str()))
            .then(|| checked_text_range(prefix.len(), prefix.len().checked_add(unit.len())?))?
    });
    FileInvocationExtraction::Exact(vec![LiteralRepositoryFileInvocation {
        operation_ordinal: 0,
        path,
        prior_path,
        kind,
        tool_name: Some(tool.to_owned()),
        normalized_text_range,
    }])
}

fn direct_patch(record: &CoreRecord, arguments: &Value) -> FileInvocationExtraction {
    let patch = match arguments {
        Value::String(value) if value.trim_start().starts_with("*** Begin Patch") => value.clone(),
        Value::Object(_) | Value::String(_) => {
            let Some(object) = decode_top_level_argument_object(arguments) else {
                return FileInvocationExtraction::Rejected;
            };
            let Some(patch) = object.get("patch").and_then(Value::as_str) else {
                return FileInvocationExtraction::Rejected;
            };
            patch.to_owned()
        }
        _ => return FileInvocationExtraction::Rejected,
    };
    patch_invocations(record.content.normalized_body.as_deref(), &patch, 0)
}

fn nested_exec(arguments: &Value) -> FileInvocationExtraction {
    let Some(source) = arguments
        .as_str()
        .filter(|source| source.len() <= MAX_ARGUMENT_BYTES)
    else {
        return FileInvocationExtraction::Rejected;
    };
    let Some(calls) = StaticJsParser::new(source).parse_program() else {
        return if source.contains("apply_patch") {
            FileInvocationExtraction::Rejected
        } else {
            FileInvocationExtraction::NotApplicable
        };
    };
    let mut invocations = Vec::new();
    let mut operation_ordinal = 0_u32;
    let mut observed_patch = false;
    for call in calls {
        match call {
            StaticNestedToolCall::ExecCommand { .. } => {
                let Some(next) = operation_ordinal.checked_add(1) else {
                    return FileInvocationExtraction::Rejected;
                };
                operation_ordinal = next;
            }
            StaticNestedToolCall::ApplyPatch { patch } => {
                observed_patch = true;
                let Some(operations) = static_patch_operations(&patch) else {
                    return FileInvocationExtraction::Rejected;
                };
                let Ok(operation_count) = u32::try_from(operations.len()) else {
                    return FileInvocationExtraction::Rejected;
                };
                if invocations
                    .len()
                    .checked_add(operations.len())
                    .is_none_or(|count| count > MAX_REPOSITORY_CANDIDATES)
                {
                    return FileInvocationExtraction::Rejected;
                }
                for (index, operation) in operations.into_iter().enumerate() {
                    let Ok(index) = u32::try_from(index) else {
                        return FileInvocationExtraction::Rejected;
                    };
                    let Some(ordinal) = operation_ordinal.checked_add(index) else {
                        return FileInvocationExtraction::Rejected;
                    };
                    invocations.push(literal_patch_invocation(operation, ordinal, None));
                }
                let Some(next) = operation_ordinal.checked_add(operation_count.max(1)) else {
                    return FileInvocationExtraction::Rejected;
                };
                operation_ordinal = next;
            }
        }
    }
    if observed_patch {
        FileInvocationExtraction::Exact(invocations)
    } else {
        FileInvocationExtraction::NotApplicable
    }
}

fn patch_invocations(
    normalized_body: Option<&str>,
    patch: &str,
    first_operation_ordinal: u32,
) -> FileInvocationExtraction {
    if patch.len() > MAX_ARGUMENT_BYTES {
        return FileInvocationExtraction::Rejected;
    }
    let Some(operations) = static_patch_operations(patch) else {
        return FileInvocationExtraction::Rejected;
    };
    if operations.len() > MAX_REPOSITORY_CANDIDATES {
        return FileInvocationExtraction::Rejected;
    }
    let normalized_patch_offset = normalized_body.and_then(|body| {
        let normalized_patch = patch.trim();
        body.strip_prefix("apply_patch: ")
            .filter(|body_patch| *body_patch == normalized_patch)
            .map(|_| "apply_patch: ".len())
    });
    let mut invocations = Vec::with_capacity(operations.len());
    for (index, operation) in operations.into_iter().enumerate() {
        let Ok(index) = u32::try_from(index) else {
            return FileInvocationExtraction::Rejected;
        };
        let Some(operation_ordinal) = first_operation_ordinal.checked_add(index) else {
            return FileInvocationExtraction::Rejected;
        };
        let normalized_text_range = normalized_patch_offset.and_then(|offset| {
            checked_text_range(
                offset.checked_add(operation.patch_text_range.start)?,
                offset.checked_add(operation.patch_text_range.end)?,
            )
        });
        invocations.push(literal_patch_invocation(
            operation,
            operation_ordinal,
            normalized_text_range,
        ));
    }
    FileInvocationExtraction::Exact(invocations)
}

fn literal_patch_invocation(
    operation: PatchOperation,
    operation_ordinal: u32,
    normalized_text_range: Option<RepositoryFileInvocationTextRange>,
) -> LiteralRepositoryFileInvocation {
    LiteralRepositoryFileInvocation {
        operation_ordinal,
        path: operation.path,
        prior_path: operation.prior_path,
        kind: operation.kind,
        tool_name: Some("apply_patch".to_owned()),
        normalized_text_range,
    }
}

fn static_patch_operations(patch: &str) -> Option<Vec<PatchOperation>> {
    let lines = patch_lines(patch);
    if lines.first()?.text != "*** Begin Patch" {
        return None;
    }
    let mut operations = Vec::new();
    let mut pending_update: Option<(String, Range<usize>, usize)> = None;
    let mut ended = false;
    for line in lines.into_iter().skip(1) {
        if ended {
            if !line.text.trim().is_empty() {
                return None;
            }
            continue;
        }
        if line.text == "*** End Patch" {
            push_pending_patch_update(&mut operations, &mut pending_update)?;
            ended = true;
            continue;
        }
        if let Some(path) = line.text.strip_prefix("*** Add File: ") {
            push_pending_patch_update(&mut operations, &mut pending_update)?;
            push_patch_operation(
                &mut operations,
                path,
                None,
                RepositoryFileInvocationKind::Create,
                line.range,
            )?;
        } else if let Some(path) = line.text.strip_prefix("*** Update File: ") {
            push_pending_patch_update(&mut operations, &mut pending_update)?;
            pending_update = Some((bounded_patch_path(path)?, line.range, line.index));
        } else if let Some(path) = line.text.strip_prefix("*** Delete File: ") {
            push_pending_patch_update(&mut operations, &mut pending_update)?;
            push_patch_operation(
                &mut operations,
                path,
                None,
                RepositoryFileInvocationKind::Delete,
                line.range,
            )?;
        } else if let Some(path) = line.text.strip_prefix("*** Move to: ") {
            let (prior_path, update_range, update_line_index) = pending_update.take()?;
            if line.index != update_line_index.checked_add(1)? {
                return None;
            }
            push_patch_operation(
                &mut operations,
                path,
                Some(prior_path),
                RepositoryFileInvocationKind::Rename,
                update_range.start..line.range.end,
            )?;
        }
    }
    ended.then_some(operations)
}

fn push_pending_patch_update(
    operations: &mut Vec<PatchOperation>,
    pending_update: &mut Option<(String, Range<usize>, usize)>,
) -> Option<()> {
    if let Some((path, range, _)) = pending_update.take() {
        push_patch_operation(
            operations,
            &path,
            None,
            RepositoryFileInvocationKind::Modify,
            range,
        )?;
    }
    Some(())
}

fn push_patch_operation(
    operations: &mut Vec<PatchOperation>,
    path: &str,
    prior_path: Option<String>,
    kind: RepositoryFileInvocationKind,
    patch_text_range: Range<usize>,
) -> Option<()> {
    if operations.len() >= MAX_STATIC_PATCH_PATHS {
        return None;
    }
    let path = bounded_patch_path(path)?;
    if prior_path.as_ref().is_some_and(|prior| prior == &path) {
        return None;
    }
    operations.push(PatchOperation {
        path,
        prior_path,
        kind,
        patch_text_range,
    });
    Some(())
}

struct PatchLine<'a> {
    index: usize,
    text: &'a str,
    range: Range<usize>,
}

fn patch_lines(patch: &str) -> Vec<PatchLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0_usize;
    for (index, segment) in patch.split_inclusive('\n').enumerate() {
        let without_lf = segment.strip_suffix('\n').unwrap_or(segment);
        let text = without_lf.strip_suffix('\r').unwrap_or(without_lf);
        let end = start + text.len();
        lines.push(PatchLine {
            index,
            text,
            range: start..end,
        });
        start += segment.len();
    }
    if patch.is_empty() {
        lines.push(PatchLine {
            index: 0,
            text: patch,
            range: 0..0,
        });
    }
    lines
}

fn exact_file_operation(tool: &str) -> Option<RepositoryFileInvocationKind> {
    match tool {
        "read" => Some(RepositoryFileInvocationKind::Read),
        "create" => Some(RepositoryFileInvocationKind::Create),
        "modify" => Some(RepositoryFileInvocationKind::Modify),
        "delete" => Some(RepositoryFileInvocationKind::Delete),
        "rename" => Some(RepositoryFileInvocationKind::Rename),
        "write" => Some(RepositoryFileInvocationKind::Write),
        _ => None,
    }
}

fn bounded_patch_path(path: &str) -> Option<String> {
    let path = path.trim();
    (!path.is_empty() && path.len() <= MAX_PATH_BYTES && !path.contains(['\0', '\r', '\n']))
        .then(|| path.to_owned())
}

fn decode_top_level_argument_object(value: &Value) -> Option<Map<String, Value>> {
    encoded_json_len_within(value, MAX_ARGUMENT_BYTES)?;
    let decoded = match value {
        Value::Object(object) => Value::Object(object.clone()),
        Value::String(text) if text.len() <= MAX_ARGUMENT_BYTES => {
            serde_json::from_str::<Value>(text).ok()?
        }
        _ => return None,
    };
    let object = decoded.as_object()?.clone();
    encoded_json_len_within(&Value::Object(object.clone()), MAX_ARGUMENT_BYTES)?;
    Some(object)
}

use std::ops::Range;

use ctx_history_core::{RepositoryFileInvocationKind, RepositoryFileInvocationTextRange};
use serde_json::Value;

use crate::repository_attribution::UnscopedRepositoryFileInvocationEvidence;

use super::{dto::GeminiToolCall, parser::MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT};

pub(super) const MAX_GEMINI_FILE_INVOCATIONS_PER_EVENT: usize = 64;
const MAX_GEMINI_FILE_INVOCATION_PATH_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GeminiFileInvocationExtraction {
    pub(super) evidence: Vec<UnscopedRepositoryFileInvocationEvidence>,
    pub(super) abstained_target_bearing_calls: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeminiFileInvocationOverflow {
    Count { limit: usize },
    Bytes { limit: usize },
    CallOrdinal,
    NormalizedTextRange,
}

pub(super) struct GeminiNormalizedToolCalls {
    pub(super) text: String,
    argument_ranges: Vec<Option<Range<usize>>>,
}

pub(super) fn normalize_gemini_tool_calls(calls: &[GeminiToolCall]) -> GeminiNormalizedToolCalls {
    let mut text = String::new();
    let mut argument_ranges = Vec::with_capacity(calls.len());
    for call in calls {
        if let Some(name) = call.name.as_deref() {
            push_normalized_unit(&mut text, name);
        }
        let range = call.args.as_ref().and_then(|args| {
            let args = serde_json::to_string(args).ok()?;
            Some(push_normalized_unit(&mut text, &args))
        });
        argument_ranges.push(range);
    }
    GeminiNormalizedToolCalls {
        text,
        argument_ranges,
    }
}

pub(super) fn extract_gemini_file_invocations(
    calls: &[GeminiToolCall],
    normalized_body: &str,
) -> Result<GeminiFileInvocationExtraction, GeminiFileInvocationOverflow> {
    let normalized = normalize_gemini_tool_calls(calls);
    let ranges_are_exact = normalized.text == normalized_body;
    let mut evidence = Vec::new();
    let mut retained_path_bytes = 0_usize;
    let mut abstained_target_bearing_calls = false;

    for (call_ordinal, call) in calls.iter().enumerate() {
        let Some(tool_name) = call.name.as_deref() else {
            abstained_target_bearing_calls |= has_target_bearing_field(call.args.as_ref());
            continue;
        };
        let Some(action) = schema_proven_action(tool_name) else {
            abstained_target_bearing_calls |= has_target_bearing_field(call.args.as_ref());
            continue;
        };
        let Some(path) = schema_proven_file_path(call.args.as_ref()) else {
            abstained_target_bearing_calls |= has_target_bearing_field(call.args.as_ref());
            continue;
        };
        if evidence.len() >= MAX_GEMINI_FILE_INVOCATIONS_PER_EVENT {
            return Err(GeminiFileInvocationOverflow::Count {
                limit: MAX_GEMINI_FILE_INVOCATIONS_PER_EVENT,
            });
        }
        retained_path_bytes = retained_path_bytes.checked_add(path.len()).ok_or(
            GeminiFileInvocationOverflow::Bytes {
                limit: MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT,
            },
        )?;
        if retained_path_bytes > MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT {
            return Err(GeminiFileInvocationOverflow::Bytes {
                limit: MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT,
            });
        }
        let call_index = call_ordinal;
        let call_ordinal =
            u32::try_from(call_ordinal).map_err(|_| GeminiFileInvocationOverflow::CallOrdinal)?;
        let normalized_text_range = if ranges_are_exact {
            normalized.argument_ranges[call_index]
                .as_ref()
                .map(checked_normalized_text_range)
                .transpose()?
        } else {
            None
        };
        evidence.push(UnscopedRepositoryFileInvocationEvidence {
            operation_ordinal: call_ordinal,
            path: path.to_owned(),
            prior_path: None,
            kind: action,
            tool_name: Some(tool_name.to_owned()),
            normalized_text_range,
        });
    }
    Ok(GeminiFileInvocationExtraction {
        evidence,
        abstained_target_bearing_calls,
    })
}

pub(super) fn checked_normalized_text_range(
    range: &Range<usize>,
) -> Result<RepositoryFileInvocationTextRange, GeminiFileInvocationOverflow> {
    Ok(RepositoryFileInvocationTextRange {
        start: u32::try_from(range.start)
            .map_err(|_| GeminiFileInvocationOverflow::NormalizedTextRange)?,
        end: u32::try_from(range.end)
            .map_err(|_| GeminiFileInvocationOverflow::NormalizedTextRange)?,
    })
}

fn push_normalized_unit(text: &mut String, unit: &str) -> Range<usize> {
    if !text.is_empty() {
        text.push('\n');
    }
    let start = text.len();
    text.push_str(unit);
    start..text.len()
}

fn schema_proven_action(tool_name: &str) -> Option<RepositoryFileInvocationKind> {
    match tool_name {
        "read_file" => Some(RepositoryFileInvocationKind::Read),
        // write_file may create or modify a file. The neutral action is Write;
        // guessing either more specific mutation would be false precision.
        "write_file" => Some(RepositoryFileInvocationKind::Write),
        "replace" => Some(RepositoryFileInvocationKind::Modify),
        _ => None,
    }
}

fn schema_proven_file_path(args: Option<&Value>) -> Option<&str> {
    let args = args?.as_object()?;
    if args.contains_key("path") || args.contains_key("filePath") {
        return None;
    }
    let path = args.get("file_path")?.as_str()?;
    (!path.trim().is_empty()
        && path.len() <= MAX_GEMINI_FILE_INVOCATION_PATH_BYTES
        && !path.contains('\0'))
    .then_some(path)
}

fn has_target_bearing_field(args: Option<&Value>) -> bool {
    args.and_then(Value::as_object).is_some_and(|args| {
        ["file_path", "filePath", "path"]
            .into_iter()
            .any(|key| args.contains_key(key))
    })
}

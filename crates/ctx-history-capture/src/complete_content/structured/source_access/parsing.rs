//! Bounded JSON decoding and task-record scanning.

use serde_json::Value;

use super::{error, ContentErrorContext};
use crate::complete_content::structured::verification::ResolutionBudget;
use crate::complete_content::{CompleteContentError, CompleteContentErrorKind};

pub(in crate::complete_content::structured) fn parse_bounded_json(
    request: &(impl ContentErrorContext + ?Sized),
    bytes: &[u8],
    budget: &mut ResolutionBudget,
) -> std::result::Result<Value, CompleteContentError> {
    let value = serde_json::from_slice(bytes)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceChanged))?;
    validate_json_shape(request, &value, budget, 0)?;
    Ok(value)
}

pub(in crate::complete_content::structured) fn validate_json_shape(
    request: &(impl ContentErrorContext + ?Sized),
    value: &Value,
    budget: &mut ResolutionBudget,
    depth: usize,
) -> std::result::Result<(), CompleteContentError> {
    if depth > budget.bounds.max_json_depth {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    budget.observe_entries(request, 1)?;
    match value {
        Value::Array(items) => {
            for item in items {
                validate_json_shape(request, item, budget, depth.saturating_add(1))?;
            }
        }
        Value::Object(object) => {
            for value in object.values() {
                validate_json_shape(request, value, budget, depth.saturating_add(1))?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(in crate::complete_content::structured) struct TaskJsonRecord<'a> {
    pub(in crate::complete_content::structured) native_index: usize,
    pub(in crate::complete_content::structured) bytes: &'a [u8],
    pub(in crate::complete_content::structured) value: Value,
}

pub(in crate::complete_content::structured) fn task_json_records<'a>(
    request: &(impl ContentErrorContext + ?Sized),
    bytes: &'a [u8],
    budget: &mut ResolutionBudget,
) -> std::result::Result<Vec<TaskJsonRecord<'a>>, CompleteContentError> {
    let range = locate_task_array(bytes)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceChanged))?;
    let mut records = Vec::new();
    let mut cursor = range.start;
    while cursor < range.end {
        cursor = skip_json_whitespace(bytes, cursor);
        if cursor >= range.end {
            break;
        }
        let end = scan_json_value_end(bytes, cursor, range.end)
            .ok_or_else(|| error(request, CompleteContentErrorKind::SourceChanged))?;
        let raw = &bytes[cursor..end];
        let value = parse_bounded_json(request, raw, budget)?;
        records.push(TaskJsonRecord {
            native_index: records.len(),
            bytes: raw,
            value,
        });
        budget.observe_entries(request, 1)?;
        cursor = skip_json_whitespace(bytes, end);
        if cursor < range.end {
            if bytes[cursor] != b',' {
                return Err(error(request, CompleteContentErrorKind::SourceChanged));
            }
            cursor = cursor.saturating_add(1);
        }
    }
    Ok(records)
}

fn locate_task_array(bytes: &[u8]) -> Option<std::ops::Range<usize>> {
    let start = skip_json_whitespace(bytes, 0);
    match *bytes.get(start)? {
        b'[' => matching_container_range(bytes, start, b'[', b']'),
        b'{' => locate_named_task_array(bytes, start),
        _ => None,
    }
}

fn locate_named_task_array(bytes: &[u8], object_start: usize) -> Option<std::ops::Range<usize>> {
    let mut cursor = object_start.checked_add(1)?;
    loop {
        cursor = skip_json_whitespace(bytes, cursor);
        if bytes.get(cursor) == Some(&b'}') {
            return None;
        }
        let key_end = scan_json_value_end(bytes, cursor, bytes.len())?;
        let key = serde_json::from_slice::<String>(bytes.get(cursor..key_end)?).ok()?;
        cursor = skip_json_whitespace(bytes, key_end);
        if bytes.get(cursor) != Some(&b':') {
            return None;
        }
        cursor = skip_json_whitespace(bytes, cursor.checked_add(1)?);
        if matches!(key.as_str(), "messages" | "history") && bytes.get(cursor) == Some(&b'[') {
            return matching_container_range(bytes, cursor, b'[', b']');
        }
        cursor = scan_json_value_end(bytes, cursor, bytes.len())?;
        cursor = skip_json_whitespace(bytes, cursor);
        match bytes.get(cursor) {
            Some(b',') => cursor = cursor.checked_add(1)?,
            Some(b'}') | None => return None,
            _ => return None,
        }
    }
}

fn matching_container_range(
    bytes: &[u8],
    start: usize,
    open: u8,
    close: u8,
) -> Option<std::ops::Range<usize>> {
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, byte) in bytes.get(start..)?.iter().copied().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        if byte == b'"' {
            quoted = true;
        } else if byte == open {
            depth = depth.saturating_add(1);
        } else if byte == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(start + 1..start + offset);
            }
        }
    }
    None
}

fn scan_json_value_end(bytes: &[u8], start: usize, limit: usize) -> Option<usize> {
    let first = *bytes.get(start)?;
    if matches!(first, b'{' | b'[') {
        let close = if first == b'{' { b'}' } else { b']' };
        return matching_container_range(bytes.get(..limit)?, start, first, close)
            .map(|range| range.end + 1);
    }
    if first == b'"' {
        let mut escaped = false;
        for (cursor, byte) in bytes
            .iter()
            .copied()
            .enumerate()
            .take(limit)
            .skip(start + 1)
        {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return Some(cursor + 1);
            }
        }
        return None;
    }
    (start..limit)
        .find(|cursor| matches!(bytes[*cursor], b',' | b']' | b'}'))
        .map(|cursor| {
            let mut end = cursor;
            while end > start && bytes[end - 1].is_ascii_whitespace() {
                end -= 1;
            }
            end
        })
        .or(Some(limit))
}

fn skip_json_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor = cursor.saturating_add(1);
    }
    cursor
}

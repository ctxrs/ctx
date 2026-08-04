use chrono::{DateTime, Utc};
use serde_json::{Map, Number, Value};

use super::super::super::wire::{warp_wire_text, WarpWireCursor, WarpWireValue};
use super::{
    select_message_oneof, validate_message_payload, WarpSelectedMessage, WarpValidatedString,
};
use crate::{CaptureError, Result};

const WARP_PROTOBUF_VALUE_MAX_DEPTH: usize = 64;

#[cfg(test)]
pub(super) fn decode_protobuf_struct(data: &[u8], depth: usize) -> Result<Value> {
    Ok(Value::Object(decode_protobuf_struct_map(data, depth)?))
}

pub(super) fn decode_protobuf_struct_map(data: &[u8], depth: usize) -> Result<Map<String, Value>> {
    ensure_value_depth(depth)?;
    let mut cursor = WarpWireCursor::new(data);
    let mut object = Map::new();
    while let Some(field) = cursor.next()? {
        let (1, WarpWireValue::LengthDelimited(entry)) = (field.number, field.value) else {
            continue;
        };
        let (key, value) = decode_protobuf_struct_entry(entry, depth + 1)?;
        object.insert(key, value);
    }
    Ok(object)
}

fn decode_protobuf_struct_entry(data: &[u8], depth: usize) -> Result<(String, Value)> {
    ensure_value_depth(depth)?;
    let mut cursor = WarpWireCursor::new(data);
    let mut key = WarpValidatedString::default();
    let mut value = None::<WarpProtobufValueKind>;
    while let Some(field) = cursor.next()? {
        match (field.number, field.value) {
            (1, WarpWireValue::LengthDelimited(raw)) => key.observe(raw),
            (2, WarpWireValue::LengthDelimited(raw)) => {
                let occurrence = decode_protobuf_value_kind(raw, depth + 1)?;
                merge_protobuf_value_kind(&mut value, occurrence);
            }
            _ => {}
        }
    }
    Ok((
        key.into_optional("Struct.FieldsEntry.key")?
            .unwrap_or_default(),
        protobuf_value_kind_to_json(value, depth + 1)?,
    ))
}

enum WarpProtobufValueKind {
    Null,
    Number(f64),
    String(String),
    Bool(bool),
    Struct(Map<String, Value>),
    List(Vec<Value>),
}

fn decode_protobuf_value(data: &[u8], depth: usize) -> Result<Value> {
    let selected = decode_protobuf_value_kind(data, depth)?;
    protobuf_value_kind_to_json(selected, depth)
}

fn decode_protobuf_value_kind(data: &[u8], depth: usize) -> Result<Option<WarpProtobufValueKind>> {
    ensure_value_depth(depth)?;
    let mut cursor = WarpWireCursor::new(data);
    let mut selected = None;
    while let Some(field) = cursor.next()? {
        match (field.number, field.value) {
            (1, WarpWireValue::Varint(_)) => selected = Some(WarpProtobufValueKind::Null),
            (2, WarpWireValue::Fixed64(bits)) => {
                selected = Some(WarpProtobufValueKind::Number(f64::from_bits(bits)));
            }
            (3, WarpWireValue::LengthDelimited(raw)) => {
                selected = Some(WarpProtobufValueKind::String(warp_text_owned(raw)?));
            }
            (4, WarpWireValue::Varint(value)) => {
                selected = Some(WarpProtobufValueKind::Bool(value != 0));
            }
            (5, WarpWireValue::LengthDelimited(raw)) => {
                let occurrence =
                    WarpProtobufValueKind::Struct(decode_protobuf_struct_map(raw, depth + 1)?);
                merge_protobuf_value_kind(&mut selected, Some(occurrence));
            }
            (6, WarpWireValue::LengthDelimited(raw)) => {
                let occurrence = WarpProtobufValueKind::List(decode_protobuf_list(raw, depth + 1)?);
                merge_protobuf_value_kind(&mut selected, Some(occurrence));
            }
            _ => {}
        }
    }
    Ok(selected)
}

fn protobuf_value_kind_to_json(
    selected: Option<WarpProtobufValueKind>,
    depth: usize,
) -> Result<Value> {
    ensure_value_depth(depth)?;
    match selected {
        None => Err(CaptureError::InvalidPayload(
            "Warp protobuf Value.kind is unset".to_owned(),
        )),
        Some(WarpProtobufValueKind::Null) => Ok(Value::Null),
        Some(WarpProtobufValueKind::Number(value)) => {
            Number::from_f64(value).map(Value::Number).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Warp protobuf Value.number_value must be finite".to_owned(),
                )
            })
        }
        Some(WarpProtobufValueKind::String(value)) => Ok(Value::String(value)),
        Some(WarpProtobufValueKind::Bool(value)) => Ok(Value::Bool(value)),
        Some(WarpProtobufValueKind::Struct(value)) => Ok(Value::Object(value)),
        Some(WarpProtobufValueKind::List(value)) => Ok(Value::Array(value)),
    }
}

fn merge_protobuf_value_kind(
    selected: &mut Option<WarpProtobufValueKind>,
    occurrence: Option<WarpProtobufValueKind>,
) {
    let Some(occurrence) = occurrence else {
        return;
    };
    match (selected, occurrence) {
        (
            Some(WarpProtobufValueKind::Struct(selected)),
            WarpProtobufValueKind::Struct(occurrence),
        ) => {
            selected.extend(occurrence);
        }
        (
            Some(WarpProtobufValueKind::List(selected)),
            WarpProtobufValueKind::List(mut occurrence),
        ) => {
            selected.append(&mut occurrence);
        }
        (selected, occurrence) => {
            *selected = Some(occurrence);
        }
    }
}

fn decode_protobuf_list(data: &[u8], depth: usize) -> Result<Vec<Value>> {
    ensure_value_depth(depth)?;
    let mut cursor = WarpWireCursor::new(data);
    let mut values = Vec::new();
    while let Some(field) = cursor.next()? {
        if let (1, WarpWireValue::LengthDelimited(raw)) = (field.number, field.value) {
            values.push(decode_protobuf_value(raw, depth + 1)?);
        }
    }
    Ok(values)
}

fn ensure_value_depth(depth: usize) -> Result<()> {
    if depth > WARP_PROTOBUF_VALUE_MAX_DEPTH {
        return Err(CaptureError::InvalidPayload(
            "Warp protobuf Struct exceeds the nesting limit".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn decode_timestamp_occurrences(payloads: &[&[u8]]) -> Result<Option<DateTime<Utc>>> {
    let mut seconds = None;
    let mut nanos = 0_u32;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::Varint(value)) => seconds = Some(value as i64),
                (2, WarpWireValue::Varint(value)) => {
                    nanos = u32::try_from(value).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "Warp protobuf timestamp nanos overflowed".to_owned(),
                        )
                    })?;
                }
                _ => {}
            }
        }
    }
    if nanos >= 1_000_000_000 {
        return Err(CaptureError::InvalidPayload(
            "Warp protobuf timestamp nanos are out of range".to_owned(),
        ));
    }
    Ok(seconds.and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, nanos)))
}

pub(super) fn decode_system_query_occurrences(payloads: &[Vec<u8>]) -> Result<String> {
    let Some(selected) = select_nested_oneof_occurrences(payloads)? else {
        return Ok("system query".to_owned());
    };
    Ok(match selected.field {
        1 => "system query: auto code diff".to_owned(),
        3 => "system query: resume conversation".to_owned(),
        4 => "system query: generate passive suggestions".to_owned(),
        5 => decode_last_nested_text_occurrences(&selected.payloads, 1)?
            .map(|query| format!("system query: create new project\n{query}"))
            .unwrap_or_else(|| "system query: create new project".to_owned()),
        6 => "system query: clone repository".to_owned(),
        7 => decode_last_nested_text_occurrences(&selected.payloads, 1)?
            .map(|prompt| format!("system query: summarize conversation\n{prompt}"))
            .unwrap_or_else(|| "system query: summarize conversation".to_owned()),
        8 => "system query: fetch review comments".to_owned(),
        9 => "system query: handoff rehydration".to_owned(),
        field => format!("system query: field {field}"),
    })
}

pub(super) fn decode_summarization_occurrences(payloads: &[Vec<u8>]) -> Result<String> {
    let Some(selected) = select_nested_oneof_occurrences(payloads)? else {
        return Err(CaptureError::InvalidPayload(
            "Warp summarization has no selected arm".to_owned(),
        ));
    };
    if selected.field == 1 {
        return Ok(decode_last_nested_text_occurrences(&selected.payloads, 1)?
            .map(|summary| format!("conversation summary\n{summary}"))
            .unwrap_or_else(|| "conversation summary".to_owned()));
    }
    Ok(format!("summarization: field {}", selected.field))
}

pub(super) fn decode_received_messages_occurrences(payloads: &[Vec<u8>]) -> Result<String> {
    let mut parts = Vec::new();
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            let (1, WarpWireValue::LengthDelimited(received)) = (field.number, field.value) else {
                continue;
            };
            let subject = decode_last_nested_text(received, 4)?.unwrap_or_default();
            let body = decode_last_nested_text(received, 5)?.unwrap_or_default();
            let text = match (subject.is_empty(), body.is_empty()) {
                (false, false) => format!("{subject}\n{body}"),
                (false, true) => subject,
                (true, false) => body,
                (true, true) => continue,
            };
            parts.push(text);
        }
    }
    Ok(parts.join("\n\n"))
}

fn select_nested_oneof_occurrences(payloads: &[Vec<u8>]) -> Result<Option<WarpSelectedMessage>> {
    let mut selected = None;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            if let WarpWireValue::LengthDelimited(value) = field.value {
                validate_message_payload(value)?;
                select_message_oneof(&mut selected, field.number, value);
            }
        }
    }
    Ok(selected)
}

pub(super) fn decode_last_nested_text_occurrences(
    payloads: &[Vec<u8>],
    desired_field: u32,
) -> Result<Option<String>> {
    let mut selected = WarpValidatedString::default();
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            if let (number, WarpWireValue::LengthDelimited(value)) = (field.number, field.value) {
                if number == desired_field {
                    selected.observe(value);
                }
            }
        }
    }
    selected.into_optional("string field")
}

pub(super) fn validate_string_fields_occurrences(
    payloads: &[Vec<u8>],
    fields: &[u32],
    label: &str,
) -> Result<()> {
    let mut valid = WarpValidatedString::default();
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            if let (number, WarpWireValue::LengthDelimited(value)) = (field.number, field.value) {
                if fields.contains(&number) {
                    valid.observe(value);
                }
            }
        }
    }
    let _ = valid.into_optional(label)?;
    Ok(())
}

fn decode_last_nested_text(data: &[u8], field: u32) -> Result<Option<String>> {
    last_length_delimited_value(data, field)?
        .map(warp_text_owned)
        .transpose()
}

pub(super) fn last_length_delimited_value(
    data: &[u8],
    desired_field: u32,
) -> Result<Option<&[u8]>> {
    let mut cursor = WarpWireCursor::new(data);
    let mut selected = None;
    while let Some(field) = cursor.next()? {
        if let (number, WarpWireValue::LengthDelimited(value)) = (field.number, field.value) {
            if number == desired_field {
                selected = Some(value);
            }
        }
    }
    Ok(selected)
}

pub(super) fn last_length_delimited_value_occurrences(
    payloads: &[Vec<u8>],
    desired_field: u32,
) -> Result<Option<&[u8]>> {
    let mut selected = None;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            if let (number, WarpWireValue::LengthDelimited(value)) = (field.number, field.value) {
                if number == desired_field {
                    selected = Some(value);
                }
            }
        }
    }
    Ok(selected)
}

pub(super) fn last_length_delimited_field_occurrences(
    payloads: &[Vec<u8>],
) -> Result<Option<(u32, &[u8])>> {
    let mut selected = None;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            if let WarpWireValue::LengthDelimited(value) = field.value {
                selected = Some((field.number, value));
            }
        }
    }
    Ok(selected)
}

pub(super) fn warp_text_owned(data: &[u8]) -> Result<String> {
    warp_wire_text(data).map(str::to_owned)
}

pub(super) fn bounded_linkage_owned(value: String) -> Option<String> {
    const MAX_LINKAGE_BYTES: usize = 16 * 1024;
    let value = value.trim();
    (!value.is_empty() && value.len() <= MAX_LINKAGE_BYTES).then(|| value.to_owned())
}

pub(super) fn bounded_exact_linkage_owned(value: String) -> Option<String> {
    const MAX_LINKAGE_BYTES: usize = 16 * 1024;
    (!value.is_empty() && value.len() <= MAX_LINKAGE_BYTES).then_some(value)
}

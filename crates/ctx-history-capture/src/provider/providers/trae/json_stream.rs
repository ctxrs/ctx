use std::ops::Range;

use serde::de::IgnoredAny;
use serde_json::Value;

use crate::{CaptureError, Result};

use super::{event::trae_message_text, TRAE_CN_INPUT_HISTORY_KEY};

pub(super) enum TraeSessionSelection {
    CnMessages(Range<usize>),
    Sessions(Range<usize>),
}

pub(super) struct TraeStreamSession {
    pub(super) native_session_id: String,
    pub(super) native_session_id_from_provider: bool,
    pub(super) messages: Range<usize>,
}

pub(super) fn trae_session_selection(
    bytes: &[u8],
    chat_key: &str,
) -> Result<Option<TraeSessionSelection>> {
    let whole = trae_json_trimmed_range(bytes, 0..bytes.len())?;
    if chat_key == TRAE_CN_INPUT_HISTORY_KEY {
        return Ok((bytes.get(whole.start) == Some(&b'['))
            .then_some(TraeSessionSelection::CnMessages(whole)));
    }
    if bytes.get(whole.start) != Some(&b'{') {
        return Ok(None);
    }
    let fields: &[&str] = if chat_key == "memento/icube-ai-agent-storage" {
        &["list"]
    } else if chat_key == "ChatStore" {
        &["sessions", "entries", "conversations", "list"]
    } else {
        &["entries", "sessions", "conversations", "list"]
    };
    let mut selected: [Option<Range<usize>>; 4] = std::array::from_fn(|_| None);
    let mut object = TraeJsonObjectFields::new(bytes, whole.clone())?;
    while let Some((key, range)) = object.next_field()? {
        if let Some(index) = fields.iter().position(|field| *field == key) {
            if matches!(bytes.get(range.start), Some(b'[' | b'{')) {
                selected[index] = Some(range);
            }
        }
    }
    if let Some(range) = selected.into_iter().take(fields.len()).flatten().next() {
        return Ok(Some(TraeSessionSelection::Sessions(range)));
    }
    if chat_key == "memento/icube-ai-agent-storage" {
        Ok(None)
    } else {
        Ok(Some(TraeSessionSelection::Sessions(whole)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TraePayloadAdmission {
    SupportedChat,
    Empty,
    Unrecognized,
}

pub(crate) fn trae_payload_admission(bytes: &[u8], chat_key: &str) -> Result<TraePayloadAdmission> {
    serde_json::from_slice::<IgnoredAny>(bytes)?;
    let Some(selection) = trae_session_selection(bytes, chat_key)? else {
        return Ok(TraePayloadAdmission::Unrecognized);
    };
    match selection {
        TraeSessionSelection::CnMessages(messages) => trae_messages_admission(bytes, messages),
        TraeSessionSelection::Sessions(container) => {
            let mut values = TraeJsonContainerValues::new(bytes, container)?;
            let mut saw_supported_chat = false;
            let mut saw_unrecognized = false;
            let mut session_index = 0_usize;
            while let Some(range) = values.next_range()? {
                if let Some(session) = trae_stream_session(bytes, range, session_index)? {
                    match trae_messages_admission(bytes, session.messages)? {
                        TraePayloadAdmission::SupportedChat => saw_supported_chat = true,
                        TraePayloadAdmission::Empty => {}
                        TraePayloadAdmission::Unrecognized => saw_unrecognized = true,
                    }
                } else {
                    saw_unrecognized = true;
                }
                session_index = session_index.saturating_add(1);
            }
            Ok(if saw_supported_chat {
                TraePayloadAdmission::SupportedChat
            } else if saw_unrecognized {
                TraePayloadAdmission::Unrecognized
            } else {
                TraePayloadAdmission::Empty
            })
        }
    }
}

fn trae_messages_admission(bytes: &[u8], messages: Range<usize>) -> Result<TraePayloadAdmission> {
    let mut values = TraeJsonArrayValues::new(bytes, messages)?;
    let mut saw_supported_chat = false;
    let mut saw_unrecognized = false;
    while let Some(range) = values.next_range()? {
        let message: Value = serde_json::from_slice(&bytes[range])?;
        if trae_message_text(&message).is_some_and(|text| !text.trim().is_empty()) {
            saw_supported_chat = true;
        } else {
            saw_unrecognized = true;
        }
    }
    Ok(if saw_supported_chat {
        TraePayloadAdmission::SupportedChat
    } else if saw_unrecognized {
        TraePayloadAdmission::Unrecognized
    } else {
        TraePayloadAdmission::Empty
    })
}

pub(super) fn trae_stream_session(
    bytes: &[u8],
    range: Range<usize>,
    session_index: usize,
) -> Result<Option<TraeStreamSession>> {
    if bytes.get(range.start) != Some(&b'{') {
        return Ok(None);
    }
    let message_fields = ["messages", "chatMessages", "bubbles", "items"];
    let session_id_fields = [
        "sessionId",
        "session_id",
        "id",
        "conversationId",
        "conversation_id",
    ];
    let mut message_ranges: [Option<Range<usize>>; 4] = std::array::from_fn(|_| None);
    let mut session_ids: [Option<String>; 5] = std::array::from_fn(|_| None);
    let mut object = TraeJsonObjectFields::new(bytes, range)?;
    while let Some((key, value_range)) = object.next_field()? {
        if let Some(index) = message_fields.iter().position(|field| *field == key) {
            if bytes.get(value_range.start) == Some(&b'[') {
                message_ranges[index] = Some(value_range);
                continue;
            }
        }
        if let Some(index) = session_id_fields.iter().position(|field| *field == key) {
            session_ids[index] = serde_json::from_slice(&bytes[value_range]).ok();
        }
    }
    let Some(messages) = message_ranges.into_iter().flatten().next() else {
        return Ok(None);
    };
    let provider_native_session_id = session_ids.into_iter().flatten().next();
    Ok(Some(TraeStreamSession {
        native_session_id_from_provider: provider_native_session_id.is_some(),
        native_session_id: provider_native_session_id
            .unwrap_or_else(|| format!("session-{}", session_index.saturating_add(1))),
        messages,
    }))
}

pub(super) struct TraeJsonArrayValues<'a> {
    bytes: &'a [u8],
    cursor: usize,
    end: usize,
    done: bool,
}

impl<'a> TraeJsonArrayValues<'a> {
    pub(super) fn new(bytes: &'a [u8], range: Range<usize>) -> Result<Self> {
        let range = trae_json_trimmed_range(bytes, range)?;
        if bytes.get(range.start) != Some(&b'[') || bytes.get(range.end - 1) != Some(&b']') {
            return Err(CaptureError::InvalidPayload(
                "Trae messages must be a JSON array".to_owned(),
            ));
        }
        Ok(Self {
            bytes,
            cursor: range.start + 1,
            end: range.end - 1,
            done: false,
        })
    }

    pub(super) fn next_range(&mut self) -> Result<Option<Range<usize>>> {
        if self.done {
            return Ok(None);
        }
        self.cursor = trae_json_skip_ws(self.bytes, self.cursor, self.end);
        if self.cursor == self.end {
            self.done = true;
            return Ok(None);
        }
        let start = self.cursor;
        let end = trae_json_value_end(self.bytes, start, self.end)?;
        self.cursor = trae_json_skip_ws(self.bytes, end, self.end);
        if self.cursor < self.end {
            if self.bytes[self.cursor] != b',' {
                return Err(CaptureError::InvalidPayload(
                    "Trae JSON array has an invalid separator".to_owned(),
                ));
            }
            self.cursor += 1;
        } else {
            self.done = true;
        }
        Ok(Some(start..end))
    }
}

pub(super) enum TraeJsonContainerValues<'a> {
    Array(TraeJsonArrayValues<'a>),
    Object(TraeJsonObjectFields<'a>),
}

impl<'a> TraeJsonContainerValues<'a> {
    pub(super) fn new(bytes: &'a [u8], range: Range<usize>) -> Result<Self> {
        match bytes.get(range.start) {
            Some(b'[') => Ok(Self::Array(TraeJsonArrayValues::new(bytes, range)?)),
            Some(b'{') => Ok(Self::Object(TraeJsonObjectFields::new(bytes, range)?)),
            _ => Err(CaptureError::InvalidPayload(
                "Trae session container must be an array or object".to_owned(),
            )),
        }
    }

    pub(super) fn next_range(&mut self) -> Result<Option<Range<usize>>> {
        match self {
            Self::Array(values) => values.next_range(),
            Self::Object(fields) => fields
                .next_field()
                .map(|field| field.map(|(_, range)| range)),
        }
    }
}

pub(super) struct TraeJsonObjectFields<'a> {
    bytes: &'a [u8],
    cursor: usize,
    end: usize,
    done: bool,
}

impl<'a> TraeJsonObjectFields<'a> {
    fn new(bytes: &'a [u8], range: Range<usize>) -> Result<Self> {
        let range = trae_json_trimmed_range(bytes, range)?;
        if bytes.get(range.start) != Some(&b'{') || bytes.get(range.end - 1) != Some(&b'}') {
            return Err(CaptureError::InvalidPayload(
                "Trae session must be a JSON object".to_owned(),
            ));
        }
        Ok(Self {
            bytes,
            cursor: range.start + 1,
            end: range.end - 1,
            done: false,
        })
    }

    fn next_field(&mut self) -> Result<Option<(String, Range<usize>)>> {
        if self.done {
            return Ok(None);
        }
        self.cursor = trae_json_skip_ws(self.bytes, self.cursor, self.end);
        if self.cursor == self.end {
            self.done = true;
            return Ok(None);
        }
        let key_start = self.cursor;
        let key_end = trae_json_string_end(self.bytes, key_start, self.end)?;
        let key: String = serde_json::from_slice(&self.bytes[key_start..key_end])?;
        self.cursor = trae_json_skip_ws(self.bytes, key_end, self.end);
        if self.bytes.get(self.cursor) != Some(&b':') {
            return Err(CaptureError::InvalidPayload(
                "Trae JSON object field is missing a colon".to_owned(),
            ));
        }
        self.cursor = trae_json_skip_ws(self.bytes, self.cursor + 1, self.end);
        let value_start = self.cursor;
        let value_end = trae_json_value_end(self.bytes, value_start, self.end)?;
        self.cursor = trae_json_skip_ws(self.bytes, value_end, self.end);
        if self.cursor < self.end {
            if self.bytes[self.cursor] != b',' {
                return Err(CaptureError::InvalidPayload(
                    "Trae JSON object has an invalid separator".to_owned(),
                ));
            }
            self.cursor += 1;
        } else {
            self.done = true;
        }
        Ok(Some((key, value_start..value_end)))
    }
}

fn trae_json_trimmed_range(bytes: &[u8], range: Range<usize>) -> Result<Range<usize>> {
    let start = trae_json_skip_ws(bytes, range.start, range.end);
    let mut end = range.end;
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if start == end {
        return Err(CaptureError::InvalidPayload(
            "Trae ItemTable JSON is empty".to_owned(),
        ));
    }
    Ok(start..end)
}

fn trae_json_skip_ws(bytes: &[u8], mut cursor: usize, end: usize) -> usize {
    while cursor < end && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    cursor
}

fn trae_json_string_end(bytes: &[u8], start: usize, end: usize) -> Result<usize> {
    if bytes.get(start) != Some(&b'"') {
        return Err(CaptureError::InvalidPayload(
            "Trae JSON object key must be a string".to_owned(),
        ));
    }
    let mut cursor = start + 1;
    while cursor < end {
        match bytes[cursor] {
            b'\\' => cursor = cursor.saturating_add(2),
            b'"' => return Ok(cursor + 1),
            _ => cursor += 1,
        }
    }
    Err(CaptureError::InvalidPayload(
        "Trae JSON contains an unterminated string".to_owned(),
    ))
}

fn trae_json_value_end(bytes: &[u8], start: usize, end: usize) -> Result<usize> {
    let Some(first) = bytes.get(start).copied() else {
        return Err(CaptureError::InvalidPayload(
            "Trae JSON value is missing".to_owned(),
        ));
    };
    if first == b'"' {
        return trae_json_string_end(bytes, start, end);
    }
    if matches!(first, b'{' | b'[') {
        let mut stack = Vec::with_capacity(32);
        stack.push(if first == b'{' { b'}' } else { b']' });
        let mut cursor = start + 1;
        while cursor < end {
            match bytes[cursor] {
                b'"' => cursor = trae_json_string_end(bytes, cursor, end)?,
                b'{' | b'[' => {
                    if stack.len() >= 128 {
                        return Err(CaptureError::InvalidPayload(
                            "Trae JSON nesting exceeds 128 levels".to_owned(),
                        ));
                    }
                    stack.push(if bytes[cursor] == b'{' { b'}' } else { b']' });
                    cursor += 1;
                }
                byte if Some(&byte) == stack.last() => {
                    stack.pop();
                    cursor += 1;
                    if stack.is_empty() {
                        return Ok(cursor);
                    }
                }
                _ => cursor += 1,
            }
        }
        return Err(CaptureError::InvalidPayload(
            "Trae JSON container is unterminated".to_owned(),
        ));
    }
    let mut cursor = start;
    while cursor < end && !matches!(bytes[cursor], b',' | b']' | b'}') {
        cursor += 1;
    }
    let mut value_end = cursor;
    while value_end > start && bytes[value_end - 1].is_ascii_whitespace() {
        value_end -= 1;
    }
    if value_end == start {
        return Err(CaptureError::InvalidPayload(
            "Trae JSON primitive is empty".to_owned(),
        ));
    }
    Ok(value_end)
}

//! Provider-owned logical-row reconstruction shared by NativePath and verified recovery.

use ctx_history_core::EventType;
use serde_json::{json, Value};

use crate::native_source::NativeSqliteValue;
use crate::{CaptureError, Result};

use super::normalization::{
    opencode_event_text, opencode_event_type, opencode_message_part_role,
    opencode_message_type_from_data, opencode_part_type, opencode_text_part_text,
    opencode_tool_part_event_data,
};
use super::schema::{OpenCodeCapturedShape, OpenCodeMessageRow, OpenCodeSqliteDialect};

pub(super) struct OpenCodeLogicalRow {
    pub(super) message_id: String,
    pub(super) entry_type: String,
    pub(super) time_created: i64,
    pub(super) message_data: String,
    pub(super) part_data: String,
    pub(super) part_id: String,
    pub(super) part_type: String,
    pub(super) source_table: String,
}

pub(super) struct OpenCodeProjectedMessage {
    pub(super) row: OpenCodeOptionalMessage,
    pub(super) data: Value,
}

pub(super) struct OpenCodeOptionalMessage {
    pub(super) event: Option<OpenCodeMessageRow>,
}

impl OpenCodeLogicalRow {
    pub(super) fn message_row(&self) -> Result<OpenCodeProjectedMessage> {
        if self.source_table == OpenCodeCapturedShape::MessagePart.label() {
            return self.message_part_row();
        }
        let data = serde_json::from_str::<Value>(&self.message_data).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "invalid JSON in {} message {}: {error}",
                self.source_table, self.message_id
            ))
        })?;
        let entry_type = if !self.entry_type.trim().is_empty() && self.entry_type != "message" {
            self.entry_type.clone()
        } else {
            opencode_message_type_from_data(&data).unwrap_or_else(|| self.entry_type.clone())
        };
        Ok(OpenCodeProjectedMessage {
            row: OpenCodeOptionalMessage {
                event: Some(OpenCodeMessageRow {
                    id: self.message_id.clone(),
                    entry_type,
                }),
            },
            data,
        })
    }

    fn message_part_row(&self) -> Result<OpenCodeProjectedMessage> {
        let part_data = serde_json::from_str::<Value>(&self.part_data).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "invalid JSON in message part {}: {error}",
                self.part_id
            ))
        })?;
        let role = opencode_message_part_role(&part_data);
        let part_type = opencode_part_type(Some(&self.part_type), &part_data);
        let is_patch = part_type == "patch";
        let (event_data, emits_event) =
            if let Some(text) = opencode_text_part_text(&part_type, &part_data) {
                let emits_event = matches!(role.as_str(), "assistant" | "user" | "system")
                    && !text.trim().is_empty();
                (
                    Some(json!({
                    "role": role.clone(),
                    "time": { "created": self.time_created },
                    "text": text,
                    "source_table": "message+part",
                    "message_id": self.message_id.clone(),
                    "part_id": self.part_id.clone(),
                    "part_type": part_type.clone(),
                    })),
                    emits_event,
                )
            } else if let Some(tool) = opencode_tool_part_event_data(
                &self.message_id,
                &self.part_id,
                &part_type,
                self.time_created,
                &part_data,
            ) {
                (Some(tool), true)
            } else if is_patch {
                (
                    Some(json!({
                        "role": role.clone(),
                        "time": { "created": self.time_created },
                        "source_table": "message+part",
                        "message_id": self.message_id.clone(),
                        "part_id": self.part_id.clone(),
                        "part_type": part_type.clone(),
                    })),
                    false,
                )
            } else {
                (None, false)
            };
        let event = emits_event.then(|| OpenCodeMessageRow {
            id: format!("{}:{}", self.message_id, self.part_id),
            entry_type: if matches!(part_type.as_str(), "tool" | "tool_result" | "result") {
                "tool".to_owned()
            } else {
                role
            },
        });
        Ok(OpenCodeProjectedMessage {
            row: OpenCodeOptionalMessage { event },
            data: event_data.unwrap_or(Value::Null),
        })
    }
}

pub(crate) fn opencode_complete_message(
    values: &[NativeSqliteValue],
    dialect: &OpenCodeSqliteDialect,
) -> Result<(String, String, String)> {
    if values.len() != 14 {
        return Err(CaptureError::InvalidPayload(
            "OpenCode complete-content row has an invalid value count".into(),
        ));
    }
    if opencode_integer_value(values, 1)? == 0 {
        return Err(CaptureError::InvalidPayload(
            "OpenCode complete-content row has no verified parent".into(),
        ));
    }
    let session_id = opencode_text_value(values, 3)?.to_owned();
    let time_created = opencode_integer_value(values, 7)?;
    let logical = OpenCodeLogicalRow {
        message_id: opencode_text_value(values, 2)?.to_owned(),
        entry_type: opencode_text_value(values, 4)?.to_owned(),
        time_created,
        message_data: opencode_text_value(values, 9)?.to_owned(),
        part_data: opencode_text_value(values, 10)?.to_owned(),
        part_id: opencode_text_value(values, 11)?.to_owned(),
        part_type: opencode_text_value(values, 12)?.to_owned(),
        source_table: opencode_text_value(values, 13)?.to_owned(),
    };
    let projected = logical.message_row()?;
    let message = projected.row.event.ok_or_else(|| {
        CaptureError::InvalidPayload("OpenCode row does not emit a message".into())
    })?;
    let event_type = opencode_event_type(&message.entry_type, &projected.data);
    if event_type != EventType::Message {
        return Err(CaptureError::InvalidPayload(
            "OpenCode row is not an ordinary message".into(),
        ));
    }
    let text = opencode_event_text(&message.entry_type, &projected.data, event_type, dialect);
    Ok((session_id, message.id, text))
}

pub(super) fn opencode_text_value(values: &[NativeSqliteValue], index: usize) -> Result<&str> {
    match values.get(index) {
        Some(NativeSqliteValue::Text(value)) => Ok(value),
        _ => Err(CaptureError::SystemInvariant(
            "OpenCode logical row has an invalid text value",
        )),
    }
}

pub(super) fn opencode_integer_value(values: &[NativeSqliteValue], index: usize) -> Result<i64> {
    match values.get(index) {
        Some(NativeSqliteValue::Integer(value)) => Ok(*value),
        _ => Err(CaptureError::SystemInvariant(
            "OpenCode logical row has an invalid integer value",
        )),
    }
}

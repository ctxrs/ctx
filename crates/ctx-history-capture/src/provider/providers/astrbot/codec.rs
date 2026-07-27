use ctx_history_core::EventRole;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowsBatchError;
use crate::captured_batch::{
    CapturedRecord, CapturedSqliteValue, NativeLocator, NativePosition,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::importer::{ProviderProjectionFatal, ProviderProjectionResult};
use crate::provider::normalization::{provider_role, provider_value_text};
use crate::{CaptureError, Result};

pub(super) const ASTRBOT_CHECKPOINT_SCHEMA_VERSION: u32 = 1;
pub(super) const ASTRBOT_CONVERSATION_RECORD_KIND: &str = "astrbot-conversation-v1";
pub(super) const ASTRBOT_PLATFORM_MESSAGE_RECORD_KIND: &str = "astrbot-platform-message-v1";
pub(super) const ASTRBOT_CONVERSATION_ORDER_VIOLATION_RECORD_KIND: &str =
    "astrbot-conversation-order-violation-v1";
pub(super) const ASTRBOT_PLATFORM_MESSAGE_ORDER_VIOLATION_RECORD_KIND: &str =
    "astrbot-platform-message-order-violation-v1";
const ASTRBOT_POSITION_KIND: &str = "astrbot-logical-keyset-v1";
const ASTRBOT_LOCATOR_KIND: &str = "astrbot-logical-row-v1";
const ASTRBOT_POSITION_BYTES: usize = 1 + 8 + 8;
const ASTRBOT_CONVERSATION_VALUE_COUNT: usize = 11;
const ASTRBOT_PLATFORM_MESSAGE_VALUE_COUNT: usize = 8;
const ASTRBOT_PLATFORM_MESSAGE_LINK_VALUE_COUNT: usize = 2;

#[derive(Debug, Clone)]
pub(super) struct AstrBotConversationRow {
    pub(super) row_id: i64,
    pub(super) inner_conversation_id: Option<String>,
    pub(super) conversation_id: String,
    pub(super) platform_id: Option<String>,
    pub(super) user_id: Option<String>,
    pub(super) content: String,
    pub(super) title: Option<String>,
    pub(super) persona_id: Option<String>,
    pub(super) token_usage: Option<String>,
    pub(super) created_at: Option<i64>,
    pub(super) updated_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub(super) struct AstrBotPlatformMessageRow {
    pub(super) id: i64,
    pub(super) platform_id: Option<String>,
    pub(super) user_id: Option<String>,
    pub(super) sender_id: Option<String>,
    pub(super) sender_name: Option<String>,
    pub(super) content: Option<String>,
    pub(super) llm_checkpoint_id: Option<String>,
    pub(super) created_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AstrBotPlatformMessageLink {
    pub(super) provider_session_id: String,
    pub(super) parent_created_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AstrBotPhase {
    Conversations,
    PlatformMessages,
}

impl AstrBotPhase {
    fn tag(self) -> u8 {
        match self {
            Self::Conversations => 1,
            Self::PlatformMessages => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::Conversations),
            2 => Ok(Self::PlatformMessages),
            _ => Err(CaptureError::InvalidPayload(
                "AstrBot cursor has an unknown phase".to_owned(),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AstrBotKeyset {
    pub(super) phase: AstrBotPhase,
    pub(super) next_ordinal: u64,
    pub(super) physical_rowid: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(super) struct AstrBotParserCheckpoint {
    pub(super) schema_version: u32,
    pub(super) source_shape_validated: bool,
}

impl AstrBotParserCheckpoint {
    pub(super) fn empty() -> Self {
        Self {
            schema_version: ASTRBOT_CHECKPOINT_SCHEMA_VERSION,
            source_shape_validated: false,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        if self.schema_version != ASTRBOT_CHECKPOINT_SCHEMA_VERSION {
            return Err(CaptureError::InvalidPayload(
                "AstrBot parser checkpoint has an unsupported schema version".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(super) fn astrbot_conversation_values(row: AstrBotConversationRow) -> Vec<CapturedSqliteValue> {
    vec![
        CapturedSqliteValue::Integer(row.row_id),
        astrbot_captured_optional_text(row.inner_conversation_id),
        CapturedSqliteValue::Text(row.conversation_id),
        astrbot_captured_optional_text(row.platform_id),
        astrbot_captured_optional_text(row.user_id),
        CapturedSqliteValue::Text(row.content),
        astrbot_captured_optional_text(row.title),
        astrbot_captured_optional_text(row.persona_id),
        astrbot_captured_optional_text(row.token_usage),
        astrbot_captured_optional_integer(row.created_at),
        astrbot_captured_optional_integer(row.updated_at),
    ]
}

pub(super) fn astrbot_platform_message_values(
    row: AstrBotPlatformMessageRow,
) -> Vec<CapturedSqliteValue> {
    vec![
        CapturedSqliteValue::Integer(row.id),
        astrbot_captured_optional_text(row.platform_id),
        astrbot_captured_optional_text(row.user_id),
        astrbot_captured_optional_text(row.sender_id),
        astrbot_captured_optional_text(row.sender_name),
        astrbot_captured_optional_text(row.content),
        astrbot_captured_optional_text(row.llm_checkpoint_id),
        astrbot_captured_optional_integer(row.created_at),
    ]
}

pub(super) fn decode_astrbot_conversation(
    values: &[CapturedSqliteValue],
) -> Result<AstrBotConversationRow> {
    if values.len() != ASTRBOT_CONVERSATION_VALUE_COUNT {
        return Err(CaptureError::InvalidPayload(
            "AstrBot conversation logical row has an unexpected value count".to_owned(),
        ));
    }
    Ok(AstrBotConversationRow {
        row_id: astrbot_required_integer(values, 0, "conversation row id")?,
        inner_conversation_id: astrbot_optional_text(values, 1, "inner_conversation_id")?,
        conversation_id: astrbot_required_text(values, 2, "conversation_id")?,
        platform_id: astrbot_optional_text(values, 3, "platform_id")?,
        user_id: astrbot_optional_text(values, 4, "user_id")?,
        content: astrbot_required_text(values, 5, "conversation content")?,
        title: astrbot_optional_text(values, 6, "conversation title")?,
        persona_id: astrbot_optional_text(values, 7, "persona_id")?,
        token_usage: astrbot_optional_text(values, 8, "token_usage")?,
        created_at: astrbot_optional_integer(values, 9, "conversation created_at")?,
        updated_at: astrbot_optional_integer(values, 10, "conversation updated_at")?,
    })
}

pub(super) fn decode_astrbot_platform_message(
    values: &[CapturedSqliteValue],
) -> Result<(
    AstrBotPlatformMessageRow,
    Option<AstrBotPlatformMessageLink>,
)> {
    if values.len()
        != ASTRBOT_PLATFORM_MESSAGE_VALUE_COUNT + ASTRBOT_PLATFORM_MESSAGE_LINK_VALUE_COUNT
    {
        return Err(CaptureError::InvalidPayload(
            "AstrBot platform-message logical row has an unexpected value count".to_owned(),
        ));
    }
    let message = AstrBotPlatformMessageRow {
        id: astrbot_required_integer(values, 0, "platform message id")?,
        platform_id: astrbot_optional_text(values, 1, "platform message platform_id")?,
        user_id: astrbot_optional_text(values, 2, "platform message user_id")?,
        sender_id: astrbot_optional_text(values, 3, "platform message sender_id")?,
        sender_name: astrbot_optional_text(values, 4, "platform message sender_name")?,
        content: astrbot_optional_text(values, 5, "platform message content")?,
        llm_checkpoint_id: astrbot_optional_text(values, 6, "platform message checkpoint")?,
        created_at: astrbot_optional_integer(values, 7, "platform message created_at")?,
    };
    let provider_session_id = astrbot_optional_text(
        values,
        ASTRBOT_PLATFORM_MESSAGE_VALUE_COUNT,
        "linked provider session id",
    )?;
    let parent_created_at = astrbot_optional_integer(
        values,
        ASTRBOT_PLATFORM_MESSAGE_VALUE_COUNT + 1,
        "linked parent created_at",
    )?;
    let link = provider_session_id.map(|provider_session_id| AstrBotPlatformMessageLink {
        provider_session_id,
        parent_created_at,
    });
    if link.is_none() && parent_created_at.is_some() {
        return Err(CaptureError::InvalidPayload(
            "AstrBot unlinked platform message retained a parent timestamp".to_owned(),
        ));
    }
    Ok((message, link))
}

pub(super) fn astrbot_value<'a>(
    values: &'a [CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<&'a CapturedSqliteValue> {
    values.get(index).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("AstrBot logical row is missing {field}"))
    })
}

pub(super) fn astrbot_required_text(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<String> {
    match astrbot_value(values, index, field)? {
        CapturedSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "AstrBot logical row {field} must be text"
        ))),
    }
}

pub(super) fn astrbot_optional_text(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<String>> {
    match astrbot_value(values, index, field)? {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::InvalidPayload(format!(
            "AstrBot logical row {field} must be text or null"
        ))),
    }
}

pub(super) fn astrbot_required_integer(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<i64> {
    match astrbot_value(values, index, field)? {
        CapturedSqliteValue::Integer(value) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "AstrBot logical row {field} must be an integer"
        ))),
    }
}

pub(super) fn astrbot_optional_integer(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<i64>> {
    match astrbot_value(values, index, field)? {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Integer(value) => Ok(Some(*value)),
        _ => Err(CaptureError::InvalidPayload(format!(
            "AstrBot logical row {field} must be an integer or null"
        ))),
    }
}

pub(super) fn astrbot_captured_optional_text(value: Option<String>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Text)
}

pub(super) fn astrbot_captured_optional_integer(value: Option<i64>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer)
}

pub(super) fn initial_astrbot_position() -> Result<NativePosition> {
    NativePosition::new(ASTRBOT_POSITION_KIND, vec![0]).map_err(astrbot_captured_error)
}

pub(super) fn encode_astrbot_position(keyset: AstrBotKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(ASTRBOT_POSITION_BYTES);
    value.push(keyset.phase.tag());
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.extend_from_slice(&astrbot_ordered_i64(keyset.physical_rowid).to_be_bytes());
    NativePosition::new(ASTRBOT_POSITION_KIND, value).map_err(astrbot_captured_error)
}

pub(super) fn decode_astrbot_position(position: &NativePosition) -> Result<Option<AstrBotKeyset>> {
    if position.kind() != ASTRBOT_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "AstrBot cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != ASTRBOT_POSITION_BYTES {
        return Err(CaptureError::InvalidPayload(
            "AstrBot cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(AstrBotKeyset {
        phase: AstrBotPhase::from_tag(position.value()[0])?,
        next_ordinal: astrbot_decode_u64(&position.value()[1..9])?,
        physical_rowid: astrbot_unordered_i64(astrbot_decode_u64(&position.value()[9..17])?),
    }))
}

pub(super) fn astrbot_locator(phase: AstrBotPhase, rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(9);
    value.push(phase.tag());
    value.extend_from_slice(&astrbot_ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(ASTRBOT_LOCATOR_KIND, value).map_err(astrbot_captured_error)
}

pub(super) fn decode_astrbot_locator(locator: &NativeLocator, phase: AstrBotPhase) -> Result<i64> {
    if locator.kind() != ASTRBOT_LOCATOR_KIND
        || locator.value().len() != 9
        || locator.value()[0] != phase.tag()
    {
        return Err(CaptureError::InvalidPayload(
            "AstrBot record has an invalid native locator".to_owned(),
        ));
    }
    Ok(astrbot_unordered_i64(astrbot_decode_u64(
        &locator.value()[1..9],
    )?))
}

pub(super) fn astrbot_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("AstrBot cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

pub(super) fn astrbot_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

pub(super) fn astrbot_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

pub(super) fn astrbot_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("AstrBot byte limit exceeds u64"))
}

pub(super) fn astrbot_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn astrbot_captured_record_line(
    record: &CapturedRecord,
) -> ProviderProjectionResult<usize> {
    usize::try_from(record.ordinal())
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or_else(|| {
            ProviderProjectionFatal::system_invariant(
                "AstrBot captured record ordinal exceeds platform limits",
            )
        })
}

pub(super) fn astrbot_sqlite_batch_error(
    error: SqliteLogicalRowsBatchError<CaptureError>,
) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

pub(super) fn astrbot_provider_session_id(conversation: &AstrBotConversationRow) -> String {
    conversation
        .inner_conversation_id
        .as_ref()
        .or(Some(&conversation.conversation_id))
        .cloned()
        .unwrap_or_else(|| format!("conversation-row-{}", conversation.row_id))
}

pub(super) fn astrbot_item_id(item: &Value) -> Option<&str> {
    item.get("id")
        .or_else(|| item.get("message_id"))
        .or_else(|| item.get("checkpoint_id"))
        .and_then(Value::as_str)
}

pub(super) fn astrbot_checkpoint_id(item: &Value) -> Option<String> {
    let item_type = item
        .get("type")
        .or_else(|| item.get("role"))
        .and_then(Value::as_str)?;
    if item_type != "_checkpoint" && item_type != "checkpoint" {
        return None;
    }
    astrbot_item_id(item).map(str::to_owned)
}

pub(super) fn astrbot_role(item: &Value) -> Option<EventRole> {
    item.get("role")
        .or_else(|| item.get("type"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)))
}

pub(super) fn astrbot_item_text(item: &Value) -> Option<String> {
    item.get("content")
        .or_else(|| item.get("text"))
        .or_else(|| item.get("message"))
        .and_then(provider_value_text)
}

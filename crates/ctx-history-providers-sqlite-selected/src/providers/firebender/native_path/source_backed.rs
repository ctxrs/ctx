use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_capture_model::normalization::provider_timestamp_millis;
use ctx_history_core::{
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope, CaptureProvider, CoreActivity, CoreRecord, CoreRecordError,
    EventIdentityInput, EventType, LiteralFactKind, NativeItemKey, NativeSessionKey,
    PositionStability, ProjectionContractError, ProviderDeclaredFact, SessionIdentityInput,
    SourceAnchor, SourceAnchorScope, SourceKey, StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
};
use thiserror::Error;

use super::super::{firebender_event_parts, firebender_message_time, firebender_result_content};
use super::FirebenderRow;
use crate::{
    native_source::NativeSqliteValue, provider_sources::SqliteSourceAccessError, CaptureError,
    FIREBENDER_SQLITE_SOURCE_FORMAT,
};

mod direct;
mod direct_snapshot;

#[cfg(test)]
pub(super) use direct::firebender_database_path_and_source;
pub(crate) use direct::source_backed_driver_scoped;

const FIREBENDER_NATIVE_SESSION_NAMESPACE: &str = "firebender.chat-session";
const FIREBENDER_NATIVE_EVENT_NAMESPACE: &str = "firebender.message";
const FIREBENDER_POSITION_KIND: &str = "firebender.messages-json-index";
const FIREBENDER_LOGICAL_SESSION_KIND: &str = "firebender-chat-session";
const FIREBENDER_LOGICAL_EVENT_KIND: &str = "firebender-message";
pub(super) const FIREBENDER_SOURCE_SCHEMA_VARIANT: &str = "firebender-chat-sessions-v1";

#[derive(Debug, Error)]
pub(crate) enum FirebenderSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Route(#[from] ctx_history_capture_runtime::SourceBackedRouteError),
    #[error("Firebender source-backed scan accounting overflowed")]
    CountOverflow,
}

impl From<ctx_history_source_io::SourceIoError> for FirebenderSourceBackedError {
    fn from(error: ctx_history_source_io::SourceIoError) -> Self {
        Self::Capture(error.into())
    }
}

impl From<ctx_history_source_sqlite::SqliteIoError> for FirebenderSourceBackedError {
    fn from(error: ctx_history_source_sqlite::SqliteIoError) -> Self {
        Self::Capture(error.into())
    }
}

pub(crate) type FirebenderSourceBackedResult<T> =
    std::result::Result<T, FirebenderSourceBackedError>;

#[cfg(test)]
pub(super) fn firebender_source_key() -> FirebenderSourceBackedResult<SourceKey> {
    firebender_source_key_scoped(SourceAnchorScope::Unqualified)
}

pub(super) fn firebender_source_key_scoped(
    source_scope: SourceAnchorScope,
) -> FirebenderSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive_scoped(
        CaptureProvider::Firebender.as_str(),
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        FIREBENDER_SOURCE_SCHEMA_VARIANT,
        super::FIREBENDER_SOURCE_IDENTITY_REVISION,
        SourceAnchor::CatalogLineage(super::FIREBENDER_SELECTED_CATALOG_LINEAGE_V1),
        source_scope,
    )?)
}

pub(super) fn firebender_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> FirebenderSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        FIREBENDER_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: FIREBENDER_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn firebender_core_record(
    source: &SourceKey,
    session_id: StableEntityId,
    workspace: Option<&str>,
    row: &FirebenderRow,
    message_index: usize,
    message: &serde_json::Value,
) -> FirebenderSourceBackedResult<Option<CoreRecord>> {
    let message_index_u64 =
        u64::try_from(message_index).map_err(|_| FirebenderSourceBackedError::CountOverflow)?;
    let event = firebender_event_parts(
        message,
        firebender_message_occurred_at(row, message_index, message),
    );
    let output = if event.event_type == EventType::ToolOutput {
        let Some(body) = firebender_result_content(message) else {
            return Ok(None);
        };
        Some(body)
    } else {
        None
    };
    let body = output.clone().unwrap_or_else(|| event.text.clone());
    let body = if body.is_empty() {
        format!("Firebender {}", event.event_type.as_str())
    } else {
        body
    };
    let native_item_key = message_native_key(message, message_index_u64)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: FIREBENDER_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        message_index_u64,
        event.event_type.as_str(),
        direct::DIRECT_PARSER_REVISION,
        body,
    )?;
    record.agent_scope = Some(AgentScope::Primary);
    record.provider_session_id = Some(row.id.clone());
    record.native_event_id = Some(TypedKey::composite(vec![
        TypedKey::I64(row.rowid),
        TypedKey::U64(message_index_u64),
    ])?);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = event.role.map(|role| role.as_str().to_owned());
    let facts = workspace
        .map(|workspace| ProviderDeclaredFact {
            kind: LiteralFactKind::Workspace,
            value: workspace.to_owned(),
        })
        .into_iter()
        .collect::<Vec<_>>();
    let (provider_call_id, invocation, result) = firebender_activity(
        message,
        event.event_type,
        event.occurred_at.timestamp_millis(),
    )?;
    record.content.structured_content = Some(message.clone());
    if invocation.is_some() || result.is_some() || !facts.is_empty() {
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result,
            facts,
        });
    }
    fit_firebender_content(&mut record)?;
    record.validate_contract()?;
    Ok(Some(record))
}

fn fit_firebender_content(record: &mut CoreRecord) -> FirebenderSourceBackedResult<()> {
    if record.content.encoded_content_bytes()? > ctx_history_core::MAX_CORE_CONTENT_BYTES {
        let capture = record.content.activity.as_mut().and_then(|activity| {
            activity
                .invocation
                .as_mut()
                .map(|invocation| &mut invocation.arguments)
                .or_else(|| {
                    activity
                        .result
                        .as_mut()
                        .map(|result| &mut result.structured_content)
                })
        });
        if let Some(capture @ ActivityJsonCapture::Present { .. }) = capture {
            let observed_encoded_bytes = match capture {
                ActivityJsonCapture::Present { value } => serde_json::to_vec(value)
                    .ok()
                    .and_then(|encoded| u64::try_from(encoded.len()).ok()),
                _ => None,
            };
            *capture = ActivityJsonCapture::Omitted {
                reason: "size_limit".to_owned(),
                observed_encoded_bytes,
            };
        }
    }
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    Ok(())
}

pub(super) fn firebender_workspace(database_path: &Path) -> Option<String> {
    let firebender_dir = database_path.parent()?;
    if firebender_dir.file_name().and_then(|name| name.to_str()) != Some("firebender") {
        return None;
    }
    let idea_dir = firebender_dir.parent()?;
    if idea_dir.file_name().and_then(|name| name.to_str()) != Some(".idea") {
        return None;
    }
    idea_dir
        .parent()
        .map(|workspace| workspace.display().to_string())
}

fn message_native_key(
    message: &serde_json::Value,
    message_index: u64,
) -> FirebenderSourceBackedResult<NativeItemKey> {
    if let Some(native_id) = message
        .get("id")
        .or_else(|| message.get("tool_call_id"))
        .or_else(|| message.get("toolCallId"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_LINKAGE_BYTES)
    {
        return Ok(NativeItemKey::native_id(
            FIREBENDER_NATIVE_EVENT_NAMESPACE,
            TypedKey::utf8(native_id)?,
        )?);
    }
    Ok(NativeItemKey::certified_position(
        FIREBENDER_POSITION_KIND,
        TypedKey::U64(message_index),
        PositionStability::StableSlot,
    )?)
}

fn firebender_message_occurred_at(
    row: &FirebenderRow,
    message_index: usize,
    message: &serde_json::Value,
) -> DateTime<Utc> {
    let started_at = provider_timestamp_millis(Some(row.created_at), DateTime::<Utc>::UNIX_EPOCH);
    let offset = i64::try_from(message_index).unwrap_or(i64::MAX);
    firebender_message_time(message, started_at + chrono::Duration::milliseconds(offset))
}

fn firebender_activity(
    message: &serde_json::Value,
    event_type: EventType,
    occurred_at_unix_ms: i64,
) -> FirebenderSourceBackedResult<(
    Option<TypedKey>,
    Option<ActivityInvocation>,
    Option<ActivityResult>,
)> {
    let call_id = exact_direct_string(
        message,
        &["tool_call_id", "toolCallId", "call_id", "callId"],
    )
    .flatten()
    .filter(|value| value.len() <= MAX_LINKAGE_BYTES);
    let Some(call_id) = call_id else {
        return Ok((None, None, None));
    };
    let provider_call_id = Some(TypedKey::utf8(call_id)?);
    if event_type == EventType::ToolCall {
        let tool = exact_direct_string(message, &["name", "tool_name", "toolName"])
            .flatten()
            .filter(|value| !value.is_empty() && value.len() <= MAX_LINKAGE_BYTES);
        let Some(tool) = tool else {
            return Ok((None, None, None));
        };
        // Firebender treats multiple aliases with the same decoded JSON value
        // as one exact selector. Conflicting or malformed aliases abstain.
        let arguments = match exact_direct_json(message, &["arguments", "args", "input"]) {
            Some(Some(value)) => ActivityJsonCapture::Present {
                value: value.clone(),
            },
            Some(None) => ActivityJsonCapture::Absent,
            None => ActivityJsonCapture::Unavailable,
        };
        return Ok((
            provider_call_id,
            Some(ActivityInvocation {
                protocol: None,
                server: None,
                tool: tool.to_owned(),
                arguments,
                started_at_unix_ms: Some(occurred_at_unix_ms),
            }),
            None,
        ));
    }
    if event_type != EventType::ToolOutput {
        return Ok((None, None, None));
    }
    Ok((
        provider_call_id,
        None,
        Some(ActivityResult {
            status: exact_direct_string(message, &["status", "state", "outcome"])
                .flatten()
                .map(str::to_owned),
            completed_at_unix_ms: Some(occurred_at_unix_ms),
            duration_ns: message
                .get("duration_ms")
                .or_else(|| message.get("durationMs"))
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| value.checked_mul(1_000_000)),
            text: ActivityTextCapture::NormalizedBody,
            structured_content: ActivityJsonCapture::Present {
                value: message.clone(),
            },
        }),
    ))
}

/// `Some(None)` means absent, `Some(Some(_))` means one exact value, and
/// `None` means the native shape is ambiguous or malformed.
const MAX_LINKAGE_BYTES: usize = 16 * 1024;

fn exact_direct_string<'a>(
    message: &'a serde_json::Value,
    keys: &[&str],
) -> Option<Option<&'a str>> {
    let object = message.as_object()?;
    let mut selected = None;
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        let value = value.as_str()?;
        if value.is_empty() {
            return None;
        }
        match selected {
            Some(existing) if existing != value => return None,
            Some(_) => {}
            None => selected = Some(value),
        }
    }
    Some(selected)
}

/// `Some(None)` means absent, `Some(Some(_))` means one exact decoded value,
/// and `None` means conflicting or malformed aliases.
fn exact_direct_json<'a>(
    message: &'a serde_json::Value,
    keys: &[&str],
) -> Option<Option<&'a serde_json::Value>> {
    let object = message.as_object()?;
    let mut selected = None;
    for key in keys {
        let Some(value) = object.get(*key).filter(|value| !value.is_null()) else {
            continue;
        };
        match selected {
            Some(existing) if existing != value => return None,
            Some(_) => {}
            None => selected = Some(value),
        }
    }
    Some(selected)
}

pub(super) fn canonical_row_bytes(row: &FirebenderRow) -> FirebenderSourceBackedResult<u64> {
    let values = row.logical_values();
    values.iter().try_fold(8_u64, |total, value| {
        let value_bytes = match value {
            NativeSqliteValue::Null => 1,
            NativeSqliteValue::Integer(_) => 9,
            NativeSqliteValue::Text(value) => checked_len(value.len())?.saturating_add(9),
        };
        total
            .checked_add(value_bytes)
            .ok_or(FirebenderSourceBackedError::CountOverflow)
    })
}

fn checked_len(value: usize) -> FirebenderSourceBackedResult<u64> {
    u64::try_from(value).map_err(|_| FirebenderSourceBackedError::CountOverflow)
}

pub(super) fn increment(target: &mut u64, value: u64) -> FirebenderSourceBackedResult<()> {
    *target = target
        .checked_add(value)
        .ok_or(FirebenderSourceBackedError::CountOverflow)?;
    Ok(())
}

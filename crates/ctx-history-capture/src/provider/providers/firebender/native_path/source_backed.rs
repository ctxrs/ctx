use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CoreRecord, CoreRecordError,
    EventIdentityInput, EventType, NativeItemKey, NativeSessionKey, PositionStability,
    ProjectionContractError, SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId,
    TypedKey,
};
use thiserror::Error;

use super::super::{
    firebender_event_parts, firebender_message_time, firebender_output_evidence,
    firebender_result_content, FirebenderOutputEvidence,
};
use super::FirebenderRow;
use crate::{
    native_source::NativeSqliteValue, provider::normalization::provider_timestamp_millis,
    CaptureError, FIREBENDER_SQLITE_SOURCE_FORMAT,
};

mod direct;
mod direct_snapshot;

#[cfg(test)]
pub(super) use direct::firebender_database_path_and_source;
pub(crate) use direct::register_source_backed_route;

const FIREBENDER_NATIVE_SESSION_NAMESPACE: &str = "firebender.chat-session";
const FIREBENDER_NATIVE_EVENT_NAMESPACE: &str = "firebender.message";
const FIREBENDER_POSITION_KIND: &str = "firebender.messages-json-index";
const FIREBENDER_LOGICAL_SESSION_KIND: &str = "firebender-chat-session";
const FIREBENDER_LOGICAL_EVENT_KIND: &str = "firebender-message";
const FIREBENDER_SOURCE_SCHEMA_VARIANT: &str = "firebender-chat-sessions-v1";

#[derive(Debug, Error)]
pub(crate) enum FirebenderSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Route(#[from] crate::provider::source_backed::SourceBackedRouteError),
    #[error("Firebender source-backed scan accounting overflowed")]
    CountOverflow,
}

pub(crate) type FirebenderSourceBackedResult<T> =
    std::result::Result<T, FirebenderSourceBackedError>;

pub(super) fn firebender_source_key() -> FirebenderSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive(
        CaptureProvider::Firebender.as_str(),
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        FIREBENDER_SOURCE_SCHEMA_VARIANT,
        super::FIREBENDER_SOURCE_IDENTITY_REVISION,
        SourceAnchor::CatalogLineage(super::FIREBENDER_SELECTED_CATALOG_LINEAGE_V1),
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
        let evidence = firebender_output_evidence(message);
        let Some(body) = firebender_result_content(message) else {
            return Ok(None);
        };
        let Some(linkage) = firebender_result_linkage(message, &evidence) else {
            return Ok(None);
        };
        Some((body, linkage))
    } else {
        None
    };
    let body = output
        .as_ref()
        .map_or_else(|| event.text.clone(), |(body, _)| body.clone());
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
        session_id,
        source.clone(),
        message_index_u64,
        event.event_type.as_str(),
        ctx_history_core::AgentType::Primary.as_str(),
        true,
        direct::DIRECT_PARSER_REVISION,
        body,
    )?;
    record.provider_session_id = Some(row.id.clone());
    record.native_event_id = Some(TypedKey::composite(vec![
        TypedKey::I64(row.rowid),
        TypedKey::U64(message_index_u64),
    ])?);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = event.role.map(|role| role.as_str().to_owned());
    record.workspace = workspace.map(str::to_owned);
    if let Some((_, linkage)) = output {
        record.content.structured_content = Some(serde_json::json!({
            "provider_native_result": linkage,
        }));
    }
    record.validate_contract()?;
    Ok(Some(record))
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

fn firebender_result_linkage(
    message: &serde_json::Value,
    evidence: &FirebenderOutputEvidence,
) -> Option<serde_json::Value> {
    let call_id = exact_direct_string(
        message,
        &["tool_call_id", "toolCallId", "call_id", "callId"],
    )?;
    let tool_name = exact_direct_string(message, &["name", "tool_name", "toolName"])?;
    let linkage_exact = call_id.is_some_and(|value| value.len() <= MAX_LINKAGE_BYTES);
    let call_id = call_id
        .filter(|value| value.len() <= MAX_LINKAGE_BYTES)
        .map(str::to_owned);
    let tool_name = tool_name
        .filter(|value| value.len() <= MAX_LINKAGE_BYTES)
        .map(str::to_owned);
    let result_outcome = if evidence.timeout {
        "timeout"
    } else if evidence.failure {
        "failure"
    } else if evidence.success {
        "success"
    } else {
        "unknown"
    };
    Some(serde_json::json!({
        "call_id": call_id,
        "tool_name": tool_name,
        "linkage_exact": linkage_exact,
        "result_outcome": result_outcome,
        "exit_code": evidence.exit_code,
        "duration_ms": evidence.duration_ms,
    }))
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
        let value = value.as_str()?.trim();
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

pub(super) fn canonical_row_bytes(row: &FirebenderRow) -> FirebenderSourceBackedResult<u64> {
    let values = row.logical_values();
    values.iter().try_fold(8_u64, |total, value| {
        let value_bytes = match value {
            NativeSqliteValue::Null => 1,
            NativeSqliteValue::Integer(_) | NativeSqliteValue::RealBits(_) => 9,
            NativeSqliteValue::Text(value) => checked_len(value.len())?.saturating_add(9),
            NativeSqliteValue::Blob(value) => checked_len(value.len())?.saturating_add(9),
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

use std::{collections::BTreeMap, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, EventIdentityInput, EventType,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    PositionStability, ProjectionContractError, SessionIdentityInput, SourceAnchor, SourceKey,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use rusqlite::{params_from_iter, Connection};
use thiserror::Error;

use super::super::{
    firebender_event_parts, firebender_message_time, firebender_output_evidence,
    FirebenderOutputEvidence,
};
use super::{FirebenderRow, FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES};
use crate::{
    native_source::NativeSqliteValue,
    provider::{
        normalization::provider_timestamp_millis,
        sqlite::{sqlite_table_columns, SqliteLengthPreflightGuard},
    },
    CaptureError, Result as CaptureResult, FIREBENDER_SQLITE_SOURCE_FORMAT,
};

mod direct;
mod direct_snapshot;
mod hydration;

pub(crate) use direct::register_source_backed_route;
#[cfg(test)]
pub(crate) use direct::{
    reset_route_work_counters, revalidate_missing_after_for_test, route_work_counters,
    scan_for_test,
};
#[cfg(test)]
pub(crate) use hydration::resolver_for_test;

const FIREBENDER_SOURCE_ANCHOR_NAMESPACE: &str = "firebender.explicit-chat-history";
const FIREBENDER_NATIVE_SESSION_NAMESPACE: &str = "firebender.chat-session";
const FIREBENDER_NATIVE_EVENT_NAMESPACE: &str = "firebender.message";
const FIREBENDER_POSITION_KIND: &str = "firebender.messages-json-index";
const FIREBENDER_LOGICAL_SESSION_KIND: &str = "firebender-chat-session";
const FIREBENDER_LOGICAL_EVENT_KIND: &str = "firebender-message";
const FIREBENDER_SOURCE_SCHEMA_VARIANT: &str = "firebender-chat-sessions-v1";
const FIREBENDER_LOCATOR_RELATION: &str = "chat_sessions.messages_json";

#[derive(Debug, Error)]
pub(crate) enum FirebenderSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Route(#[from] crate::provider::source_backed::SourceBackedRouteError),
    #[error("Firebender source-backed scan accounting overflowed")]
    CountOverflow,
    #[error("Firebender source-backed locator is malformed")]
    InvalidLocator,
    #[error("Firebender source-backed row exceeds the bounded hydration limit")]
    HydrationTooLarge,
}

pub(crate) type FirebenderSourceBackedResult<T> =
    std::result::Result<T, FirebenderSourceBackedError>;

pub(super) fn firebender_source_key(
    route_identity: &str,
) -> FirebenderSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        FIREBENDER_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(route_identity)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Firebender.as_str(),
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        FIREBENDER_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
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
pub(super) fn firebender_document(
    source: &SourceKey,
    session_id: StableEntityId,
    source_path: &str,
    workspace: Option<&str>,
    row: &FirebenderRow,
    message_index: usize,
    message: &serde_json::Value,
    row_digest: [u8; 32],
) -> FirebenderSourceBackedResult<Option<LexicalDocument>> {
    let message_index_u64 =
        u64::try_from(message_index).map_err(|_| FirebenderSourceBackedError::CountOverflow)?;
    let event = firebender_event_parts(
        &row.id,
        message_index_u64,
        message,
        firebender_message_occurred_at(row, message_index, message),
    );
    let body = if event.event_type == EventType::ToolOutput {
        let evidence = firebender_output_evidence(message);
        if !evidence.failure && !evidence.timeout {
            return Ok(None);
        }
        sparse_output_body(&evidence)
    } else {
        event.text
    };
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
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: FIREBENDER_LOCATOR_RELATION.to_owned(),
            primary_key: TypedKey::I64(row.rowid),
            row_version: Some(TypedKey::composite(vec![
                TypedKey::utf8(&row.id)?,
                TypedKey::I64(row.updated_at),
                TypedKey::U64(message_index_u64),
            ])?),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        row_digest,
    )?;
    Ok(Some(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(row.id.clone()),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: ctx_history_core::AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: message_index_u64,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: workspace.map(str::to_owned),
        cwd: None,
        touched_files: Vec::new(),
    }))
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
        .filter(|value| !value.is_empty())
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

fn sparse_output_body(evidence: &FirebenderOutputEvidence) -> String {
    let outcome = if evidence.timeout {
        "timed out"
    } else {
        "failed"
    };
    evidence.exit_code.map_or_else(
        || format!("Firebender tool output {outcome}"),
        |code| format!("Firebender tool output {outcome} with exit code {code}"),
    )
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

pub(super) fn decode_locator_coordinate(
    locator: &SourceRecordLocator,
) -> FirebenderSourceBackedResult<(i64, String, i64, u64)> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key: TypedKey::I64(rowid),
        row_version: Some(TypedKey::Composite(version)),
    } = locator.coordinate()
    else {
        return Err(FirebenderSourceBackedError::InvalidLocator);
    };
    if logical_relation != FIREBENDER_LOCATOR_RELATION {
        return Err(FirebenderSourceBackedError::InvalidLocator);
    }
    let [TypedKey::Utf8(session_id), TypedKey::I64(updated_at), TypedKey::U64(message_index)] =
        version.as_slice()
    else {
        return Err(FirebenderSourceBackedError::InvalidLocator);
    };
    Ok((*rowid, session_id.clone(), *updated_at, *message_index))
}

pub(super) fn load_exact_rows(
    conn: &Connection,
    rowids: &[i64],
) -> CaptureResult<BTreeMap<i64, FirebenderRow>> {
    if rowids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let columns = sqlite_table_columns(conn, "chat_sessions")?;
    let deleted_filter = if columns.contains("deleted_at") {
        " and deleted_at is null"
    } else {
        ""
    };
    let placeholders = (1..=rowids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let length_sql = format!(
        "select rowid,
                length(cast(id as blob)) + length(cast(name as blob)) +
                length(cast(messages_json as blob)) + length(cast(metadata_json as blob))
         from chat_sessions
         where rowid in ({placeholders}){deleted_filter}
         order by rowid"
    );
    let retained_bytes = {
        let _guard = SqliteLengthPreflightGuard::new(conn);
        let mut statement = conn.prepare(&length_sql)?;
        let rows = statement.query_map(params_from_iter(rowids), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?
    };
    for retained_bytes in retained_bytes.values() {
        if *retained_bytes < 0
            || usize::try_from(*retained_bytes).map_or(true, |bytes| {
                bytes > FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES
            })
        {
            return Err(CaptureError::InvalidPayload(
                FirebenderSourceBackedError::HydrationTooLarge.to_string(),
            ));
        }
    }
    let sql = format!(
        "select rowid, id, name, cast(created_at as integer), cast(updated_at as integer),
                messages_json, metadata_json
         from chat_sessions
         where rowid in ({placeholders}){deleted_filter}
         order by rowid"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(rowids), |row| {
        let rowid = row.get::<_, i64>(0)?;
        let messages_json: String = row.get(5)?;
        let messages =
            serde_json::from_str::<Vec<serde_json::Value>>(&messages_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
        Ok((
            rowid,
            FirebenderRow {
                rowid,
                id: row.get(1)?,
                name: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                messages_json,
                metadata_json: row.get(6)?,
                messages,
            },
        ))
    })?;
    rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()
        .map_err(CaptureError::from)
}

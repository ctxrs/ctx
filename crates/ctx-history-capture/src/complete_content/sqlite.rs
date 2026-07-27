//! Bounded complete-message recovery for provider SQLite sources.
//!
//! The resolver never opens a provider database read-write. Databases without
//! sidecars are opened through SQLite's immutable URI mode. Databases with a
//! WAL, SHM, or rollback journal are copied to a private temporary snapshot by
//! the shared provider SQLite opener before SQLite sees them. Every supported
//! request addresses one allowlisted provider row by its captured native key;
//! capture ordinals are never used as SQL offsets.

use std::{
    fs::{self, File},
    io::Read,
    time::{Duration, Instant, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType, ProviderEventEnvelope};
use rusqlite::{limits::Limit as SqliteLimit, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    captured_batch::{CapturedSqliteValue, NativeLocator},
    common::io::ensure_regular_provider_transcript_file,
    compute_payload_hash,
    provider::{
        providers::{firebender, kiro, zed},
        sqlite::open_provider_sqlite_readonly,
        sqlite::{
            ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
            ProviderSqliteSourceSnapshot,
        },
    },
    CaptureError,
};

use super::{
    CompleteContentBodyDigest, CompleteContentError, CompleteContentErrorKind,
    CompleteContentHashAuthority, CompleteContentResolver, CompleteContentSourceFamily,
    CompleteMessage, CompleteMessageRequest, PersistedCompleteContentLocatorV1, SourceVerification,
    COMPLETE_CONTENT_LOCATOR_METADATA_KEY, COMPLETE_CONTENT_MAX_BODY_BYTES,
};
#[cfg(test)]
use crate::{
    FIREBENDER_SQLITE_SOURCE_FORMAT, KIRO_SQLITE_SOURCE_FORMAT, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

const FIREBENDER_LOCATOR_KIND: &str = "firebender-chat-session-row-v1";
const KIRO_LOCATOR_KIND: &str = "kiro-conversation-row-v1";
const ZED_LOCATOR_KIND: &str = "zed-thread-row-v1";

const MAX_SQLITE_COMPLETE_REQUESTS: usize = 256;
const SQLITE_PROGRESS_INSTRUCTIONS: i32 = 1_000;
const SQLITE_RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);
const SQLITE_MAX_SCHEMA_OBJECTS: usize = 1_024;
const SQLITE_MAX_ROW_VALUES: usize = 64;
const SQLITE_MAX_SNAPSHOT_HASH_BYTES: u64 = 512 * 1024 * 1024;

mod capabilities;
use capabilities::CAPABILITIES;
pub use capabilities::{sqlite_complete_content_capabilities, SqliteCompleteContentCapability};

#[derive(Debug, Default, Clone, Copy)]
pub struct SqliteCompleteContentResolver;

impl SqliteCompleteContentResolver {
    pub fn new() -> Self {
        Self
    }
}

impl CompleteContentResolver for SqliteCompleteContentResolver {
    fn family(&self) -> CompleteContentSourceFamily {
        CompleteContentSourceFamily::Sqlite
    }

    fn supports(&self, provider: CaptureProvider, source_format: &str) -> bool {
        CAPABILITIES.iter().any(|capability| {
            capability.supported
                && capability.provider == provider
                && capability.source_format == source_format
        })
    }

    fn resolve(
        &self,
        requests: &[CompleteMessageRequest],
    ) -> Result<Vec<CompleteMessage>, CompleteContentError> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        validate_request_batch(requests)?;
        if !self.supports(first.provider, &first.source_format) {
            return Err(error(first, CompleteContentErrorKind::HydrationUnsupported));
        }
        verify_source_route(first)?;

        let before = read_source_snapshot(first)?;
        let conn = open_provider_sqlite_readonly(&first.raw_source_path)
            .map_err(|cause| map_capture_error(first, cause))?;
        configure_connection(&conn, first)?;
        validate_schema(&conn, first)?;

        let deadline = Instant::now() + SQLITE_RESOLVE_TIMEOUT;
        conn.progress_handler(
            SQLITE_PROGRESS_INSTRUCTIONS,
            Some(move || Instant::now() >= deadline),
        );
        let resolved = requests
            .iter()
            .map(|request| resolve_one(&conn, request))
            .collect::<Result<Vec<_>, _>>();
        conn.progress_handler(0, None::<fn() -> bool>);
        let messages = resolved?;
        if !before
            .revalidate(&first.raw_source_path)
            .map_err(|cause| map_capture_error(first, cause))?
        {
            return Err(error(first, CompleteContentErrorKind::SourceChanged));
        }
        Ok(messages)
    }
}

fn validate_request_batch(requests: &[CompleteMessageRequest]) -> Result<(), CompleteContentError> {
    let first = &requests[0];
    if requests.len() > MAX_SQLITE_COMPLETE_REQUESTS {
        return Err(error(first, CompleteContentErrorKind::ContentTooLarge));
    }
    let mut previous = None;
    for request in requests {
        if request.provider != first.provider
            || request.source_format != first.source_format
            || request.raw_source_path != first.raw_source_path
            || request.source_root != first.source_root
            || request.source_identity != first.source_identity
            || request.source_family != Some(CompleteContentSourceFamily::Sqlite)
            || request.source_snapshot != first.source_snapshot
        {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        let coordinate = (
            request.source_record_ordinal,
            request.source_record_subrecord_index,
        );
        if previous.is_some_and(|previous| previous >= coordinate) {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        if request.source_locator.is_none()
            || request.expected_native_record_id.is_none()
            || request.expected_record_digest.is_none()
            || request.expected_body_digest.is_none()
        {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
        previous = Some(coordinate);
    }
    Ok(())
}

fn verify_source_route(request: &CompleteMessageRequest) -> Result<(), CompleteContentError> {
    if request
        .source_identity
        .as_deref()
        .is_some_and(str::is_empty)
    {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    let Some(root) = request.source_root.as_ref() else {
        return Ok(());
    };
    let canonical_root = root
        .canonicalize()
        .map_err(|cause| map_io_error(request, cause))?;
    let canonical_source = request
        .raw_source_path
        .canonicalize()
        .map_err(|cause| map_io_error(request, cause))?;
    if !canonical_source.starts_with(&canonical_root) {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    Ok(())
}

fn read_source_snapshot(
    request: &CompleteMessageRequest,
) -> Result<ProviderSqliteSourceSnapshot, CompleteContentError> {
    ensure_regular_provider_transcript_file(&request.raw_source_path)
        .map_err(|cause| map_capture_error(request, cause))?;
    let metadata =
        fs::metadata(&request.raw_source_path).map_err(|cause| map_io_error(request, cause))?;
    if source_permissions_deny_all_reads(&metadata) {
        return Err(error(request, CompleteContentErrorKind::SourceUnreadable));
    }
    if request
        .source_snapshot
        .size_bytes
        .is_some_and(|size| metadata.len() < size)
    {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    if let (Some(expected), Ok(modified)) =
        (request.source_snapshot.modified_at_ms, metadata.modified())
    {
        let actual = modified
            .duration_since(UNIX_EPOCH)
            .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(i64::MIN);
        if request.source_snapshot.size_bytes == Some(metadata.len()) && actual != expected {
            return Err(error(request, CompleteContentErrorKind::SourceChanged));
        }
    }
    if let Some(expected) = request.source_snapshot.sha256.as_deref() {
        if CompleteContentBodyDigest::parse(expected).is_none() {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        if request.source_snapshot.size_bytes == Some(metadata.len()) {
            if metadata.len() > SQLITE_MAX_SNAPSHOT_HASH_BYTES {
                return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
            }
            let actual = hash_source_file(request, metadata.len())?;
            if actual != expected {
                return Err(error(request, CompleteContentErrorKind::SourceChanged));
            }
        }
    }
    ProviderSqliteSourceSnapshot::read(
        &request.raw_source_path,
        "complete-content SQLite source must be a regular non-symlink file",
        "complete-content SQLite sidecars must be regular non-symlink files",
    )
    .map_err(|cause| map_capture_error(request, cause))
}

fn hash_source_file(
    request: &CompleteMessageRequest,
    expected_len: u64,
) -> Result<String, CompleteContentError> {
    let mut file =
        File::open(&request.raw_source_path).map_err(|cause| map_io_error(request, cause))?;
    let mut digest = Sha256::new();
    let mut remaining = expected_len;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| error(request, CompleteContentErrorKind::ContentTooLarge))?;
        let read = file
            .read(&mut buffer[..limit])
            .map_err(|cause| map_io_error(request, cause))?;
        if read == 0 {
            return Err(error(request, CompleteContentErrorKind::SourceChanged));
        }
        digest.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(unix)]
fn source_permissions_deny_all_reads(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o444 == 0
}

#[cfg(not(unix))]
fn source_permissions_deny_all_reads(_metadata: &fs::Metadata) -> bool {
    false
}

fn configure_connection(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    let value_limit = i32::try_from(COMPLETE_CONTENT_MAX_BODY_BYTES)
        .map_err(|_| error(request, CompleteContentErrorKind::ContentTooLarge))?;
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, value_limit);
    conn.set_limit(
        SqliteLimit::SQLITE_LIMIT_COLUMN,
        SQLITE_MAX_ROW_VALUES as i32,
    );
    conn.busy_timeout(Duration::from_millis(250))
        .map_err(|cause| map_sqlite_error(request, cause))?;
    conn.pragma_update(None, "query_only", true)
        .map_err(|cause| map_sqlite_error(request, cause))?;
    let schema_objects: i64 = conn
        .query_row("select count(*) from sqlite_schema", [], |row| row.get(0))
        .map_err(|cause| map_sqlite_error(request, cause))?;
    if schema_objects < 0 || schema_objects as usize > SQLITE_MAX_SCHEMA_OBJECTS {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|cause| map_sqlite_error(request, cause))?;
    if user_version < 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(())
}

fn validate_schema(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    let required = match request.provider {
        CaptureProvider::Firebender => (
            "chat_sessions",
            &[
                "id",
                "name",
                "created_at",
                "updated_at",
                "messages_json",
                "metadata_json",
            ][..],
        ),
        CaptureProvider::KiroCli => return validate_kiro_schema(conn, request),
        CaptureProvider::Zed => (
            "threads",
            &["id", "summary", "updated_at", "data_type", "data"][..],
        ),
        _ => {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
    };
    if !sqlite_table_exists(conn, required.0).map_err(|cause| map_capture_error(request, cause))? {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let columns = sqlite_table_columns(conn, required.0)
        .map_err(|cause| map_capture_error(request, cause))?;
    ensure_sqlite_table_columns(&columns, required.0, required.1)
        .map_err(|cause| map_capture_error(request, cause))
}

fn validate_kiro_schema(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    let has_v2 = sqlite_table_exists(conn, "conversations_v2")
        .map_err(|cause| map_capture_error(request, cause))?;
    let has_legacy = sqlite_table_exists(conn, "conversations")
        .map_err(|cause| map_capture_error(request, cause))?;
    if !has_v2 && !has_legacy {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    if has_v2 {
        let columns = sqlite_table_columns(conn, "conversations_v2")
            .map_err(|cause| map_capture_error(request, cause))?;
        ensure_sqlite_table_columns(
            &columns,
            "conversations_v2",
            &[
                "key",
                "conversation_id",
                "value",
                "created_at",
                "updated_at",
            ],
        )
        .map_err(|cause| map_capture_error(request, cause))?;
    }
    if has_legacy {
        let columns = sqlite_table_columns(conn, "conversations")
            .map_err(|cause| map_capture_error(request, cause))?;
        ensure_sqlite_table_columns(&columns, "conversations", &["key", "value"])
            .map_err(|cause| map_capture_error(request, cause))?;
    }
    Ok(())
}

fn resolve_one(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<CompleteMessage, CompleteContentError> {
    let resolved = match request.provider {
        CaptureProvider::Firebender => resolve_firebender(conn, request),
        CaptureProvider::KiroCli => resolve_kiro(conn, request),
        CaptureProvider::Zed => resolve_zed(conn, request),
        _ => Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        )),
    }?;
    verify_resolved(request, &resolved)?;
    CompleteMessage::verified(request, resolved.text, SourceVerification::VERIFIED)
}

struct ResolvedSqliteMessage {
    text: String,
    event: ProviderEventEnvelope,
    native_record_id: String,
    record_digest: CompleteContentBodyDigest,
}

fn resolve_firebender(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let rowid = decode_raw_rowid(request, FIREBENDER_LOCATOR_KIND)?;
    let has_deleted_at = sqlite_table_columns(conn, "chat_sessions")
        .map_err(|cause| map_capture_error(request, cause))?
        .contains("deleted_at");
    let deleted_filter = if has_deleted_at {
        " and deleted_at is null"
    } else {
        ""
    };
    let sql = format!(
        "select id, name, cast(created_at as integer), cast(updated_at as integer), \
                messages_json, metadata_json from chat_sessions where rowid = ?1{deleted_filter}"
    );
    let values = conn
        .query_row(&sql, [rowid], |row| {
            Ok(vec![
                CapturedSqliteValue::Text(row.get(0)?),
                CapturedSqliteValue::Text(row.get(1)?),
                CapturedSqliteValue::Integer(row.get(2)?),
                CapturedSqliteValue::Integer(row.get(3)?),
                CapturedSqliteValue::Text(row.get(4)?),
                CapturedSqliteValue::Text(row.get(5)?),
            ])
        })
        .optional()
        .map_err(|cause| map_sqlite_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let [CapturedSqliteValue::Text(session_id), _, CapturedSqliteValue::Integer(created_at), _, CapturedSqliteValue::Text(messages_json), _] =
        values.as_slice()
    else {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    };
    if request.provider_session_id.as_deref() != Some(session_id) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let messages = serde_json::from_str::<Value>(messages_json)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    let index = request.source_record_subrecord_index as usize;
    let message = messages
        .get(index)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let fallback =
        DateTime::<Utc>::from_timestamp_millis(*created_at).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let occurred_at = firebender::firebender_message_time(message, fallback);
    let provider_event_index = u64::try_from(index)
        .map_err(|_| error(request, CompleteContentErrorKind::ContentTooLarge))?;
    let event =
        firebender::firebender_event(session_id, provider_event_index, message, occurred_at);
    if event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    let text = firebender::firebender_message_text(message).unwrap_or_else(|| {
        format!(
            "Firebender {}",
            message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("message")
        )
    });
    Ok(resolved_from_values(event, text, &values))
}

fn resolve_kiro(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let (table, rowid) = decode_kiro_rowid(request)?;
    let values = if table == "conversations_v2" {
        conn.query_row(
            "select rowid, cast(key as text), cast(conversation_id as text), cast(value as text), \
                    cast(created_at as integer), cast(updated_at as integer) \
             from conversations_v2 where rowid = ?1",
            [rowid],
            |row| {
                Ok(vec![
                    CapturedSqliteValue::Integer(row.get(0)?),
                    CapturedSqliteValue::Text(row.get(1)?),
                    CapturedSqliteValue::Text(row.get(2)?),
                    CapturedSqliteValue::Text(row.get(3)?),
                    optional_integer(row.get(4)?),
                    optional_integer(row.get(5)?),
                ])
            },
        )
    } else {
        conn.query_row(
            "select rowid, cast(key as text), cast(value as text) \
             from conversations where rowid = ?1",
            [rowid],
            |row| {
                Ok(vec![
                    CapturedSqliteValue::Integer(row.get(0)?),
                    CapturedSqliteValue::Text(row.get(1)?),
                    CapturedSqliteValue::Text(row.get(2)?),
                ])
            },
        )
    }
    .optional()
    .map_err(|cause| map_sqlite_error(request, cause))?
    .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let row = kiro::decode_kiro_conversation_for_complete(table, &values)
        .map_err(|cause| map_capture_error(request, cause))?;
    let value: Value = serde_json::from_str(&row.value)
        .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    let provider_session_id = kiro::kiro_provider_session_id(&row, &value);
    if request.provider_session_id.as_deref() != Some(provider_session_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let started_at = kiro::kiro_session_started_at(&row, &value, DateTime::<Utc>::UNIX_EPOCH);
    let target_index = usize::try_from(request.source_record_subrecord_index)
        .map_err(|_| error(request, CompleteContentErrorKind::ContentTooLarge))?;
    let decoded = kiro::kiro_history_events(&row, &provider_session_id, &value, started_at)
        .nth(target_index)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let text = decoded.complete_text();
    let event = decoded.event;
    if event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    Ok(resolved_from_values(event, text, &values))
}

fn resolve_zed(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let rowid = decode_raw_rowid(request, ZED_LOCATOR_KIND)?;
    let columns =
        sqlite_table_columns(conn, "threads").map_err(|cause| map_capture_error(request, cause))?;
    let parent_id = optional_column(&columns, "parent_id");
    let folder_paths = optional_column(&columns, "folder_paths");
    let folder_paths_order = optional_column(&columns, "folder_paths_order");
    let created_at = optional_column(&columns, "created_at");
    let sql = format!(
        "select rowid, cast(id as text), cast({parent_id} as text), \
                cast({folder_paths} as text), cast({folder_paths_order} as text), \
                cast(summary as text), cast(updated_at as text), cast(data_type as text), data, \
                cast({created_at} as text) from threads where rowid = ?1"
    );
    let values = conn
        .query_row(&sql, [rowid], |row| {
            Ok(vec![
                CapturedSqliteValue::Integer(row.get(0)?),
                CapturedSqliteValue::Text(row.get(1)?),
                optional_text(row.get(2)?),
                optional_text(row.get(3)?),
                optional_text(row.get(4)?),
                CapturedSqliteValue::Text(row.get(5)?),
                CapturedSqliteValue::Text(row.get(6)?),
                CapturedSqliteValue::Text(row.get(7)?),
                CapturedSqliteValue::Blob(row.get(8)?),
                optional_text(row.get(9)?),
            ])
        })
        .optional()
        .map_err(|cause| map_sqlite_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let row = zed::decode_zed_thread_for_complete(&values)
        .map_err(|cause| map_capture_error(request, cause))?;
    if request.provider_session_id.as_deref() != Some(row.id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let decoded =
        zed::decode_zed_thread_events(&row).map_err(|cause| map_capture_error(request, cause))?;
    let event_index = usize::try_from(request.source_record_subrecord_index)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let decoded_event = decoded
        .event_at(&row.id, event_index)
        .map_err(|cause| map_capture_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if decoded_event.event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    Ok(resolved_from_values(
        decoded_event.event,
        decoded_event.complete_text,
        &values,
    ))
}

fn verify_resolved(
    request: &CompleteMessageRequest,
    resolved: &ResolvedSqliteMessage,
) -> Result<(), CompleteContentError> {
    if request.expected_native_record_id.as_deref() != Some(&resolved.native_record_id)
        || request.expected_record_digest.as_ref() != Some(&resolved.record_digest)
    {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let actual_event_hash = match request.expected_hash_authority {
        CompleteContentHashAuthority::ProviderSupplied => {
            resolved.event.provider_event_hash.clone()
        }
        CompleteContentHashAuthority::NormalizedPayloadFallback => {
            compute_payload_hash(&resolved.event.payload).ok()
        }
    };
    if actual_event_hash.as_deref() != Some(request.expected_provider_event_hash.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(())
}

fn resolved_from_values(
    event: ProviderEventEnvelope,
    text: String,
    values: &[CapturedSqliteValue],
) -> ResolvedSqliteMessage {
    let native_record_id = native_record_id(&event);
    ResolvedSqliteMessage {
        text,
        event,
        native_record_id,
        record_digest: sqlite_logical_record_digest(values),
    }
}

fn native_record_id(event: &ProviderEventEnvelope) -> String {
    event
        .provider_event_hash
        .clone()
        .or_else(|| event.cursor.clone())
        .unwrap_or_else(|| format!("event-index:{}", event.provider_event_index))
}

fn decode_raw_rowid(
    request: &CompleteMessageRequest,
    expected_kind: &str,
) -> Result<i64, CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if locator.kind() != expected_kind || locator.value().len() != 8 {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let bytes: [u8; 8] = locator
        .value()
        .try_into()
        .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    Ok(i64::from_be_bytes(bytes))
}

fn decode_kiro_rowid(
    request: &CompleteMessageRequest,
) -> Result<(&'static str, i64), CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if locator.kind() != KIRO_LOCATOR_KIND || locator.value().len() != 9 {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let table = match locator.value()[0] {
        1 => "conversations_v2",
        2 => "conversations",
        _ => {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
    };
    let encoded = u64::from_be_bytes(
        locator.value()[1..]
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    Ok((table, (encoded ^ (1_u64 << 63)) as i64))
}

fn optional_integer(value: Option<i64>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer)
}

fn optional_text(value: Option<String>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Text)
}

fn optional_column<'a>(columns: &std::collections::BTreeSet<String>, name: &'a str) -> &'a str {
    if columns.contains(name) {
        name
    } else {
        "NULL"
    }
}

/// Adds the bounded local-only locator only when the canonical message text was
/// actually truncated. Provider projectors call this while the exact logical
/// SQLite row and complete text are still available.
pub(crate) fn attach_sqlite_complete_content_locator(
    event: &mut ProviderEventEnvelope,
    locator: &NativeLocator,
    values: &[CapturedSqliteValue],
    complete_text: impl FnOnce() -> String,
) -> crate::Result<()> {
    if event.event_type != EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let complete_text = complete_text();
    let record_digest = sqlite_logical_record_digest(values);
    let body_digest = CompleteContentBodyDigest::from_text(&complete_text);
    let persisted = PersistedCompleteContentLocatorV1::new(
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        native_record_id(event),
        record_digest,
        body_digest,
    )
    .ok_or(CaptureError::SystemInvariant(
        "SQLite complete-content locator exceeds the bounded canonical schema",
    ))?;
    let metadata = event
        .metadata
        .as_object_mut()
        .ok_or(CaptureError::SystemInvariant(
            "provider event metadata must be a JSON object",
        ))?;
    metadata.insert(
        COMPLETE_CONTENT_LOCATOR_METADATA_KEY.to_owned(),
        persisted.to_metadata_value(),
    );
    Ok(())
}

fn sqlite_logical_record_digest(values: &[CapturedSqliteValue]) -> CompleteContentBodyDigest {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            CapturedSqliteValue::Null => digest.update([0]),
            CapturedSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            CapturedSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            CapturedSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            CapturedSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
        .expect("SHA-256 formatter must return a valid digest")
}

fn error(request: &CompleteMessageRequest, kind: CompleteContentErrorKind) -> CompleteContentError {
    CompleteContentError::new(kind, request.event_id)
}

fn map_capture_error(
    request: &CompleteMessageRequest,
    cause: CaptureError,
) -> CompleteContentError {
    match cause {
        CaptureError::Io(cause) => map_io_error(request, cause),
        CaptureError::SourceChangedDuringCapture => {
            error(request, CompleteContentErrorKind::SourceChanged)
        }
        CaptureError::InvalidProviderTranscriptPath { .. } => {
            error(request, CompleteContentErrorKind::SourceUnreadable)
        }
        CaptureError::Sqlite(cause) => map_sqlite_error(request, cause),
        _ => error(request, CompleteContentErrorKind::ContentVerificationFailed),
    }
}

fn map_io_error(request: &CompleteMessageRequest, cause: std::io::Error) -> CompleteContentError {
    if cause.kind() == std::io::ErrorKind::NotFound {
        error(request, CompleteContentErrorKind::SourceMissing)
    } else {
        error(request, CompleteContentErrorKind::SourceUnreadable)
    }
}

fn map_sqlite_error(
    request: &CompleteMessageRequest,
    cause: rusqlite::Error,
) -> CompleteContentError {
    if let rusqlite::Error::SqliteFailure(failure, _) = &cause {
        return match failure.code {
            rusqlite::ErrorCode::TooBig | rusqlite::ErrorCode::OperationInterrupted => {
                error(request, CompleteContentErrorKind::ContentTooLarge)
            }
            rusqlite::ErrorCode::TypeMismatch | rusqlite::ErrorCode::SchemaChanged => {
                error(request, CompleteContentErrorKind::ContentVerificationFailed)
            }
            _ => error(request, CompleteContentErrorKind::SourceUnreadable),
        };
    }
    if matches!(
        cause,
        rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::Utf8Error(..)
            | rusqlite::Error::InvalidColumnType(..)
    ) {
        return error(request, CompleteContentErrorKind::ContentVerificationFailed);
    }
    error(request, CompleteContentErrorKind::SourceUnreadable)
}

#[cfg(test)]
#[path = "sqlite_tests.rs"]
mod tests;

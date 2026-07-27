use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, ContentRef, EventRole, EventType, Fidelity, ProviderEventEnvelope,
};
use ctx_history_store::Store;
use rmpv::{encode::write_value as write_msgpack_value, Value as MsgpackValue};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::*;
use crate::{
    complete_content::{
        AuthorizedSourceRoute, BrokeredSourceAccess, CompleteContentHashAuthority,
        CompleteContentSourceLocator, ResultContentRequest, ResultContentResolver,
        SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorsV1, VerifiedContentRouteStatus,
        COMPLETE_CONTENT_MAX_BODY_BYTES, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
        VERIFIED_CONTENT_ROUTES,
    },
    provider::providers::{
        astrbot, crush, deepagents, firebender, forgecode, goose, hermes, kiro, lingma, opencode,
        trae, trae::TRAE_STATE_VSCDB_SOURCE_FORMAT, zed,
    },
    NormalizedProviderImportOptions, ProviderAdapterContext, ASTRBOT_SQLITE_SOURCE_FORMAT,
    CRUSH_SQLITE_SOURCE_FORMAT, FORGECODE_SQLITE_SOURCE_FORMAT,
    GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT, HERMES_SQLITE_SOURCE_FORMAT, KILO_SQLITE_SOURCE_FORMAT,
    LINGMA_SQLITE_SOURCE_FORMAT, MIMOCODE_SQLITE_SOURCE_FORMAT, OPENCODE_SQLITE_SOURCE_FORMAT,
    PROVIDER_MAX_TEXT_CHARS, WARP_SQLITE_SOURCE_FORMAT,
};

const SESSION_ID: &str = "sqlite-complete-session";
const CREATED_AT: i64 = 1_783_653_514_000;

mod compound;
mod firebender_warp;
mod ordinary;
mod row_contained;
mod security;

fn long_body(label: &str) -> String {
    format!(
        "{label}\nUnicode: 🦀 café 東京\nEscaped: \"quoted\" \\ slash\n{}",
        "x".repeat(PROVIDER_MAX_TEXT_CHARS + 64)
    )
}

fn ordered_rowid(rowid: i64) -> [u8; 8] {
    ((rowid as u64) ^ (1_u64 << 63)).to_be_bytes()
}

fn sqlite_source_access(
    path: &Path,
    provider: CaptureProvider,
    source_format: &str,
    event_id: Uuid,
) -> BrokeredSourceAccess {
    SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider,
                source_format: source_format.to_owned(),
                family: CompleteContentSourceFamily::Sqlite,
                raw_source_path: path.to_path_buf(),
                source_root: path.parent().map(Path::to_path_buf),
                source_identity: Some("stable-result-source".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event_id,
        )
        .unwrap()
}

fn result_request_for(
    path: &Path,
    provider: CaptureProvider,
    source_format: &str,
    locator_kind: &str,
    locator_value: Vec<u8>,
    subrecord: u32,
    record: &SqliteResultRecord,
) -> ResultContentRequest {
    let event_id = Uuid::new_v4();
    ResultContentRequest {
        event_id,
        provider,
        source_format: source_format.to_owned(),
        source_access: sqlite_source_access(path, provider, source_format, event_id),
        source_family: CompleteContentSourceFamily::Sqlite,
        content_profile: verified_content_profile(
            provider,
            source_format,
            CompleteContentSourceFamily::Sqlite,
            VerifiedContentRole::ResultBody,
        )
        .unwrap()
        .to_owned(),
        source_locator: CompleteContentSourceLocator::new(locator_kind, locator_value).unwrap(),
        source_record_ordinal: 0,
        source_record_subrecord_index: subrecord,
        expected_native_record_id: record.native_record_id.clone(),
        expected_record_digest: sqlite_logical_record_digest(&record.values),
        expected_content_ref: ContentRef::from_bytes(record.content.as_bytes()).unwrap(),
    }
}

fn resolve_result(
    request: &ResultContentRequest,
) -> Result<ResolvedResultContent, CompleteContentError> {
    SqliteCompleteContentResolver::new()
        .resolve_results(std::slice::from_ref(request))
        .pop()
        .unwrap()
}

fn create_firebender_database(
    path: &Path,
    body: &str,
) -> (Vec<CapturedSqliteValue>, ProviderEventEnvelope) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "create table chat_sessions (
            id text not null,
            name text not null,
            created_at integer not null,
            updated_at integer not null,
            messages_json text not null,
            metadata_json text not null
        );",
    )
    .unwrap();
    let message = json!({
        "id": "native-message-1",
        "role": "user",
        "timestamp": CREATED_AT,
        "content": { "type": "text", "text": body },
    });
    let messages_json = serde_json::to_string(&json!([message.clone()])).unwrap();
    conn.execute(
        "insert into chat_sessions values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            SESSION_ID,
            "Complete content fixture",
            CREATED_AT,
            CREATED_AT + 1,
            messages_json,
            "{}",
        ],
    )
    .unwrap();
    drop(conn);

    let values = firebender_values(&messages_json);
    let event = firebender::firebender_event(
        SESSION_ID,
        0,
        &message,
        DateTime::<Utc>::from_timestamp_millis(CREATED_AT).unwrap(),
    );
    (values, event)
}

fn create_firebender_result_database(
    path: &Path,
    body: &str,
) -> (Vec<CapturedSqliteValue>, ProviderEventEnvelope) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "create table chat_sessions (
            id text not null,
            name text not null,
            created_at integer not null,
            updated_at integer not null,
            messages_json text not null,
            metadata_json text not null
        );",
    )
    .unwrap();
    let message = json!({
        "id": "tool-result-1",
        "role": "tool",
        "name": "display-label-must-not-be-result-content",
        "tool_call_id": "call-1",
        "timestamp": CREATED_AT,
        "content": { "type": "text", "text": body },
        "tool_calls": [{"name": "display-tool-call-must-not-be-result-content"}],
    });
    let messages_json = serde_json::to_string(&json!([message.clone()])).unwrap();
    conn.execute(
        "insert into chat_sessions values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            SESSION_ID,
            "Result content fixture",
            CREATED_AT,
            CREATED_AT + 1,
            messages_json,
            "{}",
        ],
    )
    .unwrap();
    drop(conn);

    let mut values = firebender_values(&messages_json);
    values[1] = CapturedSqliteValue::Text("Result content fixture".to_owned());
    let event = firebender::firebender_event(
        SESSION_ID,
        0,
        &message,
        DateTime::<Utc>::from_timestamp_millis(CREATED_AT).unwrap(),
    );
    (values, event)
}

fn firebender_values(messages_json: &str) -> Vec<CapturedSqliteValue> {
    vec![
        CapturedSqliteValue::Text(SESSION_ID.to_owned()),
        CapturedSqliteValue::Text("Complete content fixture".to_owned()),
        CapturedSqliteValue::Integer(CREATED_AT),
        CapturedSqliteValue::Integer(CREATED_AT + 1),
        CapturedSqliteValue::Text(messages_json.to_owned()),
        CapturedSqliteValue::Text("{}".to_owned()),
    ]
}

#[allow(clippy::too_many_arguments)]
fn request_for(
    path: &Path,
    provider: CaptureProvider,
    source_format: &str,
    provider_session_id: &str,
    subrecord: u32,
    locator_kind: &str,
    locator_value: Vec<u8>,
    values: &[CapturedSqliteValue],
    event: &ProviderEventEnvelope,
    body: &str,
) -> CompleteMessageRequest {
    let event_id = Uuid::new_v4();
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider,
                source_format: source_format.to_owned(),
                family: CompleteContentSourceFamily::Sqlite,
                raw_source_path: path.to_path_buf(),
                source_root: path.parent().map(Path::to_path_buf),
                source_identity: Some("stable-source-identity".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event_id,
        )
        .unwrap();
    CompleteMessageRequest {
        event_id,
        provider,
        source_format: source_format.to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Sqlite),
        content_profile: verified_content_profile(
            provider,
            source_format,
            CompleteContentSourceFamily::Sqlite,
            VerifiedContentRole::MessageBody,
        )
        .unwrap()
        .to_owned(),
        source_locator: CompleteContentSourceLocator::new(locator_kind, locator_value),
        provider_session_id: Some(provider_session_id.to_owned()),
        source_record_ordinal: 0,
        source_record_subrecord_index: subrecord,
        expected_provider_event_hash: event.provider_event_hash.clone().unwrap(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(native_record_id(event)),
        expected_record_digest: Some(sqlite_logical_record_digest(values)),
        expected_content_ref: ContentRef::from_bytes(body.as_bytes()),
        indexed_text: body.chars().take(PROVIDER_MAX_TEXT_CHARS).collect(),
        indexed_limit_chars: PROVIDER_MAX_TEXT_CHARS,
    }
}

fn readmit_sqlite(
    request: &mut CompleteMessageRequest,
    path: &Path,
    snapshot: SourceSnapshot,
) -> Result<(), CompleteContentError> {
    request.source_access = SourceAccessBroker::new().admit(
        AuthorizedSourceRoute {
            source_id: Uuid::new_v4(),
            provider: request.provider,
            source_format: request.source_format.clone(),
            family: CompleteContentSourceFamily::Sqlite,
            raw_source_path: path.to_path_buf(),
            source_root: path.parent().map(Path::to_path_buf),
            source_identity: Some("stable-source-identity".to_owned()),
            source_snapshot: snapshot,
        },
        request.event_id,
    )?;
    Ok(())
}

fn firebender_request(
    path: &Path,
    body: &str,
    values: &[CapturedSqliteValue],
    event: &ProviderEventEnvelope,
) -> CompleteMessageRequest {
    request_for(
        path,
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        SESSION_ID,
        0,
        FIREBENDER_LOCATOR_KIND,
        1_i64.to_be_bytes().to_vec(),
        values,
        event,
        body,
    )
}

fn assert_error_kind(request: &CompleteMessageRequest, expected: CompleteContentErrorKind) {
    let error = SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(request))
        .unwrap_err();
    assert_eq!(error.kind, expected);
    assert_eq!(error.event_id, request.event_id);
}

fn source_snapshot(path: &Path) -> SourceSnapshot {
    let bytes = fs::read(path).unwrap();
    let metadata = fs::metadata(path).unwrap();
    let modified_at_ms = metadata
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    SourceSnapshot {
        size_bytes: Some(metadata.len()),
        modified_at_ms: Some(modified_at_ms),
        sha256: Some(format!("{:x}", Sha256::digest(bytes))),
    }
}

fn sqlite_components(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut paths = vec![path.to_path_buf()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut component = path.as_os_str().to_os_string();
        component.push(suffix);
        paths.push(PathBuf::from(component));
    }
    paths
        .into_iter()
        .filter_map(|component| fs::read(&component).ok().map(|bytes| (component, bytes)))
        .collect()
}

fn create_event_without_database(body: &str) -> (Value, ProviderEventEnvelope) {
    let message = json!({
        "id": "native-short-message",
        "role": "user",
        "content": { "type": "text", "text": body },
    });
    let event = firebender::firebender_event(SESSION_ID, 0, &message, DateTime::<Utc>::UNIX_EPOCH);
    (message, event)
}

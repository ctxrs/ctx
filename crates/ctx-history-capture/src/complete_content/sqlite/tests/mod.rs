use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{compute_payload_hash, CaptureProvider, ContentRef, EventType};
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
        CompleteContentSourceLocator, SourceAccessBroker, SourceSnapshot,
        VerifiedContentLocatorsV1, VerifiedContentRouteStatus, COMPLETE_CONTENT_MAX_BODY_BYTES,
        VERIFIED_CONTENT_LOCATORS_METADATA_KEY, VERIFIED_CONTENT_ROUTES,
    },
    provider::providers::{
        astrbot, crush, deepagents, firebender, forgecode, goose, hermes, kiro, lingma, opencode,
        trae, trae::TRAE_STATE_VSCDB_SOURCE_FORMAT, zed,
    },
    ProviderAdapterContext, ProviderImportOptions, ASTRBOT_SQLITE_SOURCE_FORMAT,
    LINGMA_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

const SESSION_ID: &str = "sqlite-complete-session";
const CREATED_AT: i64 = 1_783_653_514_000;

#[derive(serde::Serialize)]
struct TestProviderEvent {
    provider_event_index: u64,
    provider_event_hash: Option<String>,
    cursor: Option<String>,
    event_type: EventType,
    payload: Value,
    metadata: Value,
}

trait TestProviderEventFields {
    fn provider_event_index(&self) -> u64;
    fn provider_event_hash(&self) -> Option<&str>;
    fn cursor(&self) -> Option<&str>;
    fn payload(&self) -> &Value;
}

impl TestProviderEventFields for TestProviderEvent {
    fn provider_event_index(&self) -> u64 {
        self.provider_event_index
    }

    fn provider_event_hash(&self) -> Option<&str> {
        self.provider_event_hash.as_deref()
    }

    fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    fn payload(&self) -> &Value {
        &self.payload
    }
}

impl TestProviderEventFields for kiro::KiroNativeEvent {
    fn provider_event_index(&self) -> u64 {
        self.provider_event_index
    }

    fn provider_event_hash(&self) -> Option<&str> {
        self.provider_event_hash.as_deref()
    }

    fn cursor(&self) -> Option<&str> {
        Some(self.cursor.as_str())
    }

    fn payload(&self) -> &Value {
        &self.payload
    }
}

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

fn test_provider_event(
    provider_event_index: u64,
    provider_event_hash: Option<String>,
    cursor: Option<String>,
    event_type: EventType,
    payload: Value,
    metadata: Value,
) -> TestProviderEvent {
    TestProviderEvent {
        provider_event_index,
        provider_event_hash,
        cursor,
        event_type,
        payload,
        metadata,
    }
}

fn firebender_event(
    provider_session_id: &str,
    provider_event_index: u64,
    message: &Value,
    occurred_at: DateTime<Utc>,
) -> TestProviderEvent {
    let event = firebender::firebender_native_event(
        provider_session_id,
        provider_event_index,
        message,
        occurred_at,
    );
    test_provider_event(
        event.provider_event_index,
        event.provider_event_hash,
        Some(event.cursor),
        event.event_type,
        event.payload,
        event.metadata,
    )
}

fn attach_test_sqlite_message_locator(
    event: &mut TestProviderEvent,
    provider: CaptureProvider,
    source_format: &str,
    locator: &NativeLocator,
    values: &[NativeSqliteValue],
    complete_text: impl FnOnce() -> String,
) -> crate::Result<()> {
    let native_record_id = native_record_id(
        event.provider_event_index,
        event.provider_event_hash.as_deref(),
        event.cursor.as_deref(),
    );
    attach_sqlite_complete_content_locator(
        provider,
        source_format,
        &native_record_id,
        &event.payload,
        &mut event.metadata,
        locator,
        values,
        complete_text,
    )
}

fn attach_sqlite_native_content_locator(
    event: &mut TestProviderEvent,
    provider: CaptureProvider,
    source_format: &str,
    locator: &NativeLocator,
    record_digest: &CompleteContentBodyDigest,
    complete_text: &str,
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
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported SQLite message route must have a verified-content profile",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        native_record_id(
            event.provider_event_index,
            event.provider_event_hash.as_deref(),
            event.cursor.as_deref(),
        ),
        record_digest.clone(),
    )
    .ok_or(CaptureError::SystemInvariant(
        "SQLite complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

fn create_firebender_database(
    path: &Path,
    body: &str,
) -> (Vec<NativeSqliteValue>, TestProviderEvent) {
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
    let event = firebender_event(
        SESSION_ID,
        0,
        &message,
        DateTime::<Utc>::from_timestamp_millis(CREATED_AT).unwrap(),
    );
    (values, event)
}

fn firebender_values(messages_json: &str) -> Vec<NativeSqliteValue> {
    vec![
        NativeSqliteValue::Text(SESSION_ID.to_owned()),
        NativeSqliteValue::Text("Complete content fixture".to_owned()),
        NativeSqliteValue::Integer(CREATED_AT),
        NativeSqliteValue::Integer(CREATED_AT + 1),
        NativeSqliteValue::Text(messages_json.to_owned()),
        NativeSqliteValue::Text("{}".to_owned()),
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
    values: &[NativeSqliteValue],
    event: &impl TestProviderEventFields,
    body: &str,
) -> CompleteMessageRequest {
    let event_id = Uuid::new_v4();
    let (expected_provider_event_hash, expected_hash_authority) =
        if let Some(provider_event_hash) = event.provider_event_hash() {
            (
                provider_event_hash.to_owned(),
                CompleteContentHashAuthority::ProviderSupplied,
            )
        } else {
            (
                compute_payload_hash(event.payload()).unwrap(),
                CompleteContentHashAuthority::NormalizedPayloadFallback,
            )
        };
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
        expected_provider_event_hash,
        expected_hash_authority,
        expected_native_record_id: Some(native_record_id(
            event.provider_event_index(),
            event.provider_event_hash(),
            event.cursor(),
        )),
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
    values: &[NativeSqliteValue],
    event: &TestProviderEvent,
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

fn create_event_without_database(body: &str) -> (Value, TestProviderEvent) {
    let message = json!({
        "id": "native-short-message",
        "role": "user",
        "content": { "type": "text", "text": body },
    });
    let event = firebender_event(SESSION_ID, 0, &message, DateTime::<Utc>::UNIX_EPOCH);
    (message, event)
}

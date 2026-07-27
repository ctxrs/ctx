use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType, ProviderEventEnvelope};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::*;
use crate::{
    complete_content::{CompleteContentSourceLocator, SourceSnapshot},
    provider::providers::{firebender, kiro, trae::TRAE_STATE_VSCDB_SOURCE_FORMAT, zed},
    PROVIDER_MAX_TEXT_CHARS,
};

const SESSION_ID: &str = "sqlite-complete-session";
const CREATED_AT: i64 = 1_783_653_514_000;

fn long_body(label: &str) -> String {
    format!(
        "{label}\nUnicode: 🦀 café 東京\nEscaped: \"quoted\" \\ slash\n{}",
        "x".repeat(PROVIDER_MAX_TEXT_CHARS + 64)
    )
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
    CompleteMessageRequest {
        event_id: Uuid::new_v4(),
        provider,
        source_format: source_format.to_owned(),
        raw_source_path: path.to_path_buf(),
        source_root: path.parent().map(Path::to_path_buf),
        source_identity: Some("stable-source-identity".to_owned()),
        source_family: Some(CompleteContentSourceFamily::Sqlite),
        source_locator: CompleteContentSourceLocator::new(locator_kind, locator_value),
        source_snapshot: SourceSnapshot::default(),
        provider_session_id: Some(provider_session_id.to_owned()),
        source_record_ordinal: 0,
        source_record_subrecord_index: subrecord,
        expected_provider_event_hash: event.provider_event_hash.clone().unwrap(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(native_record_id(event)),
        expected_record_digest: Some(sqlite_logical_record_digest(values)),
        expected_body_digest: Some(CompleteContentBodyDigest::from_text(body)),
        indexed_text: body.chars().take(PROVIDER_MAX_TEXT_CHARS).collect(),
        indexed_limit_chars: PROVIDER_MAX_TEXT_CHARS,
    }
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

#[test]
fn capabilities_account_for_every_sqlite_cohort_without_silent_fallback() {
    let capabilities = sqlite_complete_content_capabilities();
    assert_eq!(capabilities.iter().filter(|item| item.supported).count(), 3);
    assert!(capabilities.iter().all(|item| {
        item.supported == item.unsupported_reason.is_none()
            && !item.source_format.is_empty()
            && !item.cohort.is_empty()
    }));
    let mut keys = capabilities
        .iter()
        .map(|item| (item.provider.as_str(), item.source_format))
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), capabilities.len());

    let trae = capabilities
        .iter()
        .find(|item| item.provider == CaptureProvider::Trae)
        .unwrap();
    assert_eq!(trae.source_format, TRAE_STATE_VSCDB_SOURCE_FORMAT);
    assert!(!trae.supported);
    assert!(trae
        .unsupported_reason
        .is_some_and(|reason| reason.contains("whole ItemTable chat-value rows")));
}

#[test]
fn firebender_recovers_unicode_escaped_multiline_bytes_and_retains_only_truncated_locator() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("chat_history.db");
    let body = long_body("Firebender exact body");
    let (values, mut event) = create_firebender_database(&path, &body);
    assert_eq!(event.payload["text_retention"]["truncated"], true);

    let locator =
        NativeLocator::new(FIREBENDER_LOCATOR_KIND, 1_i64.to_be_bytes().to_vec()).unwrap();
    attach_sqlite_complete_content_locator(&mut event, &locator, &values, || body.clone()).unwrap();
    let persisted = PersistedCompleteContentLocatorV1::from_metadata_value(
        &event.metadata[COMPLETE_CONTENT_LOCATOR_METADATA_KEY],
    )
    .unwrap();
    assert_eq!(persisted.family(), CompleteContentSourceFamily::Sqlite);
    assert_eq!(persisted.kind(), FIREBENDER_LOCATOR_KIND);
    assert_eq!(persisted.native_record_id(), "native-message-1");
    assert_eq!(
        persisted.record_sha256(),
        &sqlite_logical_record_digest(&values)
    );
    assert_eq!(
        persisted.body_sha256(),
        &CompleteContentBodyDigest::from_text(&body)
    );

    let request = firebender_request(&path, &body, &values, &event);
    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text.as_bytes(), body.as_bytes());
    assert!(messages[0].verification.is_verified());

    let short = "ordinary short message";
    let (_, mut short_event) = create_event_without_database(short);
    attach_sqlite_complete_content_locator(&mut short_event, &locator, &values, || {
        short.to_owned()
    })
    .unwrap();
    assert!(short_event
        .metadata
        .get(COMPLETE_CONTENT_LOCATOR_METADATA_KEY)
        .is_none());
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

#[test]
fn source_move_under_current_root_and_append_only_growth_preserve_exact_row() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let original_root = temp.path().join("original");
    let moved_root = temp.path().join("moved");
    fs::create_dir(&original_root).unwrap();
    fs::create_dir(&moved_root).unwrap();
    let original = original_root.join("chat_history.db");
    let moved = moved_root.join("chat_history.db");
    let body = long_body("moved body");
    let (values, event) = create_firebender_database(&original, &body);
    let mut request = firebender_request(&original, &body, &values, &event);
    request.source_snapshot = source_snapshot(&original);

    fs::rename(&original, &moved).unwrap();
    request.raw_source_path = moved.clone();
    request.source_root = Some(moved_root);
    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request.clone()])
        .unwrap();
    assert_eq!(messages[0].text, body);

    let conn = Connection::open(&moved).unwrap();
    let other_messages = serde_json::to_string(&json!([{
        "id": "unrelated",
        "role": "user",
        "content": "append"
    }]))
    .unwrap();
    conn.execute(
        "insert into chat_sessions values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "other",
            "Other",
            CREATED_AT,
            CREATED_AT,
            other_messages,
            "{}"
        ],
    )
    .unwrap();
    drop(conn);
    assert!(fs::metadata(&moved).unwrap().len() >= request.source_snapshot.size_bytes.unwrap());
    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages[0].text, body);
}

#[test]
fn wal_snapshot_reads_committed_append_without_mutating_provider_components() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("wal.db");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "wal").unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn.execute_batch(
        "create table chat_sessions (
            id text not null, name text not null, created_at integer not null,
            updated_at integer not null, messages_json text not null,
            metadata_json text not null
        );",
    )
    .unwrap();
    let body = long_body("WAL body");
    let message = json!({
        "id": "native-message-1", "role": "user", "timestamp": CREATED_AT,
        "content": { "type": "text", "text": body }
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
            "{}"
        ],
    )
    .unwrap();
    conn.execute(
        "insert into chat_sessions values ('append', 'Append', 1, 1, '[]', '{}')",
        [],
    )
    .unwrap();
    let values = firebender_values(&messages_json);
    let event = firebender::firebender_event(
        SESSION_ID,
        0,
        &message,
        DateTime::<Utc>::from_timestamp_millis(CREATED_AT).unwrap(),
    );
    let request = firebender_request(&path, &body, &values, &event);
    let before = sqlite_components(&path);
    assert!(before
        .iter()
        .any(|(path, _)| path.to_string_lossy().ends_with("-wal")));

    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages[0].text, body);
    assert_eq!(sqlite_components(&path), before);
    drop(conn);
}

#[test]
fn rollback_journal_snapshot_never_recovers_into_provider_database() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("rollback.db");
    let body = long_body("rollback body");
    let (values, event) = create_firebender_database(&path, &body);
    let request = firebender_request(&path, &body, &values, &event);
    let writer = Connection::open(&path).unwrap();
    writer
        .pragma_update(None, "journal_mode", "delete")
        .unwrap();
    writer.execute_batch("begin immediate").unwrap();
    writer
        .execute(
            "update chat_sessions set name = 'uncommitted' where rowid = 1",
            [],
        )
        .unwrap();
    let before = sqlite_components(&path);
    assert!(before
        .iter()
        .any(|(path, _)| path.to_string_lossy().ends_with("-journal")));

    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages[0].text, body);
    assert_eq!(sqlite_components(&path), before);
    writer.execute_batch("rollback").unwrap();
}

#[test]
fn wrong_coordinates_and_digests_fail_without_plausible_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("wrong.db");
    let body = long_body("wrong identity body");
    let (values, event) = create_firebender_database(&path, &body);
    let request = firebender_request(&path, &body, &values, &event);

    let mut wrong_row = request.clone();
    wrong_row.source_locator =
        CompleteContentSourceLocator::new(FIREBENDER_LOCATOR_KIND, 99_i64.to_be_bytes().to_vec());
    assert_error_kind(&wrong_row, CompleteContentErrorKind::SourceRecordMissing);

    let mut wrong_kind = request.clone();
    wrong_kind.source_locator =
        CompleteContentSourceLocator::new("arbitrary-table-row-v1", 1_i64.to_be_bytes().to_vec());
    assert_error_kind(
        &wrong_kind,
        CompleteContentErrorKind::ContentVerificationFailed,
    );

    let mut wrong_native = request.clone();
    wrong_native.expected_native_record_id = Some("other-native-id".to_owned());
    assert_error_kind(
        &wrong_native,
        CompleteContentErrorKind::ContentVerificationFailed,
    );

    let mut wrong_record = request.clone();
    wrong_record.expected_record_digest = Some(CompleteContentBodyDigest::from_text("other row"));
    assert_error_kind(
        &wrong_record,
        CompleteContentErrorKind::ContentVerificationFailed,
    );

    let mut wrong_body = request.clone();
    wrong_body.expected_body_digest = Some(CompleteContentBodyDigest::from_text("other body"));
    assert_error_kind(
        &wrong_body,
        CompleteContentErrorKind::ContentVerificationFailed,
    );

    let mut wrong_subrecord = request.clone();
    wrong_subrecord.source_record_subrecord_index = 1;
    assert_error_kind(
        &wrong_subrecord,
        CompleteContentErrorKind::SourceRecordMissing,
    );

    let mut wrong_family = request;
    wrong_family.source_family = Some(CompleteContentSourceFamily::Jsonl);
    assert_error_kind(
        &wrong_family,
        CompleteContentErrorKind::ContentVerificationFailed,
    );
}

#[test]
fn mutation_replacement_deletion_and_permission_loss_are_typed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("mutable.db");
    let body = long_body("mutable body");
    let (values, event) = create_firebender_database(&path, &body);
    let request = firebender_request(&path, &body, &values, &event);

    let changed_body = body.replacen("mutable", "mutated", 1);
    let changed_message = json!({
        "id": "native-message-1", "role": "user", "timestamp": CREATED_AT,
        "content": { "type": "text", "text": changed_body }
    });
    let changed_json = serde_json::to_string(&json!([changed_message])).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update chat_sessions set messages_json = ?1 where rowid = 1",
        [changed_json],
    )
    .unwrap();
    drop(conn);
    assert_error_kind(
        &request,
        CompleteContentErrorKind::ContentVerificationFailed,
    );

    fs::remove_file(&path).unwrap();
    assert_error_kind(&request, CompleteContentErrorKind::SourceMissing);

    let (values, event) = create_firebender_database(&path, &body);
    let mut replacement_request = firebender_request(&path, &body, &values, &event);
    replacement_request.source_snapshot = source_snapshot(&path);
    let replacement = temp.path().join("replacement.db");
    create_firebender_database(&replacement, &body.replacen("mutable", "replaced", 1));
    fs::rename(&replacement, &path).unwrap();
    assert_error_kind(
        &replacement_request,
        CompleteContentErrorKind::SourceChanged,
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&path, permissions).unwrap();
        let mut permission_request = replacement_request;
        permission_request.source_snapshot = SourceSnapshot::default();
        assert_error_kind(
            &permission_request,
            CompleteContentErrorKind::SourceUnreadable,
        );
    }
}

#[test]
fn symlink_schema_and_request_bounds_are_enforced_before_hydration() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("real.db");
    let body = long_body("bounded body");
    let (values, event) = create_firebender_database(&path, &body);
    let request = firebender_request(&path, &body, &values, &event);

    let mut oversized_batch = Vec::new();
    for index in 0..=MAX_SQLITE_COMPLETE_REQUESTS {
        let mut item = request.clone();
        item.event_id = Uuid::new_v4();
        item.source_record_ordinal = index as u64;
        oversized_batch.push(item);
    }
    let error = SqliteCompleteContentResolver::new()
        .resolve(&oversized_batch)
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::ContentTooLarge);

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let link = temp.path().join("leaf-link.db");
        symlink(&path, &link).unwrap();
        let mut linked = request.clone();
        linked.raw_source_path = link;
        assert_error_kind(&linked, CompleteContentErrorKind::SourceUnreadable);

        let real_parent = temp.path().join("real-parent");
        fs::create_dir(&real_parent).unwrap();
        let parent_db = real_parent.join("nested.db");
        fs::copy(&path, &parent_db).unwrap();
        let linked_parent = temp.path().join("linked-parent");
        symlink(&real_parent, &linked_parent).unwrap();
        let mut parent_linked = request.clone();
        parent_linked.raw_source_path = linked_parent.join("nested.db");
        parent_linked.source_root = Some(temp.path().to_path_buf());
        assert_error_kind(&parent_linked, CompleteContentErrorKind::SourceUnreadable);
    }

    let invalid_schema = temp.path().join("invalid-schema.db");
    Connection::open(&invalid_schema)
        .unwrap()
        .execute("create table unrelated (value text)", [])
        .unwrap();
    let mut invalid = request;
    invalid.raw_source_path = invalid_schema;
    invalid.source_snapshot = SourceSnapshot::default();
    assert_error_kind(
        &invalid,
        CompleteContentErrorKind::ContentVerificationFailed,
    );
}

#[test]
fn kiro_and_zed_row_contained_cohorts_recover_exact_message_text() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let kiro_path = temp.path().join("kiro.db");
    let kiro_user_body = long_body("Kiro user body");
    let kiro_assistant_body = long_body("Kiro assistant body");
    let kiro_tool_fallback_body = long_body("Kiro tool fallback body");
    let kiro_value = json!({
        "history": [
            {"unrecognized": true},
            {
                "assistant": {
                    "ToolUse": {"tool_uses": [{"name": "shell"}]},
                    "timestamp": "2026-07-21T11:59:59Z"
                }
            },
            {
                "user": {
                    "timestamp": "2026-07-21T12:00:00Z",
                    "content": { "Prompt": { "prompt": kiro_user_body } }
                },
                "assistant": {
                    "timestamp": "2026-07-21T12:00:01Z",
                    "Response": {"content": kiro_assistant_body}
                }
            },
            {
                "user": {"content": {"Prompt": {"prompt": "   "}}},
                "assistant": {"ToolUse": {"content": kiro_tool_fallback_body}}
            }
        ]
    });
    let kiro_json = serde_json::to_string(&kiro_value).unwrap();
    let conn = Connection::open(&kiro_path).unwrap();
    conn.execute_batch(
        "create table conversations_v2 (
            key text not null, conversation_id text not null, value text not null,
            created_at integer, updated_at integer
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations_v2 values (?1, ?2, ?3, ?4, ?5)",
        params![
            "/workspace",
            "kiro-session",
            kiro_json,
            CREATED_AT,
            CREATED_AT + 1
        ],
    )
    .unwrap();
    drop(conn);
    let kiro_values = vec![
        CapturedSqliteValue::Integer(1),
        CapturedSqliteValue::Text("/workspace".to_owned()),
        CapturedSqliteValue::Text("kiro-session".to_owned()),
        CapturedSqliteValue::Text(kiro_json),
        CapturedSqliteValue::Integer(CREATED_AT),
        CapturedSqliteValue::Integer(CREATED_AT + 1),
    ];
    let kiro_row =
        kiro::decode_kiro_conversation_for_complete("conversations_v2", &kiro_values).unwrap();
    let started_at =
        kiro::kiro_session_started_at(&kiro_row, &kiro_value, DateTime::<Utc>::UNIX_EPOCH);
    let decoded = kiro::kiro_history_events(&kiro_row, "kiro-session", &kiro_value, started_at)
        .map(|decoded| {
            let text = decoded.complete_text();
            (decoded.event, text)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        decoded
            .iter()
            .map(|(event, _)| (event.provider_event_index, event.event_type))
            .collect::<Vec<_>>(),
        vec![
            (3, EventType::ToolCall),
            (4, EventType::Message),
            (5, EventType::Message),
            (7, EventType::Message),
        ]
    );
    let mut kiro_locator = vec![1_u8];
    kiro_locator.extend_from_slice(&(1_u64 ^ (1_u64 << 63)).to_be_bytes());
    let tool_call_request = request_for(
        &kiro_path,
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        "kiro-session",
        0,
        KIRO_LOCATOR_KIND,
        kiro_locator.clone(),
        &kiro_values,
        &decoded[0].0,
        &decoded[0].1,
    );
    assert_error_kind(
        &tool_call_request,
        CompleteContentErrorKind::HydrationUnsupported,
    );
    let kiro_requests = decoded
        .iter()
        .enumerate()
        .skip(1)
        .map(|(subrecord, (event, body))| {
            assert_eq!(event.payload["text_retention"]["truncated"], true);
            request_for(
                &kiro_path,
                CaptureProvider::KiroCli,
                KIRO_SQLITE_SOURCE_FORMAT,
                "kiro-session",
                u32::try_from(subrecord).unwrap(),
                KIRO_LOCATOR_KIND,
                kiro_locator.clone(),
                &kiro_values,
                event,
                body,
            )
        })
        .collect::<Vec<_>>();
    let result = SqliteCompleteContentResolver::new()
        .resolve(&kiro_requests)
        .unwrap();
    assert_eq!(
        result
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        vec![
            kiro_user_body.as_str(),
            kiro_assistant_body.as_str(),
            kiro_tool_fallback_body.as_str(),
        ]
    );

    let zed_path = temp.path().join("zed.db");
    let zed_body = long_body("Zed body");
    let zed_message = json!({ "User": { "content": [{ "Text": zed_body }] } });
    let zed_thread = json!({
        "messages": [zed_message.clone()],
        "updated_at": "2026-07-21T12:00:00Z"
    });
    let zed_data = serde_json::to_vec(&zed_thread).unwrap();
    let conn = Connection::open(&zed_path).unwrap();
    conn.execute_batch(
        "create table threads (
            id text not null, summary text not null, updated_at text not null,
            data_type text not null, data blob not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into threads values (?1, ?2, ?3, ?4, ?5)",
        params![
            "zed-session",
            "Zed fixture",
            "2026-07-21T12:00:00Z",
            "json",
            zed_data
        ],
    )
    .unwrap();
    drop(conn);
    let zed_values = vec![
        CapturedSqliteValue::Integer(1),
        CapturedSqliteValue::Text("zed-session".to_owned()),
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Text("Zed fixture".to_owned()),
        CapturedSqliteValue::Text("2026-07-21T12:00:00Z".to_owned()),
        CapturedSqliteValue::Text("json".to_owned()),
        CapturedSqliteValue::Blob(zed_data),
        CapturedSqliteValue::Null,
    ];
    let zed_row = zed::decode_zed_thread_for_complete(&zed_values).unwrap();
    let zed_decoded = zed::decode_zed_thread_events(&zed_row).unwrap();
    let zed_event = zed_decoded
        .event_at("zed-session", 0)
        .unwrap()
        .unwrap()
        .event;
    let zed_request = request_for(
        &zed_path,
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        "zed-session",
        0,
        ZED_LOCATOR_KIND,
        1_i64.to_be_bytes().to_vec(),
        &zed_values,
        &zed_event,
        &zed_body,
    );
    let result = SqliteCompleteContentResolver::new()
        .resolve(&[zed_request])
        .unwrap();
    assert_eq!(result[0].text, zed_body);
}

#[test]
fn malformed_and_truncated_kiro_records_fail_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("malformed-kiro.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations_v2 (
            key text not null, conversation_id text not null, value text not null,
            created_at integer, updated_at integer
        );",
    )
    .unwrap();
    let truncated_json = r#"{"history":["#;
    let malformed_history = r#"{"history":{"not":"an array"}}"#;
    conn.execute(
        "insert into conversations_v2 values (?1, ?2, ?3, ?4, ?5)",
        params![
            "/truncated",
            "kiro-truncated",
            truncated_json,
            CREATED_AT,
            CREATED_AT
        ],
    )
    .unwrap();
    conn.execute(
        "insert into conversations_v2 values (?1, ?2, ?3, ?4, ?5)",
        params![
            "/malformed-history",
            "kiro-malformed-history",
            malformed_history,
            CREATED_AT,
            CREATED_AT
        ],
    )
    .unwrap();
    drop(conn);

    for (rowid, key, session_id, stored_value, expected) in [
        (
            1,
            "/truncated",
            "kiro-truncated",
            truncated_json,
            CompleteContentErrorKind::ContentVerificationFailed,
        ),
        (
            2,
            "/malformed-history",
            "kiro-malformed-history",
            malformed_history,
            CompleteContentErrorKind::SourceRecordMissing,
        ),
    ] {
        let values = vec![
            CapturedSqliteValue::Integer(rowid),
            CapturedSqliteValue::Text(key.to_owned()),
            CapturedSqliteValue::Text(session_id.to_owned()),
            CapturedSqliteValue::Text(stored_value.to_owned()),
            CapturedSqliteValue::Integer(CREATED_AT),
            CapturedSqliteValue::Integer(CREATED_AT),
        ];
        let row = kiro::decode_kiro_conversation_for_complete("conversations_v2", &values).unwrap();
        let body = long_body("untrusted fallback must not be returned");
        let reference = json!({
            "history": [{"user": {"content": {"Prompt": {"prompt": body}}}}]
        });
        let decoded =
            kiro::kiro_history_events(&row, session_id, &reference, DateTime::<Utc>::UNIX_EPOCH)
                .next()
                .unwrap();
        let mut locator = vec![1_u8];
        locator.extend_from_slice(&((rowid as u64) ^ (1_u64 << 63)).to_be_bytes());
        let request = request_for(
            &path,
            CaptureProvider::KiroCli,
            KIRO_SQLITE_SOURCE_FORMAT,
            session_id,
            0,
            KIRO_LOCATOR_KIND,
            locator,
            &values,
            &decoded.event,
            &body,
        );
        assert_error_kind(&request, expected);
    }
}

#[test]
fn legacy_kiro_row_preserves_decoder_identity_locator_and_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("legacy-kiro.db");
    let body = long_body("Legacy Kiro body");
    let value = json!({
        "conversation_id": "kiro-legacy-session",
        "history": [{
            "user": {
                "timestamp": "2026-07-21T12:00:00Z",
                "content": {"Prompt": {"prompt": body}}
            }
        }]
    });
    let encoded = serde_json::to_string(&value).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("create table conversations (key text not null, value text not null);")
        .unwrap();
    conn.execute(
        "insert into conversations values (?1, ?2)",
        params!["/legacy", encoded],
    )
    .unwrap();
    drop(conn);
    let values = vec![
        CapturedSqliteValue::Integer(1),
        CapturedSqliteValue::Text("/legacy".to_owned()),
        CapturedSqliteValue::Text(encoded),
    ];
    let row = kiro::decode_kiro_conversation_for_complete("conversations", &values).unwrap();
    let provider_session_id = kiro::kiro_provider_session_id(&row, &value);
    let started_at = kiro::kiro_session_started_at(&row, &value, DateTime::<Utc>::UNIX_EPOCH);
    let decoded = kiro::kiro_history_events(&row, &provider_session_id, &value, started_at)
        .next()
        .unwrap();
    assert_eq!(
        decoded.event.provider_event_hash.as_deref(),
        Some("conversations:kiro-legacy-session:0:user")
    );
    assert_eq!(
        decoded.event.cursor.as_deref(),
        Some("conversations:kiro-legacy-session:history:0:user")
    );
    let mut locator = vec![2_u8];
    locator.extend_from_slice(&(1_u64 ^ (1_u64 << 63)).to_be_bytes());
    let request = request_for(
        &path,
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        &provider_session_id,
        0,
        KIRO_LOCATOR_KIND,
        locator,
        &values,
        &decoded.event,
        &body,
    );
    let message = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(message.text, body);
}

#[test]
fn oversized_kiro_record_fails_before_json_decode() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("oversized-kiro.db");
    let oversized_value = "x".repeat(COMPLETE_CONTENT_MAX_BODY_BYTES + 1);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations_v2 (
            key text not null, conversation_id text not null, value text not null,
            created_at integer, updated_at integer
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations_v2 values (?1, ?2, ?3, ?4, ?5)",
        params![
            "/oversized",
            "kiro-oversized",
            oversized_value,
            CREATED_AT,
            CREATED_AT
        ],
    )
    .unwrap();
    drop(conn);
    let values = vec![
        CapturedSqliteValue::Integer(1),
        CapturedSqliteValue::Text("/oversized".to_owned()),
        CapturedSqliteValue::Text("kiro-oversized".to_owned()),
        CapturedSqliteValue::Text(oversized_value),
        CapturedSqliteValue::Integer(CREATED_AT),
        CapturedSqliteValue::Integer(CREATED_AT),
    ];
    let row = kiro::decode_kiro_conversation_for_complete("conversations_v2", &values).unwrap();
    let body = long_body("oversized row fallback");
    let reference = json!({
        "history": [{"user": {"content": {"Prompt": {"prompt": body}}}}]
    });
    let decoded = kiro::kiro_history_events(
        &row,
        "kiro-oversized",
        &reference,
        DateTime::<Utc>::UNIX_EPOCH,
    )
    .next()
    .unwrap();
    let mut locator = vec![1_u8];
    locator.extend_from_slice(&(1_u64 ^ (1_u64 << 63)).to_be_bytes());
    let request = request_for(
        &path,
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        "kiro-oversized",
        0,
        KIRO_LOCATOR_KIND,
        locator,
        &values,
        &decoded.event,
        &body,
    );
    assert_error_kind(&request, CompleteContentErrorKind::ContentTooLarge);
}

#[test]
fn oversized_sqlite_record_returns_content_too_large() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("oversized.db");
    let body = "z".repeat(COMPLETE_CONTENT_MAX_BODY_BYTES + 1);
    let (values, event) = create_firebender_database(&path, &body);
    let request = firebender_request(&path, &body, &values, &event);
    assert_error_kind(&request, CompleteContentErrorKind::ContentTooLarge);
}

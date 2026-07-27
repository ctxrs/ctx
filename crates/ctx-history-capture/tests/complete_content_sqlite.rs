use ctx_history_capture::complete_content::sqlite::SqliteCompleteContentResolver;
use ctx_history_capture::complete_content::{
    CompleteContentBodyDigest, CompleteContentHashAuthority, CompleteContentResolver,
    CompleteContentSourceFamily, CompleteContentSourceLocator, CompleteMessageRequest,
    SourceSnapshot,
};
use ctx_history_core::CaptureProvider;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const INDEXED_LIMIT: usize = 16_000;

#[test]
fn native_sqlite_target_recovers_exact_firebender_message() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("chat_history.db");
    let body = format!(
        "Unicode 🦀 café 東京\nEscaped: \"quote\" \\ slash\n{}",
        "x".repeat(INDEXED_LIMIT + 32)
    );
    let message = serde_json::json!({
        "id": "native-message-1",
        "role": "user",
        "content": { "type": "text", "text": body },
    });
    let messages_json = serde_json::to_string(&serde_json::json!([message])).unwrap();
    let conn = Connection::open(&database).unwrap();
    conn.execute_batch(
        "create table chat_sessions (
            id text not null, name text not null, created_at integer not null,
            updated_at integer not null, messages_json text not null,
            metadata_json text not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into chat_sessions values (?1, ?2, ?3, ?4, ?5, ?6)",
        params!["session-1", "Fixture", 1_i64, 2_i64, messages_json, "{}"],
    )
    .unwrap();
    drop(conn);

    let record_digest = logical_row_digest(&[
        LogicalValue::Text("session-1"),
        LogicalValue::Text("Fixture"),
        LogicalValue::Integer(1),
        LogicalValue::Integer(2),
        LogicalValue::Text(&messages_json),
        LogicalValue::Text("{}"),
    ]);
    let request = CompleteMessageRequest {
        event_id: Uuid::new_v4(),
        provider: CaptureProvider::Firebender,
        source_format: "firebender_chat_history_sqlite".to_owned(),
        raw_source_path: database,
        source_root: Some(temp.path().to_path_buf()),
        source_identity: Some("stable-source".to_owned()),
        source_family: Some(CompleteContentSourceFamily::Sqlite),
        source_locator: CompleteContentSourceLocator::new(
            "firebender-chat-session-row-v1",
            1_i64.to_be_bytes().to_vec(),
        ),
        source_snapshot: SourceSnapshot::default(),
        provider_session_id: Some("session-1".to_owned()),
        source_record_ordinal: 0,
        source_record_subrecord_index: 0,
        expected_provider_event_hash: "native-message-1".to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some("native-message-1".to_owned()),
        expected_record_digest: Some(record_digest),
        expected_body_digest: Some(CompleteContentBodyDigest::from_text(&body)),
        indexed_text: body.chars().take(INDEXED_LIMIT).collect(),
        indexed_limit_chars: INDEXED_LIMIT,
    };

    let resolved = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(resolved[0].text.as_bytes(), body.as_bytes());
    assert!(resolved[0].verification.is_verified());
}

enum LogicalValue<'a> {
    Integer(i64),
    Text(&'a str),
}

fn logical_row_digest(values: &[LogicalValue<'_>]) -> CompleteContentBodyDigest {
    let mut digest = Sha256::new();
    digest.update(b"ctx-complete-content-sqlite-logical-row-v1\0");
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            LogicalValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            LogicalValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize())).unwrap()
}

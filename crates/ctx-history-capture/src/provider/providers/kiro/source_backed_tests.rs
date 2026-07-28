use std::fs;

use ctx_history_core::{LocatorRevisionPolicy, NativeRecordCoordinate, TypedKey};
use ctx_history_index::MAX_BODY_PREVIEW_CHARS;
use rusqlite::{params, Connection};
use serde_json::json;
use tempfile::TempDir;

use super::*;

fn create_database(path: &Path, rowid_offset: usize, user_text: &str, include_legacy: bool) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table conversations_v2 (
                key text not null,
                conversation_id text not null,
                value text not null,
                created_at integer,
                updated_at integer
            );
            create table conversations (
                key text not null,
                value text not null
            );",
        )
        .unwrap();
    for index in 0..rowid_offset {
        connection
            .execute(
                "insert into conversations_v2 values (?1, ?2, ?3, 1, 1)",
                params![
                    format!("/discarded-{index}"),
                    format!("discarded-{index}"),
                    json!({"history": []}).to_string(),
                ],
            )
            .unwrap();
    }
    let value = json!({
        "history": [{
            "user": {
                "content": {"Prompt": {"prompt": user_text}},
                "timestamp": "2026-07-28T12:00:01Z"
            },
            "assistant": {
                "Response": {"content": "assistant exact body"},
                "timestamp": "2026-07-28T12:00:02Z"
            }
        }]
    })
    .to_string();
    connection
        .execute(
            "insert into conversations_v2 values (
                '/workspace', 'kiro-session', ?1, 1785240000000, 1785240002000
            )",
            [&value],
        )
        .unwrap();
    if rowid_offset != 0 {
        connection
            .execute(
                "delete from conversations_v2 where key like '/discarded-%'",
                [],
            )
            .unwrap();
    }
    if include_legacy {
        connection
            .execute(
                "insert into conversations values (
                    '/legacy', '{\"conversation_id\":\"legacy\",\"history\":[]}'
                )",
                [],
            )
            .unwrap();
    }
}

fn replace_database(path: &Path, replacement: &Path) {
    fs::remove_file(path).unwrap();
    fs::rename(replacement, path).unwrap();
}

#[test]
fn cold_scan_bounds_projection_and_exactly_hydrates_the_conversation_row() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("data.sqlite3");
    let long_user_text = "kiro exact body ".repeat(MAX_BODY_PREVIEW_CHARS);
    create_database(&path, 0, &long_user_text, true);

    let scan = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_eq!(scan.documents.len(), 2);
    assert_eq!(scan.certificate.counts().complete_records, 3);
    assert_eq!(scan.certificate.counts().retained_records, 2);
    assert_eq!(scan.certificate.counts().ignored_records, 1);
    assert_eq!(scan.certificate.counts().indexed_documents, 2);
    assert!(scan.certificate.counts().certified_bytes > 0);

    let user = scan
        .documents
        .iter()
        .find(|document| document.role.as_deref() == Some("user"))
        .unwrap();
    assert_eq!(user.body.chars().count(), MAX_BODY_PREVIEW_CHARS);
    assert_eq!(user.parent_session_id, None);
    assert_eq!(user.root_session_id, user.session_id);
    assert_eq!(user.provider_session_id.as_deref(), Some("kiro-session"));
    assert_eq!(user.branch, None);
    let canonical_source_path = fs::canonicalize(&path).unwrap().display().to_string();
    assert_eq!(
        user.source_path.as_deref(),
        Some(canonical_source_path.as_str())
    );
    assert_eq!(user.agent_type, "primary");
    assert!(user.is_primary);
    assert_eq!(
        user.locator.revision_policy(),
        LocatorRevisionPolicy::StableRecordEvidence
    );
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = user.locator.coordinate()
    else {
        panic!("expected Kiro SQLite locator");
    };
    assert_eq!(logical_relation, "conversations_v2");
    assert!(row_version.is_none());
    assert!(matches!(
        primary_key,
        TypedKey::Composite(parts)
            if matches!(parts.as_slice(), [TypedKey::Utf8(key), TypedKey::Utf8(_)] if key == "/workspace")
    ));

    let resolver = KiroLocatorResolverV0::discover(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert!(resolver.source().exact_descriptor_eq(&scan.source));
    let hydrated = resolver.hydrate(&user.locator).unwrap();
    assert_eq!(hydrated.decoded_display_text, long_user_text);
    assert!(String::from_utf8(hydrated.provider_bytes)
        .unwrap()
        .contains("assistant exact body"));
}

#[test]
fn stable_ids_and_row_locators_survive_snapshot_replacement_but_not_row_replacement() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("data.sqlite3");
    create_database(&path, 0, "stable user body", false);
    let cold = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    let cold_ids = cold
        .documents
        .iter()
        .map(|document| (document.event_id, document.session_id))
        .collect::<Vec<_>>();
    let old_locator = cold.documents[0].locator.clone();
    let old_content_digest = *cold.certificate.content_digest();
    let resolver = KiroLocatorResolverV0::discover(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();

    let replacement = temp.path().join("replacement.sqlite3");
    create_database(&replacement, 3, "stable user body", false);
    replace_database(&path, &replacement);
    let replaced = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_eq!(
        replaced
            .documents
            .iter()
            .map(|document| (document.event_id, document.session_id))
            .collect::<Vec<_>>(),
        cold_ids
    );
    assert_eq!(*replaced.certificate.content_digest(), old_content_digest);
    assert_ne!(
        replaced.certificate.observation().revision(),
        cold.certificate.observation().revision()
    );
    assert_eq!(
        resolver.hydrate(&old_locator).unwrap().decoded_display_text,
        "stable user body"
    );

    let changed = temp.path().join("changed.sqlite3");
    create_database(&changed, 1, "changed user body", false);
    replace_database(&path, &changed);
    assert!(matches!(
        resolver.hydrate(&old_locator),
        Err(KiroSourceBackedErrorV0::ConversationRowDigestMismatch)
    ));
}

#[test]
fn acp_v3_saved_chat_and_non_kiro_sqlite_remain_unsupported() {
    let temp = TempDir::new().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    assert!(matches!(
        scan_kiro_source_backed_v0(&sessions, KIRO_SQLITE_SOURCE_FORMAT),
        Err(KiroSourceBackedErrorV0::UnsupportedFormat(_))
    ));

    let saved_chat = temp.path().join("saved-chat.json");
    fs::write(&saved_chat, b"{\"history\":[]}").unwrap();
    assert!(matches!(
        scan_kiro_source_backed_v0(&saved_chat, KIRO_SQLITE_SOURCE_FORMAT),
        Err(KiroSourceBackedErrorV0::UnsupportedFormat(_))
    ));

    let sqlite = temp.path().join("data.sqlite3");
    create_database(&sqlite, 0, "body", false);
    assert!(matches!(
        scan_kiro_source_backed_v0(&sqlite, "kiro_cli_acp_v3"),
        Err(KiroSourceBackedErrorV0::UnsupportedFormat(_))
    ));

    let unrelated = temp.path().join("unrelated.sqlite3");
    Connection::open(&unrelated)
        .unwrap()
        .execute_batch("create table saved_chats (value text);")
        .unwrap();
    assert!(matches!(
        scan_kiro_source_backed_v0(&unrelated, KIRO_SQLITE_SOURCE_FORMAT),
        Err(KiroSourceBackedErrorV0::Capture(
            CaptureError::UnsupportedSchema(_)
        ))
    ));
}

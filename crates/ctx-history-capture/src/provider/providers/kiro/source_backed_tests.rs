use std::fs;

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind, LocatorRevisionPolicy, NativeRecordCoordinate, TypedKey,
};
use ctx_history_index::WriterOptions;
use rusqlite::{config::DbConfig, params, Connection};
use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, PROVIDER_MAX_TEXT_CHARS,
};

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

fn insert_conversation(connection: &Connection, key: &str, session: &str, text: &str) {
    let value = json!({
        "history": [{
            "user": {
                "content": {"Prompt": {"prompt": text}},
                "timestamp": "2026-07-28T12:00:01Z"
            }
        }]
    })
    .to_string();
    connection
        .execute(
            "insert into conversations_v2 values (?1, ?2, ?3, 1, 1)",
            params![key, session, value],
        )
        .unwrap();
}

#[test]
fn cold_scan_keeps_full_policy_body_and_exactly_hydrates_the_conversation_row() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("data.sqlite3");
    let long_user_text = format!(
        "{}kiro-tail-term{}",
        "x".repeat(3_000),
        "y".repeat(PROVIDER_MAX_TEXT_CHARS)
    );
    create_database(&path, 0, &long_user_text, true);

    let scan = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_eq!(scan.documents.len(), 2);
    assert_eq!(scan.certificate.counts().complete_records, 3);
    assert_eq!(scan.certificate.counts().retained_records, 2);
    assert_eq!(scan.certificate.counts().ignored_records, 1);
    assert_eq!(scan.certificate.counts().indexed_documents, 2);
    assert!(scan.certificate.counts().certified_bytes > 0);
    assert!(scan.certificate.frontier().is_none());
    assert_eq!(scan.row_decode_passes, 1);
    assert_eq!(scan.decoded_rows, 2);
    assert_eq!(scan.emitted_pages, 1);
    assert_eq!(scan.peak_buffered_rows, 2);

    let user = scan
        .documents
        .iter()
        .find(|document| document.role.as_deref() == Some("user"))
        .unwrap();
    assert_eq!(user.body.chars().count(), PROVIDER_MAX_TEXT_CHARS);
    assert!(user.body.contains("kiro-tail-term"));
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
    assert_eq!(hydrated.provider_bytes, long_user_text.as_bytes());
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
    assert_eq!(replaced.certificate, cold.certificate);
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
fn checkpoint_wal_removal_and_vacuum_are_logical_noops_without_frontiers() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("data.sqlite3");
    create_database(&path, 0, "stable body", false);
    let cold = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();

    let writer = Connection::open(&path).unwrap();
    let mode: String = writer
        .query_row("pragma journal_mode=wal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer
        .execute(
            "update conversations_v2 set updated_at = updated_at where key = '/workspace'",
            [],
        )
        .unwrap();
    writer
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    drop(writer);
    assert!(path.with_file_name("data.sqlite3-wal").exists());

    let wal = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_eq!(wal.certificate, cold.certificate);
    assert!(wal.certificate.frontier().is_none());

    Connection::open(&path)
        .unwrap()
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    let checkpointed = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_eq!(checkpointed.certificate, cold.certificate);

    Connection::open(&path)
        .unwrap()
        .execute_batch("vacuum")
        .unwrap();
    let vacuumed = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_eq!(vacuumed.certificate, cold.certificate);
}

#[test]
fn row_schema_classification_and_deletion_changes_are_full_replacements() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("data.sqlite3");
    create_database(&path, 0, "baseline", true);
    let baseline = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();

    Connection::open(&path)
        .unwrap()
        .execute(
            "update conversations_v2 set value = '{\"history\":\"invalid\"}' \
             where key = '/workspace'",
            [],
        )
        .unwrap();
    let classification = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_ne!(classification.certificate, baseline.certificate);
    assert_eq!(classification.certificate.counts().rejected_records, 1);
    assert!(classification.certificate.frontier().is_none());

    Connection::open(&path)
        .unwrap()
        .execute_batch("pragma user_version=17")
        .unwrap();
    let schema = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_ne!(schema.certificate, classification.certificate);

    Connection::open(&path)
        .unwrap()
        .execute("delete from conversations_v2 where key = '/workspace'", [])
        .unwrap();
    let deleted = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_ne!(deleted.certificate, schema.certificate);
    assert_eq!(deleted.certificate.counts().indexed_documents, 0);

    fs::remove_file(&path).unwrap();
    assert!(matches!(
        scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT),
        Err(KiroSourceBackedErrorV0::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
fn one_decode_pass_streams_pages_with_a_hard_sixty_four_document_peak() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("data.sqlite3");
    create_database(&path, 0, "discard", false);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("delete from conversations_v2", [])
        .unwrap();
    for index in 0..80 {
        insert_conversation(
            &connection,
            &format!("/workspace-{index:03}"),
            &format!("session-{index:03}"),
            &format!("body-{index:03}"),
        );
    }
    drop(connection);

    let scan = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_eq!(scan.documents.len(), 80);
    assert_eq!(scan.row_decode_passes, 1);
    assert_eq!(scan.decoded_rows, 80);
    assert_eq!(scan.emitted_pages, 2);
    assert_eq!(scan.peak_buffered_rows, SOURCE_BACKED_PAGE_ROWS as u64);
}

#[test]
fn grouped_hydration_uses_one_snapshot_preserves_order_and_batches_native_keys() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("data.sqlite3");
    create_database(&path, 0, "discard", false);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("delete from conversations_v2", [])
        .unwrap();
    for index in 0..257 {
        insert_conversation(
            &connection,
            &format!("/workspace-{index:03}"),
            &format!("session-{index:03}"),
            &format!("body-{index:03}"),
        );
    }
    drop(connection);
    let scan = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    let events = scan
        .documents
        .iter()
        .rev()
        .map(|document| {
            EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
        })
        .collect();
    let request = BatchHydrationRequest::new(events).unwrap();
    let resolver = KiroLocatorResolverV0::discover(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    let hydrated = resolver.hydrate_batch(&request).unwrap();

    assert_eq!(resolver.hydration_counters(), (1, 2));
    for (record, document) in hydrated.records().iter().zip(scan.documents.iter().rev()) {
        assert_eq!(record.event_id, document.event_id);
        assert_eq!(record.provider_bytes, document.body.as_bytes());
    }
}

#[test]
fn exact_hydration_survives_unrelated_mutation_and_types_stale_exact_row() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("data.sqlite3");
    create_database(&path, 0, "first body", false);
    let connection = Connection::open(&path).unwrap();
    insert_conversation(&connection, "/other", "other-session", "other body");
    drop(connection);
    let scan = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    let document = scan
        .documents
        .iter()
        .find(|document| document.body == "first body")
        .unwrap();
    let request = EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap();
    let resolver = KiroLocatorResolverV0::discover(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();

    Connection::open(&path)
        .unwrap()
        .execute(
            "update conversations_v2 set updated_at = 99 where key = '/other'",
            [],
        )
        .unwrap();
    assert_eq!(
        resolver.hydrate_event(&request).unwrap().provider_bytes,
        b"first body"
    );

    Connection::open(&path)
        .unwrap()
        .execute(
            "update conversations_v2 set updated_at = 99 where key = '/workspace'",
            [],
        )
        .unwrap();
    let stale = resolver.hydrate_event(&request).unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);
}

#[test]
fn active_wal_read_is_side_effect_free_and_terminal_race_fails_closed() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("data.sqlite3");
    create_database(&path, 0, "stable body", false);
    let writer = Connection::open(&path).unwrap();
    writer.execute_batch("pragma journal_mode=wal").unwrap();
    insert_conversation(&writer, "/wal", "wal-session", "wal body");
    let before = persistent_directory_snapshot(temp.path());

    let scan = scan_kiro_source_backed_v0(&path, KIRO_SQLITE_SOURCE_FORMAT).unwrap();
    assert_eq!(persistent_directory_snapshot(temp.path()), before);
    assert!(terminal_fence_matches(&path, &scan.terminal_fence).unwrap());

    writer
        .execute(
            "update conversations_v2 set updated_at = 2 where key = '/wal'",
            [],
        )
        .unwrap();
    assert!(!terminal_fence_matches(&path, &scan.terminal_fence).unwrap());
}

#[test]
fn direct_route_publishes_cold_reuses_logical_noop_and_replaces_mutation() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("data.sqlite3");
    let index = temp.path().join("index");
    create_database(&path, 0, "baseline body", false);
    let source = ProviderSource {
        provider: CaptureProvider::KiroCli,
        path: path.clone(),
        exists: true,
        source_format: KIRO_SQLITE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    registration::register(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };

    let cold = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 2);
    assert!(cold.sources[0].frontier().is_none());

    Connection::open(&path)
        .unwrap()
        .execute_batch("vacuum")
        .unwrap();
    let logical_noop =
        refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert_eq!(logical_noop.sources, cold.sources);
    assert_eq!(logical_noop.commit.indexed_documents, 2);
    assert!(logical_noop.removals.is_empty());

    Connection::open(&path)
        .unwrap()
        .execute(
            "update conversations_v2 set value = replace(
                 value, 'baseline body', 'replacement body'
             ) where key = '/workspace'",
            [],
        )
        .unwrap();
    let replacement = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_ne!(replacement.sources, logical_noop.sources);
    assert_eq!(replacement.commit.indexed_documents, 2);
    assert!(replacement.sources[0].frontier().is_none());
}

#[test]
fn owned_route_has_no_generic_captured_driver_or_append_frontier() {
    let registration = include_str!("source_backed/registration.rs");
    assert!(!registration.contains(concat!("captured_route_", "driver")));
    assert!(!registration.contains(concat!("begin_source_", "append")));
    assert!(!registration.contains(concat!("certify_source_", "append")));
    assert!(registration.contains(".with_batch_hydration("));
    assert!(registration.contains("fence_result"));
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

fn persistent_directory_snapshot(directory: &Path) -> Vec<(String, Vec<u8>)> {
    let mut paths = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read(path).unwrap(),
            )
        })
        .collect()
}

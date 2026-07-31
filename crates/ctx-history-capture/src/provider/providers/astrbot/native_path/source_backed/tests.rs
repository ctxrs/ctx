use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{AgentType, CaptureProvider, CoreRecord, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::{params, Connection};
use serde_json::json;

use crate::{
    provider::{
        providers::astrbot::source::{astrbot_query_counters, reset_astrbot_query_counters},
        source_backed::{
            refresh_source_backed_generation, register_astrbot_source_backed_route,
            SourceBackedProviderRegistry, SourceBackedRouteSelection,
        },
    },
    provider_sources::{provider_source_for_path, SqliteSourceAccessError},
    test_support_paths::tempdir,
    DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs,
};

use super::{
    discovery::{open_root_authorized_snapshot_with_hook, source_key},
    parsing::scan_astrbot_source_backed_v0,
    *,
};

fn create_database(path: &Path, session: &str, text: &str) {
    fs::create_dir_all(path.parent().expect("database parent")).expect("create parent");
    let connection = Connection::open(path).expect("open AstrBot fixture");
    connection
        .execute_batch(
            "pragma user_version = 4;
             create table conversations (
                 id integer primary key,
                 inner_conversation_id text,
                 conversation_id text,
                 platform_id text,
                 user_id text,
                 content text not null,
                 title text,
                 persona_id text,
                 token_usage text,
                 created_at integer,
                 updated_at integer
             );
             create table platform_message_history (
                 id integer primary key,
                 platform_id text,
                 user_id text,
                 sender_id text,
                 sender_name text,
                 content text,
                 llm_checkpoint_id text,
                 created_at integer
             );",
        )
        .expect("AstrBot schema");
    insert_conversation(&connection, 1, session, text, &format!("message-{session}"));
}

fn insert_conversation(
    connection: &Connection,
    rowid: i64,
    session: &str,
    text: &str,
    message_id: &str,
) {
    connection
        .execute(
            "insert into conversations (
                 id, inner_conversation_id, conversation_id, platform_id, user_id,
                 content, title, persona_id, token_usage, created_at, updated_at
             ) values (?1, ?2, ?3, 'webchat', 'user', ?4, 'title', 'persona',
                       '{\"prompt\":1,\"completion\":2}', ?5, ?5)",
            params![
                rowid,
                session,
                format!("conversation-{session}"),
                json!([{
                    "id": message_id,
                    "role": "user",
                    "content": text,
                }])
                .to_string(),
                1_780_000_000_000_i64 + rowid,
            ],
        )
        .expect("AstrBot conversation");
}

fn context(home: &Path, cwd: &Path) -> DiscoveryContext {
    DiscoveryContext::new(
        home,
        cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
}

fn selected_source(path: &Path) -> AstrBotSourceBackedSourceV0 {
    let identity = AstrBotSourceIdentityV0::SelectedCore;
    AstrBotSourceBackedSourceV0 {
        path: path.to_path_buf(),
        source_key: source_key(&identity).unwrap(),
        identity,
    }
}

fn scan_records(source: &AstrBotSourceBackedSourceV0) -> Vec<CoreRecord> {
    let mut records = Vec::new();
    scan_astrbot_source_backed_v0(
        crate::test_provider_sqlite_data_root(),
        source,
        &mut |record| {
            record.validate_contract()?;
            records.push(record);
            Ok(())
        },
    )
    .unwrap();
    records
}

fn sqlite_persistent_bytes(path: &Path) -> Vec<Vec<u8>> {
    // Stock WAL readers may update volatile SHM reader marks.
    ["", "-wal"]
        .into_iter()
        .map(|suffix| {
            let mut component = path.as_os_str().to_os_string();
            component.push(suffix);
            fs::read(PathBuf::from(component)).unwrap()
        })
        .collect()
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn register_route(
    database: &Path,
    data_root: &Path,
    discovery: DiscoveryContext,
) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_astrbot_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::AstrBot, database.to_path_buf()),
        SourceBackedRouteSelection::Automatic,
        data_root,
        discovery,
    )
    .unwrap();
    registry
}

#[cfg(target_os = "linux")]
#[test]
fn source_backed_open_does_not_follow_leaf_swap_after_authorization() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("data_v4.db");
    let attacker = temp.path().join("attacker.db");
    let original = temp.path().join("original.db");
    create_database(&path, "expected", "expected");
    create_database(&attacker, "attacker", "attacker");

    let result = open_root_authorized_snapshot_with_hook(
        crate::test_provider_sqlite_data_root(),
        &path,
        || {
            fs::rename(&path, &original).unwrap();
            fs::rename(&attacker, &path).unwrap();
        },
    );
    assert!(matches!(
        result,
        Err(AstrBotSourceBackedErrorV0::SqliteSource(
            SqliteSourceAccessError::SourceChanged,
        ))
    ));
}

#[test]
fn active_wal_scan_reads_complete_core_without_persistent_source_writes() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("data_v4.db");
    create_database(&path, "wal-session", "before WAL");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    writer
        .execute(
            "update conversations set content = ?1 where id = 1",
            [json!([{
                "id": "message-wal-session",
                "role": "user",
                "content": "AstrBot active WAL sentinel",
            }])
            .to_string()],
        )
        .unwrap();
    let before = sqlite_persistent_bytes(&path);
    let record = scan_records(&selected_source(&path)).remove(0);
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some("AstrBot active WAL sentinel")
    );
    assert_eq!(sqlite_persistent_bytes(&path), before);
    drop(writer);
}

#[test]
fn cold_scan_is_bounded_deterministic_and_emits_valid_stable_core() {
    const ROW_COUNT: i64 = 257;

    let temp = tempdir().unwrap();
    let path = temp.path().join("data_v4.db");
    create_database(&path, "session-1", "prompt-1");
    let connection = Connection::open(&path).unwrap();
    for rowid in 2..=ROW_COUNT {
        insert_conversation(
            &connection,
            rowid,
            &format!("session-{rowid}"),
            &format!("prompt-{rowid}"),
            &format!("message-{rowid}"),
        );
    }
    drop(connection);
    let source = selected_source(&path);

    reset_astrbot_query_counters();
    let first = scan_records(&source);
    assert_eq!(first.len(), usize::try_from(ROW_COUNT).unwrap());
    assert_eq!(
        first
            .iter()
            .map(|record| record.content.meaningful_text())
            .collect::<Vec<_>>(),
        (1..=ROW_COUNT)
            .map(|rowid| format!("prompt-{rowid}"))
            .collect::<Vec<_>>()
    );
    assert!(first.iter().enumerate().all(|(index, record)| {
        let rowid = i64::try_from(index).unwrap() + 1;
        let expected_session = format!("session-{rowid}");
        record.provider_session_id.as_deref() == Some(expected_session.as_str())
            && record.event_sequence == u64::try_from(index).unwrap()
            && record.native_event_id
                == Some(TypedKey::Composite(vec![
                    TypedKey::I64(rowid),
                    TypedKey::Utf8(format!("message-{rowid}")),
                ]))
    }));
    assert_eq!(
        astrbot_query_counters(),
        crate::provider::providers::astrbot::source::AstrBotQueryCounters {
            candidate_set_reads: 7,
            row_set_reads: 5,
            decoded_rows: 257,
        }
    );

    reset_astrbot_query_counters();
    let replay = scan_records(&source);
    assert_eq!(replay, first);
    assert_eq!(
        astrbot_query_counters(),
        crate::provider::providers::astrbot::source::AstrBotQueryCounters {
            candidate_set_reads: 7,
            row_set_reads: 5,
            decoded_rows: 257,
        }
    );
}

#[test]
fn complete_native_structure_and_event_fields_survive_direct_core_projection() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("data_v4.db");
    let user_text = format!("astrbot-user-{}-user-tail", "u".repeat(20_000));
    let output_text = format!("astrbot-output-{}-output-tail", "o".repeat(20_000));
    let platform_text = format!("astrbot-platform-{}-platform-tail", "p".repeat(20_000));
    create_database(&path, "complete-session", &user_text);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "update conversations set content = ?1 where id = 1",
            [json!([
                {
                    "id": "message-complete",
                    "role": "user",
                    "content": user_text,
                    "native": {"kind": "message"},
                },
                {
                    "id": "tool-complete",
                    "role": "tool",
                    "content": output_text,
                    "status": "success",
                }
            ])
            .to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into platform_message_history (
                 id, platform_id, user_id, sender_id, sender_name, content,
                 llm_checkpoint_id, created_at
             ) values (7, 'webchat', 'platform-user', 'platform-user', 'User', ?1,
                       null, 1780000002000)",
            [json!([{"text": platform_text, "native": "block"}]).to_string()],
        )
        .unwrap();
    drop(connection);

    let records = scan_records(&selected_source(&path));
    assert_eq!(records.len(), 3);
    let user = records
        .iter()
        .find(|record| record.content.meaningful_text().ends_with("user-tail"))
        .unwrap();
    let output = records
        .iter()
        .find(|record| record.content.meaningful_text().ends_with("output-tail"))
        .unwrap();
    let platform = records
        .iter()
        .find(|record| record.content.meaningful_text().ends_with("platform-tail"))
        .unwrap();

    assert_eq!(user.event_type, "message");
    assert_eq!(user.role.as_deref(), Some("user"));
    assert_eq!(output.event_type, "tool_output");
    assert_eq!(output.role.as_deref(), Some("tool"));
    assert_eq!(platform.event_type, "message");
    assert_eq!(platform.role.as_deref(), Some("user"));
    assert_eq!(
        records
            .iter()
            .map(|record| record.event_sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    for record in &records {
        assert_eq!(record.agent_type, AgentType::Primary.as_str());
        assert!(record.is_primary);
        assert!(record.occurred_at_unix_ms.is_some());
        assert!(record.provider_session_id.is_some());
        assert!(record.native_event_id.is_some());
        assert!(record.content.structured_content.is_some());
        assert!(record.repository_bindings.is_empty());
        assert!(record.repository_file_observations.is_empty());
        assert!(record.repository_vcs_observations.is_empty());
    }
}

#[test]
fn multi_instance_inventory_certifies_and_preserves_stable_ids() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("core");
    create_database(
        &cwd.join("data/data_v4.db"),
        "selected-session",
        "selected-core-prompt",
    );
    for (instance, session) in [
        ("123e4567-e89b-12d3-a456-426614174000", "launcher-one"),
        ("123e4567-e89b-12d3-a456-426614174001", "launcher-two"),
    ] {
        create_database(
            &home
                .join(".astrbot_launcher/instances")
                .join(instance)
                .join("core/data/data_v4.db"),
            session,
            &format!("{session}-prompt"),
        );
    }
    create_database(
        &home.join(".astrbot_launcher/instances/not-a-uuid/core/data/data_v4.db"),
        "ignored-instance",
        "ignored-instance-prompt",
    );

    let discovery = context(&home, &cwd);
    let opening = AstrBotSourceBackedInventoryV0::discover(&discovery).unwrap();
    assert_eq!(opening.sources().len(), 3);
    let first = opening
        .sources()
        .iter()
        .map(|source| {
            let record = scan_records(source).remove(0);
            (
                source.source_key().identity().digest(),
                record.session_id,
                record.event_id,
            )
        })
        .collect::<Vec<_>>();
    let closing = AstrBotSourceBackedInventoryV0::discover(&discovery).unwrap();
    assert_eq!(opening.certify(&closing).unwrap().observed_sources(), 3);
    let replay = closing
        .sources()
        .iter()
        .map(|source| {
            let record = scan_records(source).remove(0);
            (
                source.source_key().identity().digest(),
                record.session_id,
                record.event_id,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(replay, first);
}

#[test]
fn tantivy_round_trip_is_complete_locator_free_and_replacement_lifecycle_is_exact() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("core");
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = cwd.join("data/data_v4.db");
    let original = format!("astrbot-core-{}-original-tail", "x".repeat(20_000));
    create_database(&database, "core-session", &original);
    let source = selected_source(&database);
    let expected = scan_records(&source).remove(0);
    let registry = register_route(&database, &data_root, context(&home, &cwd));

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let stored = index
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(stored, expected);
    assert_eq!(
        stored.content.normalized_body.as_deref(),
        Some(original.as_str())
    );
    assert_eq!(
        stored.native_event_id,
        Some(TypedKey::Composite(vec![
            TypedKey::I64(1),
            TypedKey::Utf8("message-core-session".to_owned()),
        ]))
    );
    assert_eq!(
        stored.content.structured_content.as_ref().unwrap()["content"],
        original
    );
    let encoded = serde_json::to_string(&stored).unwrap();
    assert!(!encoded.contains("locator"));
    assert!(!encoded.contains("source_path"));
    assert!(!encoded.contains(database.to_string_lossy().as_ref()));

    let noop = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);

    let rewritten = format!("astrbot-core-{}-rewritten-tail", "r".repeat(20_000));
    Connection::open(&database)
        .unwrap()
        .execute(
            "update conversations set content = ?1 where id = 1",
            [json!([{
                "id": "message-core-session",
                "role": "user",
                "content": rewritten,
            }])
            .to_string()],
        )
        .unwrap();
    // Core is generation-owned: source mutation cannot alter the published record.
    assert_eq!(
        VerifiedIndex::open(&index_root)
            .unwrap()
            .core_record_by_id(expected.event_id.as_uuid())
            .unwrap()
            .unwrap()
            .content
            .normalized_body
            .as_deref(),
        Some(original.as_str())
    );

    let rewritten_generation =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_ne!(
        rewritten_generation.commit.generation_id,
        cold.commit.generation_id
    );
    let rewritten_record = VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(rewritten_record.event_id, expected.event_id);
    assert_eq!(
        rewritten_record.content.normalized_body.as_deref(),
        Some(rewritten.as_str())
    );

    let connection = Connection::open(&database).unwrap();
    insert_conversation(
        &connection,
        2,
        "appended-session",
        "appended body",
        "message-appended",
    );
    drop(connection);
    let appended = scan_records(&source)
        .into_iter()
        .find(|record| record.provider_session_id.as_deref() == Some("appended-session"))
        .unwrap();
    let appended_generation =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_ne!(
        appended_generation.commit.generation_id,
        rewritten_generation.commit.generation_id
    );
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(appended.event_id.as_uuid())
        .unwrap()
        .is_some());

    Connection::open(&database)
        .unwrap()
        .execute("delete from conversations where id = 2", [])
        .unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_ne!(
        deleted.commit.generation_id,
        appended_generation.commit.generation_id
    );
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(appended.event_id.as_uuid())
        .unwrap()
        .is_none());

    fs::remove_file(&database).unwrap();
    let source_deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_ne!(
        source_deleted.commit.generation_id,
        deleted.commit.generation_id
    );
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .is_none());

    let provider_source = concat!(
        include_str!("../source_backed.rs"),
        include_str!("identity.rs"),
        include_str!("parsing.rs"),
        include_str!("discovery.rs"),
        include_str!("../../model.rs"),
        include_str!("../../source.rs"),
        include_str!("../../../astrbot.rs"),
    );
    for forbidden in [
        "LexicalDocument",
        "SourceRecordLocator",
        "source_path",
        "hydrate",
        "hydration",
        "resolver",
        "provider_local_preview",
        "MAX_BODY_PREVIEW_CHARS",
    ] {
        assert!(
            !provider_source.contains(forbidden),
            "AstrBot direct-Core path contains forbidden token {forbidden}"
        );
    }
}

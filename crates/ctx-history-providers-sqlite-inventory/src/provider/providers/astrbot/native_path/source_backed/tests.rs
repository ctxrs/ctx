use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{CoreRecord, TypedKey};
use rusqlite::{params, Connection};
use serde_json::json;

use crate::{
    provider::providers::astrbot::source::{astrbot_query_counters, reset_astrbot_query_counters},
    provider_sources::{
        fail_next_opened_snapshot_cleanup_for_test, sqlite_retry_decision, SqliteCleanupStatus,
        SqliteRetryDecision,
    },
    test_support_paths::tempdir,
};
use ctx_history_source_discovery::{DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs};

use super::{
    discovery::{
        open_root_authorized_snapshot_with_hook, source_key, source_key_scoped,
        AstrBotSourceIdentityV0,
    },
    parsing::scan_astrbot_source_backed_v0,
    *,
};

#[test]
fn root_scope_composes_with_astrbot_slots_and_preserves_unqualified_identity() {
    use ctx_history_core::{CaptureProvider, SourceAnchor, SourceAnchorScope, SourceKey};

    let selected = AstrBotSourceIdentityV0::SelectedCore;
    let released = SourceKey::derive(
        CaptureProvider::AstrBot.as_str(),
        crate::ASTRBOT_SQLITE_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        SOURCE_IDENTITY_VERSION,
        SourceAnchor::provider_native(
            SELECTED_SOURCE_NAMESPACE,
            TypedKey::utf8("selected-core").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let unqualified = source_key_scoped(&selected, SourceAnchorScope::Unqualified).unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let first = source_key_scoped(&selected, SourceAnchorScope::Lineage([0x11; 32])).unwrap();
    let second = source_key_scoped(&selected, SourceAnchorScope::Lineage([0x22; 32])).unwrap();
    assert_ne!(
        super::identity::stable_session_id(&first, "shared-conversation").unwrap(),
        super::identity::stable_session_id(&second, "shared-conversation").unwrap()
    );

    let sibling = source_key_scoped(
        &AstrBotSourceIdentityV0::LauncherInstance("shared-launcher-client".to_owned()),
        SourceAnchorScope::Lineage([0x11; 32]),
    )
    .unwrap();
    assert_ne!(first.identity(), sibling.identity());
}

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
        &mut |record: CoreRecord| {
            record.validate_contract()?;
            records.push(record);
            Ok(())
        },
    )
    .unwrap();
    records
}

#[test]
fn transferred_snapshot_scan_failure_reports_cleanup_fatal_error() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("data_v4.db");
    create_database(&path, "cleanup", "cleanup");
    let source = selected_source(&path);
    fail_next_opened_snapshot_cleanup_for_test();
    let (_source_root, snapshot) = open_root_authorized_snapshot_with_hook(
        crate::test_provider_sqlite_data_root(),
        &path,
        || {},
    )
    .unwrap();

    let error = scan_astrbot_snapshot_v0(
        &source,
        snapshot,
        &mut |_record| Err(AstrBotSourceBackedErrorV0::CountOverflow),
        &mut crate::lifecycle::SourceBackedRecordRejectionDrafts::default(),
    )
    .unwrap_err();
    let AstrBotSourceBackedErrorV0::SnapshotCleanup { primary, cleanup } = error else {
        panic!("expected typed primary-plus-cleanup failure");
    };
    assert!(matches!(
        *primary,
        AstrBotSourceBackedErrorV0::CountOverflow
    ));
    let diagnostic = cleanup.diagnostic().unwrap();
    assert_eq!(diagnostic.cleanup, SqliteCleanupStatus::Failed);
    assert_eq!(
        sqlite_retry_decision(&cleanup),
        SqliteRetryDecision::RouteFatalResource
    );
}

fn sqlite_persistent_bytes(path: &Path) -> Vec<Vec<u8>> {
    ["", "-wal"]
        .into_iter()
        .map(|suffix| {
            let mut component = path.as_os_str().to_os_string();
            component.push(suffix);
            fs::read(PathBuf::from(component)).unwrap()
        })
        .collect()
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
            crate::provider_sources::SqliteSourceAccessError::SourceChanged,
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
    assert_eq!(
        first
            .iter()
            .map(|record| (
                record.provider_session_id.clone(),
                record.event_sequence,
                record.native_event_id.clone(),
            ))
            .collect::<Vec<_>>(),
        (1..=ROW_COUNT)
            .enumerate()
            .map(|(index, rowid)| {
                let provider_session_id = format!("session-{rowid}");
                let native_message_id = if rowid == 1 {
                    format!("message-{provider_session_id}")
                } else {
                    format!("message-{rowid}")
                };
                (
                    Some(provider_session_id),
                    u64::try_from(index).unwrap(),
                    Some(TypedKey::Composite(vec![
                        TypedKey::I64(rowid),
                        TypedKey::Utf8(native_message_id),
                    ])),
                )
            })
            .collect::<Vec<_>>()
    );
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
fn row_local_projection_failure_rejects_only_its_conversation() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("data_v4.db");
    create_database(&path, "bad-session", "bad body");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "update conversations set inner_conversation_id = ?1 where id = 1",
            ["x".repeat(70 * 1024)],
        )
        .unwrap();
    insert_conversation(
        &connection,
        2,
        "healthy-session",
        "healthy body",
        "healthy-message",
    );
    drop(connection);

    let source = selected_source(&path);
    let mut records = Vec::new();
    let certificate = scan_astrbot_source_backed_v0(
        crate::test_provider_sqlite_data_root(),
        &source,
        &mut |record| {
            records.push(record);
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].content.meaningful_text(), "healthy body");
    assert_eq!(certificate.counts().complete_records, 2);
    assert_eq!(certificate.counts().retained_records, 1);
    assert_eq!(certificate.counts().rejected_records, 1);
    assert_eq!(certificate.counts().indexed_documents, 1);
}

#[test]
fn row_local_projection_filter_preserves_core_invariants() {
    assert!(astrbot_row_projection_error(
        &AstrBotSourceBackedErrorV0::Projection(ProjectionContractError::FieldTooLarge {
            field: "typed_key_utf8",
            actual: 2,
            maximum: 1,
        })
    ));
    for error in [
        AstrBotSourceBackedErrorV0::Projection(ProjectionContractError::SourceChanged),
        AstrBotSourceBackedErrorV0::Projection(ProjectionContractError::InvalidDerivedIdentity),
        AstrBotSourceBackedErrorV0::CoreRecord(CoreRecordError::Projection(
            ProjectionContractError::SourceChanged,
        )),
        AstrBotSourceBackedErrorV0::CoreRecord(CoreRecordError::InvalidIdentityRelationship),
    ] {
        assert!(!astrbot_row_projection_error(&error), "{error:?}");
    }
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
        assert_eq!(record.root_session_id, None);
        assert!(record.occurred_at_unix_ms.is_some());
        assert!(record.provider_session_id.is_some());
        assert!(record.native_event_id.is_some());
        assert!(record.content.structured_content.is_some());
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

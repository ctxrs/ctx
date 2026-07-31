use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    AgentType, BatchHydrationRequest, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind, LocatorRevisionPolicy, NativeRecordCoordinate, SourceRecordLocator,
    TypedKey,
};
use ctx_history_index::LexicalDocument;
use rusqlite::{params, Connection};
use serde_json::json;

use crate::{
    provider::providers::astrbot::source::{astrbot_query_counters, reset_astrbot_query_counters},
    provider_sources::SqliteSourceAccessError,
    test_support_paths::tempdir,
    DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs,
};

use super::{
    discovery::{open_root_authorized_snapshot_with_hook, source_key},
    hydration::AstrBotSourceBackedResolverV0,
    parsing::scan_astrbot_source_backed_v0,
    *,
};

fn create_database(path: &Path, session: &str, text: &str) {
    fs::create_dir_all(path.parent().expect("database parent")).expect("create parent");
    let conn = Connection::open(path).expect("open AstrBot fixture");
    conn.execute_batch(
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
    conn.execute(
        "insert into conversations (
                 id, inner_conversation_id, conversation_id, platform_id, user_id,
                 content, title, persona_id, token_usage, created_at, updated_at
             ) values (1, ?1, ?2, 'webchat', 'user', ?3, 'title', 'persona',
                       '{\"prompt\":1,\"completion\":2}', 1780000000000, 1780000001000)",
        params![
            session,
            format!("conversation-{session}"),
            json!([{
                "id": format!("message-{session}"),
                "role": "user",
                "content": text,
            }])
            .to_string(),
        ],
    )
    .expect("AstrBot conversation");
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
fn active_wal_scan_reads_latest_rows_without_persistent_source_writes() {
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
    let source = selected_source(&path);
    let documents = scan_documents(&source);
    let document = documents
        .iter()
        .find(|document| document.body.contains("AstrBot active WAL sentinel"))
        .unwrap();
    let hydrated = resolver_for(&source)
        .hydrate_event(&event_request(document))
        .unwrap();
    assert_eq!(hydrated.provider_bytes, b"AstrBot active WAL sentinel");
    assert_eq!(sqlite_persistent_bytes(&path), before);
    drop(writer);
}

fn context(home: &Path, cwd: &Path) -> DiscoveryContext {
    DiscoveryContext::new(
        home,
        cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
}

fn relation(document: &LexicalDocument) -> &str {
    match document.locator.coordinate() {
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation, ..
        } => logical_relation,
        coordinate => panic!("unexpected AstrBot coordinate: {coordinate:?}"),
    }
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

fn selected_source(path: &Path) -> AstrBotSourceBackedSourceV0 {
    let identity = AstrBotSourceIdentityV0::SelectedCore;
    AstrBotSourceBackedSourceV0 {
        path: path.to_path_buf(),
        source_key: source_key(&identity).unwrap(),
        identity,
    }
}

fn resolver_for(source: &AstrBotSourceBackedSourceV0) -> AstrBotSourceBackedResolverV0 {
    AstrBotSourceBackedResolverV0::from_source(source.clone())
}

fn scan_documents(source: &AstrBotSourceBackedSourceV0) -> Vec<LexicalDocument> {
    let mut documents = Vec::new();
    scan_astrbot_source_backed_v0(
        crate::test_provider_sqlite_data_root(),
        source,
        &mut |document| {
            documents.push(document);
            Ok(())
        },
    )
    .unwrap();
    documents
}

#[test]
fn astrbot_scan_and_hydration_queries_are_bounded_by_row_sets() {
    const ROW_COUNT: i64 = 257;

    let temp = tempdir().unwrap();
    let path = temp.path().join("data_v4.db");
    create_database(&path, "session-1", "prompt-1");
    let connection = Connection::open(&path).unwrap();
    for rowid in 2..=ROW_COUNT {
        connection
            .execute(
                "insert into conversations (
                     id, inner_conversation_id, conversation_id, platform_id, user_id,
                     content, title, persona_id, token_usage, created_at, updated_at
                 ) values (?1, ?2, ?3, 'webchat', 'user', ?4, 'title', 'persona',
                           null, ?5, ?5)",
                params![
                    rowid,
                    format!("session-{rowid}"),
                    format!("conversation-{rowid}"),
                    json!([{
                        "id": format!("message-{rowid}"),
                        "role": "user",
                        "content": format!("prompt-{rowid}"),
                    }])
                    .to_string(),
                    1_780_000_000_000_i64 + rowid,
                ],
            )
            .unwrap();
    }
    drop(connection);
    let source = selected_source(&path);

    reset_astrbot_query_counters();
    let documents = scan_documents(&source);
    assert_eq!(documents.len(), usize::try_from(ROW_COUNT).unwrap());
    assert_eq!(
        documents
            .iter()
            .map(|document| document.body.as_str())
            .collect::<Vec<_>>(),
        (1..=ROW_COUNT)
            .map(|rowid| format!("prompt-{rowid}"))
            .collect::<Vec<_>>()
    );
    for (index, document) in documents.iter().enumerate() {
        let expected_rowid = i64::try_from(index).unwrap() + 1;
        assert!(matches!(
            document.locator.coordinate(),
            NativeRecordCoordinate::ProviderSqlite {
                logical_relation,
                primary_key: TypedKey::Composite(parts),
                ..
            } if logical_relation == CONVERSATION_MESSAGE_RELATION
                && parts.as_slice()
                    == [TypedKey::I64(expected_rowid), TypedKey::U64(0)]
        ));
    }
    assert_eq!(
        astrbot_query_counters(),
        crate::provider::providers::astrbot::source::AstrBotQueryCounters {
            candidate_set_reads: 7,
            hydration_set_reads: 5,
            hydrated_rows: 257,
        }
    );

    reset_astrbot_query_counters();
    let replay = scan_documents(&source);
    assert_eq!(
        replay
            .iter()
            .map(|document| (
                document.event_id,
                document.locator.coordinate().clone(),
                document.body.clone(),
            ))
            .collect::<Vec<_>>(),
        documents
            .iter()
            .map(|document| (
                document.event_id,
                document.locator.coordinate().clone(),
                document.body.clone(),
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        astrbot_query_counters(),
        crate::provider::providers::astrbot::source::AstrBotQueryCounters {
            candidate_set_reads: 7,
            hydration_set_reads: 5,
            hydrated_rows: 257,
        }
    );

    let requested = documents
        .iter()
        .rev()
        .map(event_request)
        .collect::<Vec<_>>();
    reset_astrbot_query_counters();
    let hydrated = resolver_for(&source)
        .hydrate_batch_request(&BatchHydrationRequest::new(requested.clone()).unwrap())
        .unwrap();
    assert_eq!(
        hydrated
            .records()
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        requested
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        hydrated
            .records()
            .iter()
            .map(|record| String::from_utf8(record.provider_bytes.clone()).unwrap())
            .collect::<Vec<_>>(),
        (1..=ROW_COUNT)
            .rev()
            .map(|rowid| format!("prompt-{rowid}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        astrbot_query_counters(),
        crate::provider::providers::astrbot::source::AstrBotQueryCounters {
            candidate_set_reads: 0,
            hydration_set_reads: 2,
            hydrated_rows: 257,
        }
    );
}

fn event_request(document: &LexicalDocument) -> EventHydrationRequest {
    EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
}

fn request_with_locator_evidence(
    document: &LexicalDocument,
    coordinate: NativeRecordCoordinate,
    record_digest: [u8; 32],
) -> EventHydrationRequest {
    let locator = SourceRecordLocator::new(
        document.source.clone(),
        coordinate,
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record_digest,
    )
    .unwrap();
    EventHydrationRequest::new(document.event_id, locator).unwrap()
}

#[test]
fn astrbot_source_backed_multi_instance_cold_scan_has_stable_ids_and_inventory() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("core");
    create_database(
        &cwd.join("data/data_v4.db"),
        "selected-session",
        "selected-core-prompt",
    );
    let instances = [
        (
            "123e4567-e89b-12d3-a456-426614174000",
            "launcher-one",
            "launcher-one-prompt",
        ),
        (
            "123e4567-e89b-12d3-a456-426614174001",
            "launcher-two",
            "launcher-two-prompt",
        ),
    ];
    for (instance, session, text) in instances {
        create_database(
            &home
                .join(".astrbot_launcher/instances")
                .join(instance)
                .join("core/data/data_v4.db"),
            session,
            text,
        );
    }
    create_database(
        &home.join(".astrbot_launcher/instances/not-a-uuid/core/data/data_v4.db"),
        "ignored-instance",
        "ignored-instance-prompt",
    );

    let discovery = context(&home, &cwd);
    let opening = AstrBotSourceBackedInventoryV0::discover(&discovery).expect("opening inventory");
    assert_eq!(opening.sources().len(), 3);
    assert_eq!(
        &opening.sources()[0].identity,
        &AstrBotSourceIdentityV0::SelectedCore
    );
    assert!(opening.sources()[1..].iter().all(|source| matches!(
        &source.identity,
        AstrBotSourceIdentityV0::LauncherInstance(_)
    )));

    let mut first_ids = Vec::new();
    for source in opening.sources() {
        let mut documents = Vec::new();
        let certificate = scan_astrbot_source_backed_v0(
            crate::test_provider_sqlite_data_root(),
            source,
            &mut |document| {
                documents.push(document);
                Ok(())
            },
        )
        .expect("cold source scan");
        assert_eq!(certificate.counts().complete_records, 1);
        assert_eq!(certificate.counts().retained_records, 1);
        assert_eq!(certificate.counts().indexed_documents, 1);
        assert_eq!(documents.len(), 1);
        first_ids.push((
            source.source_key().identity().digest(),
            documents[0].session_id.digest(),
            documents[0].event_id.digest(),
        ));
    }

    let closing = AstrBotSourceBackedInventoryV0::discover(&discovery).expect("closing inventory");
    let certified = opening.certify(&closing).expect("certified inventory");
    assert_eq!(certified.observed_sources(), 3);
    let mut second_ids = Vec::new();
    for source in closing.sources() {
        let mut documents = Vec::new();
        scan_astrbot_source_backed_v0(
            crate::test_provider_sqlite_data_root(),
            source,
            &mut |document| {
                documents.push(document);
                Ok(())
            },
        )
        .expect("repeat source scan");
        second_ids.push((
            source.source_key().identity().digest(),
            documents[0].session_id.digest(),
            documents[0].event_id.digest(),
        ));
    }
    assert_eq!(first_ids, second_ids);
}

#[test]
fn astrbot_source_backed_reopens_full_conversation_and_platform_text_exactly() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("core");
    let database = cwd.join("data/data_v4.db");
    let exact_text = format!(
        "astrbot-exact-content-{}-full-body-tail-sentinel",
        "x".repeat(4_096)
    );
    let exact_output = format!(
        "astrbot-tool-output-{}-output-tail-sentinel",
        "o".repeat(4_096)
    );
    let platform_text = format!(
        "astrbot-platform-content-{}-platform-tail-sentinel",
        "p".repeat(4_096)
    );
    create_database(&database, "exact-session", &exact_text);
    let conn = Connection::open(&database).expect("open platform fixture");
    conn.execute(
        "update conversations set content = ?1 where id = 1",
        [json!([
            {
                "id": "message-exact-session",
                "role": "user",
                "content": exact_text,
            },
            {
                "id": "tool-exact-session",
                "role": "tool",
                "content": exact_output,
                "status": "success",
            }
        ])
        .to_string()],
    )
    .expect("conversation message and output");
    conn.execute(
        "insert into platform_message_history (
                 id, platform_id, user_id, sender_id, sender_name, content,
                 llm_checkpoint_id, created_at
             ) values (7, 'webchat', 'platform-user', 'platform-user', 'User', ?1,
                       null, 1780000002000)",
        [&platform_text],
    )
    .expect("platform message");
    drop(conn);

    let inventory =
        AstrBotSourceBackedInventoryV0::discover(&context(&home, &cwd)).expect("inventory");
    let source = inventory.sources().first().expect("selected source");
    let mut documents = Vec::new();
    scan_astrbot_source_backed_v0(
        crate::test_provider_sqlite_data_root(),
        source,
        &mut |document| {
            documents.push(document);
            Ok(())
        },
    )
    .expect("source scan");
    assert_eq!(documents.len(), 3);

    let conversation = documents
        .iter()
        .find(|document| relation(document) == CONVERSATION_MESSAGE_RELATION)
        .expect("conversation document");
    assert_eq!(conversation.body, exact_text);
    assert!(conversation.body.ends_with("full-body-tail-sentinel"));
    assert_eq!(conversation.parent_session_id, None);
    assert_eq!(conversation.root_session_id, conversation.session_id);
    assert_eq!(
        conversation.provider_session_id.as_deref(),
        Some("exact-session")
    );
    assert_eq!(conversation.branch, None);
    assert_eq!(
        conversation.source_path.as_deref(),
        Some(database.to_string_lossy().as_ref())
    );
    assert_eq!(conversation.agent_type, AgentType::Primary.as_str());
    assert!(conversation.is_primary);
    assert_eq!(
        conversation.locator.revision_policy(),
        LocatorRevisionPolicy::StableRecordEvidence
    );
    assert!(conversation
        .locator
        .certified_source_revision_digest()
        .is_none());
    assert!(matches!(
        conversation.locator.coordinate(),
        NativeRecordCoordinate::ProviderSqlite {
            primary_key: TypedKey::Composite(parts),
            row_version: Some(TypedKey::Bytes(row_digest)),
            ..
        } if matches!(
            parts.as_slice(),
            [TypedKey::I64(1), TypedKey::U64(0)]
        ) && row_digest.len() == 32
    ));
    let resolver = AstrBotSourceBackedResolverV0::from_inventory(
        crate::test_provider_sqlite_data_root(),
        &inventory,
    )
    .expect("resolver");
    let request = event_request(conversation);
    let hydrated = resolver
        .hydrate_event(&request)
        .expect("exact conversation hydration");
    assert_eq!(hydrated.provider_bytes, exact_text.as_bytes());

    let output = documents
        .iter()
        .find(|document| relation(document) == CONVERSATION_OUTPUT_RELATION)
        .expect("conversation output document");
    assert_eq!(output.body, exact_output);
    assert!(output.body.ends_with("output-tail-sentinel"));
    let hydrated = resolver
        .hydrate_event(&event_request(output))
        .expect("exact conversation-output hydration");
    assert_eq!(hydrated.provider_bytes, exact_output.as_bytes());

    let platform = documents
        .iter()
        .find(|document| relation(document) == PLATFORM_MESSAGE_RELATION)
        .expect("platform document");
    assert_eq!(platform.body, platform_text);
    assert!(platform.body.ends_with("platform-tail-sentinel"));
    let request = event_request(platform);
    let hydrated = resolver
        .hydrate_event(&request)
        .expect("exact platform-message hydration");
    assert_eq!(hydrated.provider_bytes, platform_text.as_bytes());

    let requested = vec![
        event_request(platform),
        event_request(output),
        event_request(conversation),
    ];
    let batch = BatchHydrationRequest::new(requested.clone()).unwrap();
    let hydrated = resolver.hydrate_batch_request(&batch).unwrap();
    assert_eq!(
        hydrated
            .records()
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        requested
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        hydrated.records()[0].provider_bytes,
        platform_text.as_bytes()
    );
    assert_eq!(
        hydrated.records()[1].provider_bytes,
        exact_output.as_bytes()
    );
    assert_eq!(hydrated.records()[2].provider_bytes, exact_text.as_bytes());
}

#[test]
fn astrbot_hydration_types_stale_row_and_record_digest() {
    let temp = tempdir().unwrap();
    let stale_path = temp.path().join("stale.db");
    create_database(&stale_path, "stale-session", "original AstrBot body");
    let stale_source = selected_source(&stale_path);
    let documents = scan_documents(&stale_source);
    let document = &documents[0];
    Connection::open(&stale_path)
        .unwrap()
        .execute(
            "update conversations set content = ?1 where id = 1",
            [json!([{
                "id": "message-stale-session",
                "role": "user",
                "content": "rewritten AstrBot body with a different length",
            }])
            .to_string()],
        )
        .unwrap();
    let failure = resolver_for(&stale_source)
        .hydrate_event(&event_request(document))
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);

    let digest_path = temp.path().join("digest.db");
    create_database(&digest_path, "digest-session", "digest AstrBot body");
    let digest_source = selected_source(&digest_path);
    let documents = scan_documents(&digest_source);
    let document = &documents[0];
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        ..
    } = document.locator.coordinate()
    else {
        panic!("expected provider SQLite locator");
    };
    let coordinate = NativeRecordCoordinate::ProviderSqlite {
        logical_relation: logical_relation.clone(),
        primary_key: primary_key.clone(),
        row_version: Some(TypedKey::bytes(vec![0x6b; 32]).unwrap()),
    };
    let request =
        request_with_locator_evidence(document, coordinate, *document.locator.record_digest());
    let failure = resolver_for(&digest_source)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);
    let request =
        request_with_locator_evidence(document, document.locator.coordinate().clone(), [0xb6; 32]);
    let failure = resolver_for(&digest_source)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);
}

#[test]
fn astrbot_hydration_distinguishes_missing_row_deletion_and_unavailable_root() {
    let temp = tempdir().unwrap();
    let missing_path = temp.path().join("missing-row.db");
    create_database(&missing_path, "missing-session", "missing AstrBot body");
    let missing_source = selected_source(&missing_path);
    let documents = scan_documents(&missing_source);
    Connection::open(&missing_path)
        .unwrap()
        .execute("delete from conversations", [])
        .unwrap();
    let request = request_with_locator_evidence(
        &documents[0],
        documents[0].locator.coordinate().clone(),
        *documents[0].locator.record_digest(),
    );
    let failure = resolver_for(&missing_source)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::MissingRecord);

    let deleted_path = temp.path().join("deleted.db");
    create_database(&deleted_path, "deleted-session", "deleted AstrBot body");
    let deleted_source = selected_source(&deleted_path);
    let request = event_request(&scan_documents(&deleted_source)[0]);
    fs::remove_file(&deleted_path).unwrap();
    let failure = resolver_for(&deleted_source)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::ConfirmedDeleted);

    let available_root = temp.path().join("available-root");
    let unavailable_path = available_root.join("data_v4.db");
    create_database(
        &unavailable_path,
        "unavailable-session",
        "unavailable AstrBot body",
    );
    let unavailable_source = selected_source(&unavailable_path);
    let request = event_request(&scan_documents(&unavailable_source)[0]);
    fs::rename(&available_root, temp.path().join("offline-root")).unwrap();
    let failure = resolver_for(&unavailable_source)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::TemporarilyUnavailable);
}

#[test]
fn astrbot_hydration_types_malformed_rows_schema_and_locator_without_fallbacks() {
    let temp = tempdir().unwrap();
    let malformed_path = temp.path().join("malformed.db");
    create_database(&malformed_path, "malformed-session", "valid AstrBot body");
    let malformed_source = selected_source(&malformed_path);
    let documents = scan_documents(&malformed_source);
    Connection::open(&malformed_path)
        .unwrap()
        .execute(
            "update conversations set content = cast(x'80' as text) where id = 1",
            [],
        )
        .unwrap();
    let request = request_with_locator_evidence(
        &documents[0],
        documents[0].locator.coordinate().clone(),
        *documents[0].locator.record_digest(),
    );
    let failure = resolver_for(&malformed_source)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);

    let unsupported_path = temp.path().join("unsupported.db");
    create_database(
        &unsupported_path,
        "unsupported-session",
        "valid AstrBot body",
    );
    let unsupported_source = selected_source(&unsupported_path);
    let request = event_request(&scan_documents(&unsupported_source)[0]);
    Connection::open(&unsupported_path)
        .unwrap()
        .execute_batch("drop table conversations;")
        .unwrap();
    let failure = resolver_for(&unsupported_source)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(
        failure.kind,
        HydrationFailureKind::UnsupportedParserRevision
    );

    let invalid_path = temp.path().join("invalid.db");
    create_database(&invalid_path, "invalid-session", "valid AstrBot body");
    let invalid_source = selected_source(&invalid_path);
    let documents = scan_documents(&invalid_source);
    let malformed_coordinate = NativeRecordCoordinate::ProviderSqlite {
        logical_relation: CONVERSATION_MESSAGE_RELATION.to_owned(),
        primary_key: TypedKey::I64(1),
        row_version: Some(TypedKey::bytes(vec![0; 32]).unwrap()),
    };
    let request = request_with_locator_evidence(
        &documents[0],
        malformed_coordinate,
        *documents[0].locator.record_digest(),
    );
    let failure = resolver_for(&invalid_source)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);

    let provider_source = concat!(
        include_str!("../source_backed.rs"),
        include_str!("discovery.rs"),
        include_str!("hydration.rs"),
        include_str!("identity.rs"),
        include_str!("parsing.rs"),
    );
    for forbidden in [
        ["work", ".sqlite"].concat(),
        ["ctx_history_", "store"].concat(),
        ["MAX_BODY_", "PREVIEW_CHARS"].concat(),
        ["provider_local_", "preview"].concat(),
    ] {
        assert!(
            !provider_source.contains(&forbidden),
            "AstrBot source-backed path contains forbidden fallback {forbidden}"
        );
    }
    let route_source =
        include_str!("../../../../source_backed/registration/families/sqlite/inventory.rs");
    let route = route_source
        .split_once("pub fn register_astrbot_source_backed_route")
        .unwrap()
        .1
        .split_once("/// Registers Shelley")
        .unwrap()
        .0;
    assert!(route.contains("register_replacement_document_tree_route"));
    assert!(route.contains("SqliteInventoryDocumentAdapter"));
    assert!(route.contains("AstrBotSourceBackedResolverV0"));
    assert!(!route.contains("captured_route_driver"));
    assert!(!route.contains(&["work", ".sqlite"].concat()));
    assert!(!route.contains(&["ctx_history_", "store"].concat()));
}

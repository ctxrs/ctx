use std::path::Path;

use ctx_history_core::{
    BatchHydrationRequest, ContentSourceResolver, EventHydrationRequest, HydrationFailureKind,
    LocatorRevisionPolicy, NativeRecordCoordinate, SourceRecordLocator, TypedKey,
};
use rusqlite::{config::DbConfig, Connection};

use crate::provider::sqlite::sqlite_schema_fingerprint;

use super::super::detect_schema;
use super::{
    discovery::{source_observation, source_revision_digest, LingmaRootAuthorizedSource},
    hydration::LingmaSourceBackedResolverV0,
    parsing::{scan_lingma_source_backed_v0, set_before_database_certification},
    *,
};

fn create_database(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table chat_record (
                    session_id text not null,
                    request_id text,
                    chat_prompt text,
                    summary text,
                    error_result text,
                    gmt_create integer,
                    extra text
                 );",
        )
        .unwrap();
    connection
}

fn insert_row(
    connection: &Connection,
    session_id: &str,
    request_id: &str,
    prompt: &str,
    summary: Option<&str>,
) {
    connection
        .execute(
            "insert into chat_record (
                    session_id, request_id, chat_prompt, summary, error_result, gmt_create, extra
                 ) values (?1, ?2, ?3, ?4, null, 1700000000, null)",
            rusqlite::params![session_id, request_id, prompt, summary],
        )
        .unwrap();
}

fn database(path: &Path, lineage: &str) -> LingmaDatabaseSourceV0 {
    LingmaDatabaseSourceV0::new(path, TypedKey::utf8(lineage).unwrap()).unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn stock_sqlite_snapshot_finish_rejects_leaf_swap_after_open() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let attacker = temp.path().join("attacker.db");
    let original = temp.path().join("original.db");
    drop(create_database(&path));
    drop(create_database(&attacker));

    let authority = LingmaRootAuthorizedSource::retain(&path).unwrap();
    let snapshot = authority.open_snapshot().unwrap();
    std::fs::rename(&path, &original).unwrap();
    std::fs::rename(&attacker, &path).unwrap();
    assert!(snapshot.finish().is_err());
}

fn inventory(databases: Vec<LingmaDatabaseSourceV0>) -> LingmaSourceInventoryV0 {
    LingmaSourceInventoryV0::new(TypedKey::utf8("test-installed-clients").unwrap(), databases)
        .unwrap()
}

fn all_records(scan: &LingmaSourceBackedScanV0) -> Vec<&LingmaSourceBackedRecordV0> {
    scan.databases
        .iter()
        .flat_map(|database| database.records.iter())
        .collect()
}

fn event_request(record: &LingmaSourceBackedRecordV0) -> EventHydrationRequest {
    EventHydrationRequest::new(record.document.event_id, record.document.locator.clone()).unwrap()
}

fn current_source_revision(source: &LingmaDatabaseSourceV0) -> [u8; 32] {
    let authority = LingmaRootAuthorizedSource::retain(&source.path).unwrap();
    let snapshot = authority.open_snapshot().unwrap();
    let digest = {
        let connection = snapshot.connection().unwrap();
        let encoding = detect_schema(connection).unwrap();
        let user_version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap();
        let schema_fingerprint = sqlite_schema_fingerprint(connection).unwrap();
        let observation = source_observation(
            source.source_key().unwrap(),
            snapshot.evidence(),
            user_version,
            &schema_fingerprint,
            encoding,
        )
        .unwrap();
        source_revision_digest(&observation)
    };
    snapshot.finish().unwrap();
    authority.source_root.revalidate().unwrap();
    digest
}

fn request_with_locator_evidence(
    record: &LingmaSourceBackedRecordV0,
    source_revision: [u8; 32],
    coordinate: NativeRecordCoordinate,
    record_digest: [u8; 32],
) -> EventHydrationRequest {
    let locator = SourceRecordLocator::new(
        record.document.source.clone(),
        coordinate,
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source_revision),
        record_digest,
    )
    .unwrap();
    EventHydrationRequest::new(record.document.event_id, locator).unwrap()
}

#[test]
fn source_backed_cold_scan_certifies_stable_full_meaningful_bodies() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first_path = temp.path().join("vscode-local.db");
    let second_path = temp.path().join("jetbrains-local.db");
    let long_prompt = format!("vscode prompt {} lingma-full-body-tail", "v".repeat(4_096));
    let first = create_database(&first_path);
    insert_row(
        &first,
        "vscode-session",
        "vscode-request",
        &long_prompt,
        Some("vscode summary"),
    );
    drop(first);
    let second = create_database(&second_path);
    insert_row(
        &second,
        "jetbrains-session",
        "jetbrains-request",
        "jetbrains prompt",
        Some("jetbrains summary"),
    );
    drop(second);

    let opening = inventory(vec![
        database(&first_path, "vscode:stable:default"),
        database(&second_path, "jetbrains:idea:2026.2"),
    ]);
    let closing = opening.clone();
    let scan = scan_lingma_source_backed_v0(opening, || Ok(closing.clone())).unwrap();
    assert_eq!(scan.databases.len(), 2);
    assert_eq!(all_records(&scan).len(), 4);
    let long_user = all_records(&scan)
        .into_iter()
        .find(|record| {
            record.document.provider_session_id.as_deref() == Some("vscode-session")
                && record.document.role.as_deref() == Some("user")
        })
        .unwrap();
    assert_eq!(long_user.document.body, long_prompt);
    assert!(long_user.document.body.ends_with("lingma-full-body-tail"));
    assert!(all_records(&scan).iter().all(|record| {
        record.document.parent_session_id.is_none()
            && record.document.root_session_id == record.document.session_id
            && record.document.provider_session_id.is_some()
            && record.document.branch.is_none()
            && record.document.source_path.is_some()
            && record.document.agent_type == "primary"
            && record.document.is_primary
    }));
    assert!(scan.databases.iter().all(|database| {
        database.certificate.counts().indexed_documents == 2
            && database.certificate.counts().certified_bytes != 0
    }));

    let reversed = inventory(vec![
        database(&second_path, "jetbrains:idea:2026.2"),
        database(&first_path, "vscode:stable:default"),
    ]);
    let replay = scan_lingma_source_backed_v0(reversed.clone(), || Ok(reversed)).unwrap();
    let mut first_ids = all_records(&scan)
        .into_iter()
        .map(|record| record.document.event_id.digest())
        .collect::<Vec<_>>();
    let mut replay_ids = all_records(&replay)
        .into_iter()
        .map(|record| record.document.event_id.digest())
        .collect::<Vec<_>>();
    first_ids.sort();
    replay_ids.sort();
    assert_eq!(first_ids, replay_ids);
}

#[test]
fn stock_sqlite_snapshot_scan_sees_committed_content_retained_in_active_wal() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let writer = create_database(&path);
    insert_row(
        &writer,
        "wal-session",
        "wal-request",
        "main database prompt",
        None,
    );
    let mode: String = writer
        .query_row("pragma journal_mode = wal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer
        .execute_batch("pragma wal_autocheckpoint = 0")
        .unwrap();
    writer
        .execute(
            "update chat_record
                    set chat_prompt = 'committed Lingma WAL prompt'
                  where request_id = 'wal-request'",
            [],
        )
        .unwrap();
    writer
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    drop(writer);
    assert!(path.with_file_name("local.db-wal").exists());
    assert!(path.with_file_name("local.db-shm").exists());

    let opening = inventory(vec![database(&path, "vscode:stable:wal")]);
    let admitted = opening.clone();
    let closing = opening.clone();
    let scan = scan_lingma_source_backed_v0(opening, || Ok(closing)).unwrap();
    let user = all_records(&scan)
        .into_iter()
        .find(|record| record.document.role.as_deref() == Some("user"))
        .unwrap();
    assert_eq!(user.document.body, "committed Lingma WAL prompt");
    let hydrated = LingmaSourceBackedResolverV0::new(&admitted)
        .unwrap()
        .hydrate_record(user)
        .unwrap();
    assert_eq!(hydrated.provider_bytes, b"committed Lingma WAL prompt");
}

#[cfg(target_os = "linux")]
#[test]
fn stock_sqlite_snapshot_finish_precedes_source_certification() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let replacement = temp.path().join("replacement.db");
    let opening = create_database(&path);
    insert_row(&opening, "session", "request", "opening body", None);
    drop(opening);
    let replacement_connection = create_database(&replacement);
    insert_row(
        &replacement_connection,
        "session",
        "request",
        "replacement body",
        None,
    );
    drop(replacement_connection);
    let inventory = inventory(vec![database(&path, "vscode:stable:finish-order")]);
    let closing = inventory.clone();
    let replaced_path = path.clone();
    set_before_database_certification(Some(Box::new(move || {
        std::fs::rename(&replacement, &replaced_path).unwrap();
    })));

    let result = scan_lingma_source_backed_v0(inventory, || Ok(closing));
    assert!(matches!(
        result,
        Err(LingmaSourceBackedErrorV0::SourceChangedDuringScan
            | LingmaSourceBackedErrorV0::Capture(_))
    ));
}

#[test]
fn source_backed_exact_hydration_and_native_batch_preserve_order_and_full_text() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let prompt = format!(
        "exact row-local Lingma prompt {} lingma-user-tail",
        "x".repeat(4_096)
    );
    let summary = format!(
        "exact Lingma assistant summary {} lingma-summary-tail",
        "s".repeat(4_096)
    );
    let connection = create_database(&path);
    insert_row(
        &connection,
        "exact-session",
        "exact-request",
        &prompt,
        Some(&summary),
    );
    insert_row(
        &connection,
        "error-session",
        "error-request",
        "error prompt",
        None,
    );
    connection
        .execute(
            "update chat_record
                    set error_result = ?1
                  where request_id = 'error-request'",
            [format!(
                "provider failure {} lingma-error-tail",
                "e".repeat(4_096)
            )],
        )
        .unwrap();
    drop(connection);
    let inventory = inventory(vec![database(&path, "vscode:profile:exact")]);
    let closing = inventory.clone();
    let scan = scan_lingma_source_backed_v0(inventory.clone(), || Ok(closing)).unwrap();
    let records = all_records(&scan);
    let user = records
        .iter()
        .copied()
        .find(|record| record.document.body.ends_with("lingma-user-tail"))
        .unwrap();
    let assistant = records
        .iter()
        .copied()
        .find(|record| record.document.body.ends_with("lingma-summary-tail"))
        .unwrap();
    let error = records
        .iter()
        .copied()
        .find(|record| record.document.body.ends_with("lingma-error-tail"))
        .unwrap();
    assert_eq!(user.document.body, prompt);
    assert_eq!(assistant.document.body, summary);
    assert!(error.document.body.starts_with("Lingma error result: "));
    assert!(matches!(
        user.document.locator.coordinate(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation,
            primary_key: TypedKey::Composite(parts),
            row_version: Some(TypedKey::Bytes(version)),
        } if logical_relation == LOGICAL_RELATION
            && matches!(
                parts.as_slice(),
                [
                    TypedKey::I64(1),
                    TypedKey::Utf8(kind),
                    TypedKey::Composite(_)
                ]
                    if kind == USER_PROMPT_COORDINATE
            )
            && version.len() == 32
    ));
    assert_eq!(
        user.document.locator.revision_policy(),
        LocatorRevisionPolicy::ExactSourceRevision
    );
    assert!(user
        .document
        .locator
        .certified_source_revision_digest()
        .is_some());

    let resolver = LingmaSourceBackedResolverV0::new(&inventory).unwrap();
    assert_eq!(
        resolver.hydrate_record(user).unwrap().provider_bytes,
        prompt.as_bytes()
    );
    assert_eq!(
        resolver.hydrate_record(assistant).unwrap().provider_bytes,
        summary.as_bytes()
    );
    assert!(resolver
        .hydrate_record(error)
        .unwrap()
        .provider_bytes
        .ends_with(b"lingma-error-tail"));

    let requested = vec![
        event_request(error),
        event_request(user),
        event_request(assistant),
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
    assert!(hydrated.records()[0]
        .provider_bytes
        .ends_with(b"lingma-error-tail"));
    assert_eq!(hydrated.records()[1].provider_bytes, prompt.as_bytes());
    assert_eq!(hydrated.records()[2].provider_bytes, summary.as_bytes());
}

#[test]
fn source_backed_hydration_types_stale_source_and_record_digest() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let stale_path = temp.path().join("stale.db");
    let connection = create_database(&stale_path);
    insert_row(
        &connection,
        "stale-session",
        "stale-request",
        "original prompt",
        None,
    );
    drop(connection);
    let stale_inventory = inventory(vec![database(&stale_path, "jetbrains:idea:stale-source")]);
    let scan =
        scan_lingma_source_backed_v0(stale_inventory.clone(), || Ok(stale_inventory.clone()))
            .unwrap();
    let stale_record = all_records(&scan)
        .into_iter()
        .find(|record| record.document.role.as_deref() == Some("user"))
        .unwrap();
    Connection::open(&stale_path)
        .unwrap()
        .execute(
            "update chat_record set chat_prompt = 'rewritten prompt'",
            [],
        )
        .unwrap();
    let failure = LingmaSourceBackedResolverV0::new(&stale_inventory)
        .unwrap()
        .hydrate_record(stale_record)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);

    let digest_path = temp.path().join("digest.db");
    let connection = create_database(&digest_path);
    insert_row(
        &connection,
        "digest-session",
        "digest-request",
        "digest prompt",
        None,
    );
    drop(connection);
    let digest_inventory = inventory(vec![database(&digest_path, "vscode:stable:bad-digest")]);
    let scan =
        scan_lingma_source_backed_v0(digest_inventory.clone(), || Ok(digest_inventory.clone()))
            .unwrap();
    let digest_record = all_records(&scan)
        .into_iter()
        .find(|record| record.document.role.as_deref() == Some("user"))
        .unwrap();
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        ..
    } = digest_record.document.locator.coordinate()
    else {
        panic!("expected provider SQLite locator");
    };
    let coordinate = NativeRecordCoordinate::ProviderSqlite {
        logical_relation: logical_relation.clone(),
        primary_key: primary_key.clone(),
        row_version: Some(TypedKey::bytes(vec![0x5a; 32]).unwrap()),
    };
    let request = request_with_locator_evidence(
        digest_record,
        *digest_record
            .document
            .locator
            .certified_source_revision_digest()
            .unwrap(),
        coordinate,
        *digest_record.document.locator.record_digest(),
    );
    let failure = LingmaSourceBackedResolverV0::new(&digest_inventory)
        .unwrap()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);
    let request = request_with_locator_evidence(
        digest_record,
        *digest_record
            .document
            .locator
            .certified_source_revision_digest()
            .unwrap(),
        digest_record.document.locator.coordinate().clone(),
        [0xa5; 32],
    );
    let failure = LingmaSourceBackedResolverV0::new(&digest_inventory)
        .unwrap()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);

    let native_path = temp.path().join("native-key.db");
    let connection = create_database(&native_path);
    insert_row(
        &connection,
        "native-session",
        "native-request",
        "native prompt",
        None,
    );
    drop(connection);
    let native_source = database(&native_path, "vscode:stable:native-key");
    let native_inventory = inventory(vec![native_source.clone()]);
    let scan =
        scan_lingma_source_backed_v0(native_inventory.clone(), || Ok(native_inventory.clone()))
            .unwrap();
    let native_record = all_records(&scan)[0];
    let connection = Connection::open(&native_path).unwrap();
    insert_row(
        &connection,
        "native-session",
        "native-request",
        "duplicate native prompt",
        None,
    );
    drop(connection);
    let request = request_with_locator_evidence(
        native_record,
        current_source_revision(&native_source),
        native_record.document.locator.coordinate().clone(),
        *native_record.document.locator.record_digest(),
    );
    let failure = LingmaSourceBackedResolverV0::new(&native_inventory)
        .unwrap()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);
}

#[test]
fn source_backed_hydration_distinguishes_missing_row_deletion_and_unavailable_root() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let missing_path = temp.path().join("missing-row.db");
    let connection = create_database(&missing_path);
    insert_row(
        &connection,
        "missing-session",
        "missing-request",
        "missing prompt",
        None,
    );
    drop(connection);
    let missing_source = database(&missing_path, "vscode:stable:missing-row");
    let missing_inventory = inventory(vec![missing_source.clone()]);
    let scan =
        scan_lingma_source_backed_v0(missing_inventory.clone(), || Ok(missing_inventory.clone()))
            .unwrap();
    let record = all_records(&scan)
        .into_iter()
        .find(|record| record.document.role.as_deref() == Some("user"))
        .unwrap();
    Connection::open(&missing_path)
        .unwrap()
        .execute("delete from chat_record", [])
        .unwrap();
    let request = request_with_locator_evidence(
        record,
        current_source_revision(&missing_source),
        record.document.locator.coordinate().clone(),
        *record.document.locator.record_digest(),
    );
    let failure = LingmaSourceBackedResolverV0::new(&missing_inventory)
        .unwrap()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::MissingRecord);

    let deleted_path = temp.path().join("deleted.db");
    let connection = create_database(&deleted_path);
    insert_row(
        &connection,
        "deleted-session",
        "deleted-request",
        "deleted prompt",
        None,
    );
    drop(connection);
    let deleted_inventory = inventory(vec![database(&deleted_path, "vscode:stable:deleted")]);
    let scan =
        scan_lingma_source_backed_v0(deleted_inventory.clone(), || Ok(deleted_inventory.clone()))
            .unwrap();
    let request = event_request(all_records(&scan)[0]);
    std::fs::remove_file(&deleted_path).unwrap();
    let failure = LingmaSourceBackedResolverV0::new(&deleted_inventory)
        .unwrap()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::ConfirmedDeleted);

    let available_root = temp.path().join("available-root");
    std::fs::create_dir(&available_root).unwrap();
    let unavailable_path = available_root.join("local.db");
    let connection = create_database(&unavailable_path);
    insert_row(
        &connection,
        "offline-session",
        "offline-request",
        "offline prompt",
        None,
    );
    drop(connection);
    let unavailable_inventory = inventory(vec![database(
        &unavailable_path,
        "jetbrains:idea:unavailable-root",
    )]);
    let scan = scan_lingma_source_backed_v0(unavailable_inventory.clone(), || {
        Ok(unavailable_inventory.clone())
    })
    .unwrap();
    let request = event_request(all_records(&scan)[0]);
    std::fs::rename(&available_root, temp.path().join("offline-root")).unwrap();
    let failure = LingmaSourceBackedResolverV0::new(&unavailable_inventory)
        .unwrap()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::TemporarilyUnavailable);
}

#[test]
fn source_backed_hydration_types_malformed_row_and_unsupported_schema() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let malformed_path = temp.path().join("malformed.db");
    let connection = create_database(&malformed_path);
    insert_row(
        &connection,
        "malformed-session",
        "malformed-request",
        "valid prompt",
        None,
    );
    drop(connection);
    let malformed_source = database(&malformed_path, "vscode:stable:malformed");
    let malformed_inventory = inventory(vec![malformed_source.clone()]);
    let scan = scan_lingma_source_backed_v0(malformed_inventory.clone(), || {
        Ok(malformed_inventory.clone())
    })
    .unwrap();
    let record = all_records(&scan)[0];
    Connection::open(&malformed_path)
        .unwrap()
        .execute(
            "update chat_record set chat_prompt = cast(x'80' as text)",
            [],
        )
        .unwrap();
    let request = request_with_locator_evidence(
        record,
        current_source_revision(&malformed_source),
        record.document.locator.coordinate().clone(),
        *record.document.locator.record_digest(),
    );
    let failure = LingmaSourceBackedResolverV0::new(&malformed_inventory)
        .unwrap()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);

    let unsupported_path = temp.path().join("unsupported.db");
    let connection = create_database(&unsupported_path);
    insert_row(
        &connection,
        "unsupported-session",
        "unsupported-request",
        "valid prompt",
        None,
    );
    drop(connection);
    let unsupported_inventory = inventory(vec![database(
        &unsupported_path,
        "jetbrains:idea:unsupported",
    )]);
    let scan = scan_lingma_source_backed_v0(unsupported_inventory.clone(), || {
        Ok(unsupported_inventory.clone())
    })
    .unwrap();
    let request = event_request(all_records(&scan)[0]);
    Connection::open(&unsupported_path)
        .unwrap()
        .execute_batch("drop table chat_record;")
        .unwrap();
    let failure = LingmaSourceBackedResolverV0::new(&unsupported_inventory)
        .unwrap()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(
        failure.kind,
        HydrationFailureKind::UnsupportedParserRevision
    );
}

#[test]
fn source_backed_hydration_rejects_malformed_coordinate_and_forbidden_fallbacks() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let connection = create_database(&path);
    insert_row(
        &connection,
        "invalid-session",
        "invalid-request",
        "invalid prompt",
        None,
    );
    drop(connection);
    let inventory = inventory(vec![database(&path, "vscode:stable:invalid")]);
    let scan = scan_lingma_source_backed_v0(inventory.clone(), || Ok(inventory.clone())).unwrap();
    let record = all_records(&scan)[0];
    let malformed_coordinate = NativeRecordCoordinate::ProviderSqlite {
        logical_relation: LOGICAL_RELATION.to_owned(),
        primary_key: TypedKey::I64(1),
        row_version: Some(TypedKey::bytes(vec![0; 32]).unwrap()),
    };
    let request = request_with_locator_evidence(
        record,
        *record
            .document
            .locator
            .certified_source_revision_digest()
            .unwrap(),
        malformed_coordinate,
        *record.document.locator.record_digest(),
    );
    let failure = LingmaSourceBackedResolverV0::new(&inventory)
        .unwrap()
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
            "Lingma source-backed path contains forbidden fallback {forbidden}"
        );
    }
    let route_source =
        include_str!("../../../../source_backed/registration/families/sqlite/inventory.rs");
    let route = route_source
        .split_once("pub fn register_lingma_source_backed_route")
        .unwrap()
        .1
        .split_once("fn discovered_lingma_inventory_source")
        .unwrap()
        .0;
    assert!(route.contains("with_batch_hydration"));
    assert!(route.contains("LingmaSourceBackedResolverV0"));
    assert!(!route.contains(&["work", ".sqlite"].concat()));
    assert!(!route.contains(&["ctx_history_", "store"].concat()));
}

use std::{
    cell::Cell,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES;
use rusqlite::{config::DbConfig, params, Connection};
use serde_json::{json, Value};

use super::*;

fn create_test_database(root: &Path, rows: &[(&str, i64, &str)]) -> PathBuf {
    let database = root
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let conn = Connection::open(&database).unwrap();
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
    for (id, updated_at, messages_json) in rows {
        conn.execute(
            "insert into chat_sessions
             (id, name, created_at, updated_at, messages_json, metadata_json)
             values (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                format!("{id} title"),
                updated_at - 1,
                updated_at,
                messages_json,
                "{}"
            ],
        )
        .unwrap();
    }
    drop(conn);
    database
}

fn replace_messages(database: &Path, id: &str, updated_at: i64, messages: Value) {
    let conn = Connection::open(database).unwrap();
    conn.execute(
        "update chat_sessions set updated_at = ?1, messages_json = ?2 where id = ?3",
        params![updated_at, messages.to_string(), id],
    )
    .unwrap();
}

fn drain_source_backed_plan(
    plan: FirebenderSourceBackedPlan,
) -> (
    ctx_history_core::CertifiedSource,
    Vec<ctx_history_index::LexicalDocument>,
    Vec<usize>,
) {
    let FirebenderSourceBackedPlan::Replacement(mut scanner) = plan else {
        panic!("expected a replacement scan");
    };
    let mut documents = Vec::new();
    let mut page_sizes = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        assert!(page.retained_bytes() <= NATIVE_PATH_MAX_RETAINED_PAGE_BYTES);
        page_sizes.push(page.documents().len());
        documents.extend(page.into_documents());
    }
    let certificate = scanner.finish().unwrap();
    (certificate, documents, page_sizes)
}

#[test]
fn stock_snapshot_queries_active_wal_without_persistent_writes_and_rejects_swap() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source = temp.path().join("firebender.sqlite");
    let attacker = temp.path().join("attacker.sqlite");
    let admitted = temp.path().join("admitted.sqlite");
    create_database(&source, "main");
    create_database(&attacker, "attacker");
    persist_wal_row(&source, "from-wal");
    let before_read = persistent_directory_snapshot(temp.path());

    let (database, opened_value) = FirebenderSqliteDatabase::open(&source, read_latest).unwrap();
    assert_eq!(opened_value, "from-wal");
    assert!(database.evidence().wal_length().is_some());
    assert!(database.evidence().shared_memory_length().is_some());
    assert_eq!(database.read(&source, read_latest).unwrap(), "from-wal");
    assert_eq!(persistent_directory_snapshot(temp.path()), before_read);

    fs::rename(&source, &admitted).unwrap();
    fs::rename(&attacker, &source).unwrap();
    let before_rejected_read = persistent_directory_snapshot(temp.path());
    let queried = Cell::new(false);
    let result = database.read(&source, |_| -> crate::Result<()> {
        queried.set(true);
        Ok(())
    });
    assert!(result.is_err());
    assert!(!queried.get());
    assert_eq!(
        persistent_directory_snapshot(temp.path()),
        before_rejected_read
    );
}

#[test]
fn firebender_source_backed_cold_and_exact_keep_full_policy_body_and_hydrate() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let complete_text = format!("firebender-head-{}-firebender-tail", "x".repeat(3_000));
    let messages = (0..61)
        .map(|index| {
            if index == 0 {
                json!({
                    "role": "user",
                    "content": complete_text,
                })
            } else {
                json!({
                    "role": "assistant",
                    "content": format!("bounded Firebender message {index}"),
                })
            }
        })
        .collect::<Vec<_>>();
    let database = create_test_database(
        &root,
        &[("stable-session", 10, &Value::Array(messages).to_string())],
    );

    let cold = prepare_firebender_source_backed(&root, None).unwrap();
    let (certificate, documents, page_sizes) = drain_source_backed_plan(cold);
    assert_eq!(page_sizes, vec![60, 1]);
    assert_eq!(documents.len(), 61);
    assert_eq!(certificate.counts().complete_records, 61);
    assert_eq!(certificate.counts().retained_records, 61);
    assert_eq!(certificate.counts().indexed_documents, 61);
    assert_eq!(certificate.counts().rejected_records, 0);
    assert_eq!(certificate.counts().ignored_records, 0);
    assert!(certificate.counts().certified_bytes > 0);
    assert!(documents.iter().all(|document| {
        &document.source == certificate.observation().source()
            && document.event_id.source_digest()
                == certificate.observation().source().identity().digest()
    }));

    let first = &documents[0];
    assert_eq!(first.body, complete_text);
    assert!(first.body.ends_with("firebender-tail"));
    assert_eq!(first.parent_session_id, None);
    assert_eq!(first.root_session_id, first.session_id);
    assert_eq!(first.provider_session_id.as_deref(), Some("stable-session"));
    assert_eq!(first.branch, None);
    assert_eq!(
        first.source_path.as_deref().map(Path::new),
        Some(database.canonicalize().unwrap().as_path())
    );
    assert_eq!(first.agent_type, "primary");
    assert!(first.is_primary);
    assert_eq!(
        first.workspace.as_deref(),
        Some(root.canonicalize().unwrap().to_str().unwrap())
    );
    assert_eq!(first.cwd, None);
    let ctx_history_core::NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key: ctx_history_core::TypedKey::I64(rowid),
        row_version: Some(ctx_history_core::TypedKey::Composite(version)),
    } = first.locator.coordinate()
    else {
        panic!("expected a typed Firebender SQLite locator");
    };
    assert_eq!(logical_relation, "chat_sessions.messages_json");
    assert_eq!(*rowid, 1);
    assert_eq!(
        version,
        &vec![
            ctx_history_core::TypedKey::Utf8("stable-session".to_owned()),
            ctx_history_core::TypedKey::I64(10),
            ctx_history_core::TypedKey::U64(0),
        ]
    );
    assert!(first.locator.certified_source_revision_digest().is_some());
    assert!(documents
        .iter()
        .all(|document| document.locator.record_digest() == first.locator.record_digest()));

    let hydrated = hydrate_firebender_source_backed_row(&root, &first.locator).unwrap();
    assert_eq!(hydrated.provider_session_id(), "stable-session");
    assert_eq!(hydrated.message_index(), 0);
    let hydrated_messages: Value = serde_json::from_slice(hydrated.messages_json()).unwrap();
    assert_eq!(
        hydrated_messages.pointer("/0/role").and_then(Value::as_str),
        Some("user")
    );
    assert_eq!(
        hydrated_messages
            .pointer("/0/content")
            .and_then(Value::as_str),
        Some(complete_text.as_str())
    );

    let exact = prepare_firebender_source_backed(&root, Some(&certificate)).unwrap();
    let FirebenderSourceBackedPlan::Exact(exact_certificate) = exact else {
        panic!("unchanged Firebender snapshot was reparsed");
    };
    assert_eq!(exact_certificate, certificate);
}

#[test]
fn firebender_source_backed_replacement_keeps_ids_and_stales_old_row_evidence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let database = create_test_database(
        &root,
        &[(
            "stable-session",
            10,
            r#"[
                {"role":"user","content":"original"},
                {"role":"assistant","content":"answer"}
            ]"#,
        )],
    );
    let (before, before_documents, _) =
        drain_source_backed_plan(prepare_firebender_source_backed(&root, None).unwrap());
    let old_locator = before_documents[0].locator.clone();
    let old_ids = before_documents
        .iter()
        .map(|document| document.event_id)
        .collect::<Vec<_>>();

    replace_messages(
        &database,
        "stable-session",
        20,
        json!([
            {"role": "user", "content": "replacement"},
            {"role": "assistant", "content": "answer"}
        ]),
    );
    let replacement =
        prepare_firebender_source_backed(&root, Some(&before)).expect("replacement plan");
    let (after, after_documents, _) = drain_source_backed_plan(replacement);
    assert_ne!(after.observation(), before.observation());
    assert_eq!(
        after_documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>(),
        old_ids
    );
    assert_eq!(after_documents[0].body, "replacement");
    assert_ne!(
        after_documents[0].locator.record_digest(),
        old_locator.record_digest()
    );
    assert!(matches!(
        hydrate_firebender_source_backed_row(&root, &old_locator),
        Err(FirebenderSourceBackedError::StaleSourceEvidence)
    ));
    assert_eq!(
        hydrate_firebender_source_backed_row(&root, &after_documents[0].locator)
            .unwrap()
            .provider_session_id(),
        "stable-session"
    );
}

#[test]
fn firebender_source_backed_does_not_create_automatic_leaf_discovery() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    create_test_database(
        &root,
        &[(
            "explicit-only",
            10,
            r#"[{"role":"user","content":"explicit"}]"#,
        )],
    );

    let report = crate::provider_sources::discover_provider_sources_for_provider_report(
        temp.path(),
        CaptureProvider::Firebender,
    );
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        crate::provider_sources::DiscoveryIssueKind::InsufficientOfficialEvidence
    );
}

fn create_database(path: &Path, value: &str) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
        .unwrap();
    connection
        .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
        .unwrap();
}

fn persist_wal_row(path: &Path, value: &str) {
    let writer = Connection::open(path).unwrap();
    let mode: String = writer
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer
        .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
        .unwrap();
    writer
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    drop(writer);
    assert!(path.with_file_name("firebender.sqlite-wal").exists());
    assert!(path.with_file_name("firebender.sqlite-shm").exists());
}

fn read_latest(connection: &Connection) -> crate::Result<String> {
    Ok(connection.query_row(
        "SELECT body FROM messages ORDER BY rowid DESC LIMIT 1",
        [],
        |row| row.get(0),
    )?)
}

fn persistent_directory_snapshot(directory: &Path) -> Vec<(OsString, Vec<u8>)> {
    let mut paths = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-shm")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            (
                path.file_name().unwrap().to_os_string(),
                fs::read(path).unwrap(),
            )
        })
        .collect()
}

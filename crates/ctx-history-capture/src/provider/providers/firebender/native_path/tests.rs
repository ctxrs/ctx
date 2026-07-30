use std::{
    cell::Cell,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Barrier, Mutex,
    },
    thread,
};

use ctx_history_core::{BatchHydrationRequest, EventHydrationRequest, HydrationFailureKind};
use ctx_history_index::WriterOptions;
use rusqlite::{config::DbConfig, params, Connection};
use serde_json::{json, Value};

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    provider_sources::provider_source_for_path,
};

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
        assert!(page.retained_bytes() <= FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES);
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
    assert_eq!(page_sizes, vec![61]);
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
    assert_eq!(
        first.locator.revision_policy(),
        ctx_history_core::LocatorRevisionPolicy::StableRecordEvidence
    );
    assert!(first.locator.certified_source_revision_digest().is_none());
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
        Err(FirebenderSourceBackedError::StaleRowEvidence)
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

#[test]
fn direct_scan_is_one_decode_hash_projection_pass_with_64_document_pages() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let messages = (0..130)
        .map(|index| {
            json!({
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": format!("direct Firebender message {index}")
            })
        })
        .collect::<Vec<_>>();
    create_test_database(
        &root,
        &[("one-pass", 10, &Value::Array(messages).to_string())],
    );

    let mut documents = Vec::new();
    let mut page_lengths = Vec::new();
    let scan = source_backed::scan_for_test(&root, &mut |page| {
        page_lengths.push(page.len());
        documents.extend(page);
        Ok(())
    })
    .unwrap();

    assert_eq!(page_lengths, vec![64, 64, 2]);
    assert_eq!(documents.len(), 130);
    assert_eq!(scan.work_counters(), (1, 1, 3, 64));
    assert_eq!(scan.certificate().counts().complete_records, 130);
    assert_eq!(scan.certificate().counts().indexed_documents, 130);
    assert!(scan.certificate().frontier().is_none());
    assert!(documents.iter().all(|document| {
        document.locator.revision_policy()
            == ctx_history_core::LocatorRevisionPolicy::StableRecordEvidence
            && document
                .locator
                .certified_source_revision_digest()
                .is_none()
    }));
}

#[test]
fn direct_route_logical_noop_survives_wal_only_physical_churn() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let database = create_test_database(
        &root,
        &[(
            "logical-noop",
            10,
            r#"[{"role":"user","content":"logical body"}]"#,
        )],
    );
    let index = temp.path().join("index");
    let source = provider_source_for_path(CaptureProvider::Firebender, root.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
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

    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute(
            "update chat_sessions set metadata_json = metadata_json where id = 'logical-noop'",
            [],
        )
        .unwrap();
    let replay = refresh_source_backed_generation(&index, &registry, options).unwrap();

    assert_eq!(replay.sources, cold.sources);
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    assert_eq!(replay.commit.opstamp, cold.commit.opstamp);
    assert!(replay.removals.is_empty());
    drop(writer);
}

#[test]
fn direct_route_certifies_authoritative_deletion_and_rejects_unavailable_shape() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let database = create_test_database(
        &root,
        &[("deleted", 10, r#"[{"role":"user","content":"delete me"}]"#)],
    );
    let index = temp.path().join("index");
    let source = provider_source_for_path(CaptureProvider::Firebender, root.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();

    fs::remove_file(&database).unwrap();
    let deleted = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert!(deleted.sources.is_empty());
    assert_eq!(deleted.removals.len(), 1);

    fs::create_dir(&database).unwrap();
    let unavailable = refresh_source_backed_generation(&index, &registry, options);
    assert!(unavailable.is_err());
}

#[test]
fn direct_route_never_deletes_when_the_provider_parent_is_missing() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let database = create_test_database(
        &root,
        &[("retained", 10, r#"[{"role":"user","content":"keep me"}]"#)],
    );
    let index = temp.path().join("index");
    let source = provider_source_for_path(CaptureProvider::Firebender, root.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
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

    fs::remove_file(&database).unwrap();
    fs::remove_dir(database.parent().unwrap()).unwrap();
    assert!(refresh_source_backed_generation(&index, &registry, options.clone()).is_err());
    assert!(matches!(
        source_backed::scan_for_test(&root, &mut |_| Ok(())),
        Err(FirebenderSourceBackedError::Capture(CaptureError::Io(error)))
            if error.kind() == std::io::ErrorKind::NotFound
    ));

    create_test_database(
        &root,
        &[("retained", 10, r#"[{"role":"user","content":"keep me"}]"#)],
    );
    let replay = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_eq!(replay.sources, cold.sources);
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    assert!(replay.removals.is_empty());
}

#[test]
fn missing_leaf_fence_rejects_parent_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let database = create_test_database(
        &root,
        &[("moving", 10, r#"[{"role":"user","content":"moving"}]"#)],
    );
    let parent = database.parent().unwrap().to_path_buf();
    let admitted = parent.with_file_name("firebender-admitted");
    fs::remove_file(&database).unwrap();

    let still_missing = source_backed::revalidate_missing_after_for_test(&root, || {
        fs::rename(&parent, &admitted).unwrap();
        fs::create_dir(&parent).unwrap();
    })
    .unwrap();

    assert!(!still_missing);
}

#[test]
fn direct_route_never_deletes_for_an_unsafe_leaf() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let database = create_test_database(
        &root,
        &[("safe", 10, r#"[{"role":"user","content":"safe"}]"#)],
    );
    let index = temp.path().join("index");
    let source = provider_source_for_path(CaptureProvider::Firebender, root.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
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

    fs::remove_file(&database).unwrap();
    fs::create_dir(&database).unwrap();
    assert!(refresh_source_backed_generation(&index, &registry, options.clone()).is_err());
    assert!(matches!(
        source_backed::scan_for_test(&root, &mut |_| Ok(())),
        Err(FirebenderSourceBackedError::Capture(
            CaptureError::InvalidProviderTranscriptPath { .. }
        ))
    ));
    fs::remove_dir(&database).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = temp.path().join("outside.sqlite");
        fs::write(&outside, b"not admitted").unwrap();
        symlink(&outside, &database).unwrap();
        assert!(refresh_source_backed_generation(&index, &registry, options.clone()).is_err());
        assert!(matches!(
            source_backed::scan_for_test(&root, &mut |_| Ok(())),
            Err(FirebenderSourceBackedError::Capture(
                CaptureError::InvalidProviderTranscriptPath { .. }
            ))
        ));
        fs::remove_file(&database).unwrap();
    }

    create_test_database(
        &root,
        &[("safe", 10, r#"[{"role":"user","content":"safe"}]"#)],
    );
    let replay = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_eq!(replay.sources, cold.sources);
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    assert!(replay.removals.is_empty());
}

#[cfg(unix)]
#[test]
fn direct_route_never_deletes_for_a_permission_denied_leaf() {
    use std::os::unix::fs::PermissionsExt;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let database = create_test_database(
        &root,
        &[(
            "protected",
            10,
            r#"[{"role":"user","content":"protected"}]"#,
        )],
    );
    let index = temp.path().join("index");
    let source = provider_source_for_path(CaptureProvider::Firebender, root.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
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

    let original_permissions = fs::metadata(&database).unwrap().permissions();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o0)).unwrap();
    assert!(refresh_source_backed_generation(&index, &registry, options.clone()).is_err());
    assert!(matches!(
        source_backed::scan_for_test(&root, &mut |_| Ok(())),
        Err(FirebenderSourceBackedError::Capture(CaptureError::Io(error)))
            if error.kind() == std::io::ErrorKind::PermissionDenied
    ));
    fs::set_permissions(&database, original_permissions).unwrap();

    let replay = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_eq!(replay.sources, cold.sources);
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    assert!(replay.removals.is_empty());
}

#[test]
fn direct_scan_fails_when_a_wal_writer_mutates_after_the_first_page() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let messages = (0..65)
        .map(|index| json!({"role":"user","content":format!("message {index}")}))
        .collect::<Vec<_>>();
    let database = create_test_database(
        &root,
        &[("moving", 10, &Value::Array(messages).to_string())],
    );
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let mut mutated = false;

    let result = source_backed::scan_for_test(&root, &mut |_| {
        if !mutated {
            writer
                .execute(
                    "update chat_sessions set updated_at = 20 where id = 'moving'",
                    [],
                )
                .unwrap();
            mutated = true;
        }
        Ok(())
    });

    assert!(mutated);
    assert!(result.is_err());
    drop(writer);
}

#[test]
fn exact_single_and_grouped_batch_hydration_use_one_snapshot_and_native_row_reads() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    create_test_database(
        &root,
        &[
            (
                "first",
                10,
                r#"[
                    {"role":"user","content":"first zero"},
                    {"role":"assistant","content":"first one"}
                ]"#,
            ),
            ("second", 20, r#"[{"role":"user","content":"second zero"}]"#),
        ],
    );
    let mut documents = Vec::new();
    source_backed::scan_for_test(&root, &mut |page| {
        documents.extend(page);
        Ok(())
    })
    .unwrap();
    let requests = [2_usize, 0, 1]
        .into_iter()
        .map(|index| {
            EventHydrationRequest::new(documents[index].event_id, documents[index].locator.clone())
                .unwrap()
        })
        .collect::<Vec<_>>();

    let single = source_backed::resolver_for_test(&root);
    let hydrated = single.hydrate_event(&requests[0]).unwrap();
    assert_eq!(hydrated.provider_bytes, b"second zero");
    assert_eq!(single.counters(), (1, 1));

    let batch = source_backed::resolver_for_test(&root);
    let request = BatchHydrationRequest::new(requests.clone()).unwrap();
    let result = batch.hydrate_batch(&request).unwrap();
    assert_eq!(
        result
            .records()
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![
            b"second zero".as_slice(),
            b"first zero".as_slice(),
            b"first one".as_slice()
        ]
    );
    assert_eq!(batch.counters(), (1, 2));

    replace_messages(
        &root.join(".idea/firebender/chat_history.db"),
        "first",
        30,
        json!([
            {"role":"user","content":"changed"},
            {"role":"assistant","content":"first one"}
        ]),
    );
    let stale = source_backed::resolver_for_test(&root)
        .hydrate_batch(&BatchHydrationRequest::new(requests).unwrap())
        .unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);
}

#[test]
fn exact_hydration_confirms_only_a_missing_leaf_under_a_live_parent() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let database = create_test_database(
        &root,
        &[(
            "hydration",
            10,
            r#"[{"role":"user","content":"hydrate me"}]"#,
        )],
    );
    let mut documents = Vec::new();
    source_backed::scan_for_test(&root, &mut |page| {
        documents.extend(page);
        Ok(())
    })
    .unwrap();
    let request =
        EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone()).unwrap();

    fs::remove_file(&database).unwrap();
    let deleted = source_backed::resolver_for_test(&root)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(deleted.kind, HydrationFailureKind::ConfirmedDeleted);

    fs::remove_dir(database.parent().unwrap()).unwrap();
    let unavailable = source_backed::resolver_for_test(&root)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(
        unavailable.kind,
        HydrationFailureKind::TemporarilyUnavailable
    );
}

#[test]
fn terminal_fence_is_evaluated_once_and_false_is_cached() {
    for verdict in [true, false] {
        let cached = Arc::new(Mutex::new(None));
        let calls = Arc::new(AtomicU64::new(0));
        let barrier = Arc::new(Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let cached = Arc::clone(&cached);
                let calls = Arc::clone(&calls);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let mut cached = cached.lock().unwrap();
                    source_backed::cache_terminal_fence_result(&mut cached, || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        verdict
                    })
                })
            })
            .collect::<Vec<_>>();
        let results = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(results, vec![verdict; 8]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn provider_local_route_is_direct_replacement_only_and_batch_hydrated() {
    let route = include_str!("source_backed/direct.rs");
    assert!(route.contains("SourceBackedRouteDriver::new("));
    assert!(route.contains(".with_batch_hydration("));
    assert!(route.contains("certify_complete_inventory("));
    assert!(route.contains("DIRECT_PAGE_DOCUMENTS: usize = 64"));
    assert!(!route.contains(concat!("captured_route_", "driver")));
    assert!(!route.contains(concat!("begin_source_", "append")));
    assert!(!route.contains(concat!("certify_source_", "append")));
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

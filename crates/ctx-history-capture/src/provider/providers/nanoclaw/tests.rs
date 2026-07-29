use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{CaptureProvider, LocatorRevisionPolicy, NativeRecordCoordinate, TypedKey};
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use super::native_path::source_backed::{
    hydrate_nanoclaw_source_backed_exact, scan_nanoclaw_source_backed,
    set_before_source_backed_finish_hook,
};
use super::position::{nanoclaw_message_locator, NanoClawMessageSource};
use super::project::set_before_central_guard_open_hook;
use super::*;
use crate::complete_content::{
    source_access::set_nanoclaw_before_source_set_revalidation,
    sqlite::CompleteContentSqliteQueryBudget, AuthorizedSourceRoute, CompleteContentErrorKind,
    CompleteContentSourceFamily, CompleteContentSourceLocator, SourceAccessBroker, SourceSnapshot,
};
use crate::{NANOCLAW_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS};

fn create_project(temp: &TempDir, name: &str, sessions: usize) -> PathBuf {
    let root = temp.path().join(name);
    let data = root.join("data");
    fs::create_dir_all(data.join("v2-sessions")).unwrap();
    let central = Connection::open(data.join("v2.db")).unwrap();
    central
        .execute_batch(
            "create table agent_groups (
                id text primary key, name text, folder text, agent_provider text
            );
            create table messaging_groups (
                id text primary key, channel_type text, platform_id text,
                instance text, name text
            );
            create table sessions (
                id text primary key, agent_group_id text not null,
                messaging_group_id text, thread_id text, agent_provider text,
                status text, container_status text, last_active integer,
                created_at integer
            );
            insert into agent_groups values (
                'ag-1', 'Personal', '/workspace/nanoclaw', 'codex'
            );
            insert into messaging_groups values (
                'mg-1', 'telegram', 'chat-1', 'default', 'DM'
            );",
        )
        .unwrap();
    for index in 0..sessions {
        central
            .execute(
                "insert into sessions values (
                    ?1, 'ag-1', 'mg-1', ?2, 'codex', 'active', 'running',
                    ?3, ?4
                )",
                rusqlite::params![
                    format!("session-{index:04}"),
                    format!("thread-{index:04}"),
                    1_782_259_202_000_i64 + index as i64,
                    1_782_259_200_000_i64 + index as i64,
                ],
            )
            .unwrap();
    }
    root
}

fn create_message_stores(root: &Path, session_id: &str) -> (PathBuf, PathBuf) {
    let session_dir = root
        .join("data")
        .join("v2-sessions")
        .join("ag-1")
        .join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    let inbound_path = session_dir.join("inbound.db");
    let inbound = Connection::open(&inbound_path).unwrap();
    inbound
        .execute_batch(
            "create table messages_in (
                id text primary key, seq integer, kind text, timestamp integer,
                status text, trigger text, platform_id text, channel_type text,
                thread_id text, content text, source_session_id text, on_wake integer
            );",
        )
        .unwrap();
    let outbound_path = session_dir.join("outbound.db");
    let outbound = Connection::open(&outbound_path).unwrap();
    outbound
        .execute_batch(
            "create table messages_out (
                id text primary key, seq integer, in_reply_to text, timestamp integer,
                kind text, platform_id text, channel_type text, thread_id text,
                content text
            );",
        )
        .unwrap();
    (inbound_path, outbound_path)
}

fn insert_inbound(path: &Path, id: &str, seq: i64, timestamp: i64, content: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "insert into messages_in values (
                ?1, ?2, 'chat', ?3, 'done', 'message', 'chat-1', 'telegram',
                'thread', ?4, null, 0
            )",
            rusqlite::params![id, seq, timestamp, content],
        )
        .unwrap();
}

fn insert_outbound(path: &Path, id: &str, seq: i64, timestamp: i64, content: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "insert into messages_out values (
                ?1, ?2, null, ?3, 'chat', 'chat-1', 'telegram', 'thread', ?4
            )",
            rusqlite::params![id, seq, timestamp, content],
        )
        .unwrap();
}

fn sqlite_persistent_disk_state(
    databases: &[&Path],
) -> Vec<(PathBuf, Option<(Vec<u8>, u64, std::time::SystemTime)>)> {
    let mut state = Vec::new();
    for database in databases {
        // Stock WAL readers may update volatile SHM reader marks.
        for suffix in ["", "-wal", "-journal"] {
            let path = if suffix.is_empty() {
                database.to_path_buf()
            } else {
                let mut value = database.as_os_str().to_os_string();
                value.push(suffix);
                PathBuf::from(value)
            };
            let contents = match fs::read(&path) {
                Ok(contents) => {
                    let metadata = fs::metadata(&path).unwrap();
                    Some((contents, metadata.len(), metadata.modified().unwrap()))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("failed to read {}: {error}", path.display()),
            };
            state.push((path, contents));
        }
    }
    state
}
#[test]
fn compound_locator_recovers_exact_inbound_and_outbound_content_without_paths() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "locator", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "exact-inbound-content");
    insert_outbound(&outbound, "outbound", 2, 2_000, "exact-outbound-content");
    let inbound_locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let outbound_locator = nanoclaw_message_locator(1, NanoClawMessageSource::Outbound, 1).unwrap();
    let project = NanoClawCompleteProject::open(
        &root,
        &[inbound_locator.clone(), outbound_locator.clone()],
        CompleteContentSqliteQueryBudget::new(),
    )
    .unwrap();
    assert_eq!(
        project.resolve(&inbound_locator).unwrap().unwrap().text,
        "exact-inbound-content"
    );
    assert_eq!(
        project.resolve(&outbound_locator).unwrap().unwrap().text,
        "exact-outbound-content"
    );

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'mutated-content' where id = 'inbound'",
            [],
        )
        .unwrap();
    assert!(project.resolve(&inbound_locator).is_err());
}

#[test]
fn source_backed_route_has_no_legacy_store_publication_fallback() {
    let module_source = include_str!("../nanoclaw.rs");
    let native_path_source = include_str!("native_path.rs");
    let source_backed_source = include_str!("native_path/source_backed.rs");
    let scanner_source = include_str!("source.rs");
    let rows_source = include_str!("rows.rs");

    assert!(!native_path_source.contains("mod lifecycle;"));
    assert!(!native_path_source.contains("mod publication;"));
    assert!(!native_path_source.contains("mod scanner;"));
    for source in [source_backed_source, scanner_source, rows_source] {
        assert!(!source.contains("ctx_history_store"));
        assert!(!source.contains("EventSearchBulkGuard"));
        assert!(!source.contains("NativePathPublicationGroup"));
    }
    assert!(!source_backed_source.contains("publication::"));
    assert_eq!(module_source.matches("ctx_history_store::Store").count(), 1);
    assert!(module_source.contains("CaptureError::UnsupportedSchema"));
}

#[test]
fn source_backed_indexes_full_meaningful_body_and_hydrates_the_exact_tail() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "full-source-body", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    let secret = "NANOCLAW_COMPLETE_TAIL_MUST_REMAIN_INDEXED";
    let mut content = "p".repeat(PROVIDER_MAX_TEXT_CHARS + 256);
    content.push_str(secret);
    insert_inbound(&inbound, "full-body", 1, 1_000, &content);

    let lineage = [0x51; 32];
    let mut documents = Vec::new();
    scan_nanoclaw_source_backed(&root, lineage, |page| {
        documents.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].body, content);
    assert!(documents[0].body.ends_with(secret));

    let hydrated =
        hydrate_nanoclaw_source_backed_exact(&root, lineage, &documents[0].locator).unwrap();
    assert_eq!(hydrated.text, content);
}

#[test]
fn source_backed_append_rewrite_truncate_delete_and_unavailable_are_exact() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "source-lifecycle", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "message-1", 1, 1_000, "cold-body");
    let lineage = [0x52; 32];

    let mut cold = Vec::new();
    let cold_receipt = scan_nanoclaw_source_backed(&root, lineage, |page| {
        cold.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(cold.len(), 1);
    let stable_event_id = cold[0].event_id;
    let cold_locator = cold[0].locator.clone();

    insert_inbound(&inbound, "message-2", 2, 2_000, "appended-body");
    let mut appended = Vec::new();
    let append_receipt = scan_nanoclaw_source_backed(&root, lineage, |page| {
        appended.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(appended.len(), 2);
    assert_eq!(appended[0].event_id, stable_event_id);
    assert_ne!(cold_receipt.source, append_receipt.source);
    let appended_locator = appended[1].locator.clone();

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'rewritten-body' where id = 'message-1'",
            [],
        )
        .unwrap();
    let mut rewritten = Vec::new();
    let rewrite_receipt = scan_nanoclaw_source_backed(&root, lineage, |page| {
        rewritten.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(rewritten.len(), 2);
    assert_eq!(rewritten[0].event_id, stable_event_id);
    assert_eq!(rewritten[0].body, "rewritten-body");
    assert_ne!(append_receipt.source, rewrite_receipt.source);
    assert!(hydrate_nanoclaw_source_backed_exact(&root, lineage, &cold_locator).is_err());
    assert_eq!(
        hydrate_nanoclaw_source_backed_exact(&root, lineage, &rewritten[0].locator)
            .unwrap()
            .text,
        "rewritten-body"
    );

    Connection::open(&inbound)
        .unwrap()
        .execute("delete from messages_in where id = 'message-2'", [])
        .unwrap();
    let mut truncated = Vec::new();
    let truncate_receipt = scan_nanoclaw_source_backed(&root, lineage, |page| {
        truncated.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(truncated.len(), 1);
    assert_eq!(truncated[0].event_id, stable_event_id);
    assert_ne!(rewrite_receipt.source, truncate_receipt.source);
    assert!(hydrate_nanoclaw_source_backed_exact(&root, lineage, &appended_locator).is_err());

    Connection::open(root.join("data").join("v2.db"))
        .unwrap()
        .execute("delete from sessions where id = 'session-0000'", [])
        .unwrap();
    let mut after_delete = Vec::new();
    let delete_receipt = scan_nanoclaw_source_backed(&root, lineage, |page| {
        after_delete.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert!(after_delete.is_empty());
    assert_eq!(delete_receipt.source.counts().retained_records, 0);
    assert_ne!(truncate_receipt.source, delete_receipt.source);

    let unavailable = temp.path().join("source-lifecycle-unavailable");
    fs::rename(&root, &unavailable).unwrap();
    let mut emitted = 0;
    assert!(scan_nanoclaw_source_backed(&root, lineage, |_| {
        emitted += 1;
        Ok(())
    })
    .is_err());
    assert_eq!(emitted, 0);
}

#[test]
fn source_backed_cold_scan_has_stable_ids_compound_evidence_and_exact_locators() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "source-backed", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "source-backed-inbound");
    insert_outbound(&outbound, "outbound", 2, 2_000, "source-backed-outbound");
    let lineage = [0x4a; 32];

    let mut documents = Vec::new();
    let receipt = scan_nanoclaw_source_backed(&root, lineage, |page| {
        assert!(!page.documents.is_empty());
        assert!(page.documents.len() <= 64);
        documents.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(documents.len(), 2);
    assert_eq!(receipt.emitted_pages, 1);
    assert_eq!(
        receipt.source.counts(),
        ctx_history_core::ScannedSourceCounts {
            complete_records: 3,
            retained_records: 2,
            rejected_records: 0,
            ignored_records: 1,
            indexed_documents: 2,
            certified_bytes: receipt.source.counts().certified_bytes,
        }
    );
    assert!(receipt.source.counts().certified_bytes > 0);
    let evidence: serde_json::Value =
        serde_json::from_slice(receipt.source.observation().revision()).unwrap();
    assert_eq!(evidence["version"], json!(1));
    assert_eq!(evidence["sessions"], json!(1));
    assert_eq!(evidence["component_databases"], json!(2));
    assert!(evidence["central_sha256"].as_str().unwrap().len() == 64);
    assert!(evidence["session_inventory_sha256"].as_str().unwrap().len() == 64);

    let canonical_root = fs::canonicalize(&root).unwrap().display().to_string();
    for document in &documents {
        assert_eq!(document.parent_session_id, None);
        assert_eq!(document.root_session_id, document.session_id);
        assert_eq!(document.provider_session_id.as_deref(), Some("thread-0000"));
        assert_eq!(document.branch, None);
        assert_eq!(
            document.source_path.as_deref(),
            Some(canonical_root.as_str())
        );
        assert_eq!(document.agent_type, "codex");
        assert!(document.is_primary);
        assert_eq!(
            document.locator.revision_policy(),
            LocatorRevisionPolicy::ExactSourceRevision
        );
        assert!(document
            .locator
            .certified_source_revision_digest()
            .is_some());
        let NativeRecordCoordinate::ProviderNative {
            namespace,
            coordinate,
        } = document.locator.coordinate()
        else {
            panic!("NanoClaw source-backed locator was not provider-native");
        };
        assert_eq!(namespace, NANOCLAW_MESSAGE_LOCATOR_KIND);
        assert!(matches!(coordinate, TypedKey::Bytes(value) if value.len() == 17));
    }

    let exact = documents
        .iter()
        .map(|document| {
            hydrate_nanoclaw_source_backed_exact(&root, lineage, &document.locator)
                .unwrap()
                .text
        })
        .collect::<Vec<_>>();
    assert_eq!(
        exact,
        vec!["source-backed-inbound", "source-backed-outbound"]
    );

    let mut repeated_documents = Vec::new();
    let repeated = scan_nanoclaw_source_backed(&root, lineage, |page| {
        repeated_documents.extend(page.documents);
        Ok(())
    })
    .unwrap();
    assert_eq!(receipt.source, repeated.source);
    assert_eq!(
        documents
            .iter()
            .map(|document| (document.session_id, document.event_id))
            .collect::<Vec<_>>(),
        repeated_documents
            .iter()
            .map(|document| (document.session_id, document.event_id))
            .collect::<Vec<_>>()
    );

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'changed' where id = 'inbound'",
            [],
        )
        .unwrap();
    assert!(hydrate_nanoclaw_source_backed_exact(&root, lineage, &documents[0].locator).is_err());
}

#[test]
fn source_backed_partial_authority_and_unsupported_roots_fail_before_emitting() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "partial-authority", 1);
    let (_, outbound) = create_message_stores(&root, "session-0000");
    let lineage = [0x73; 32];
    let mut emitted = 0;

    let unsupported = root.join("data").join("v2-sessions");
    assert!(scan_nanoclaw_source_backed(&unsupported, lineage, |_| {
        emitted += 1;
        Ok(())
    })
    .is_err());
    assert_eq!(emitted, 0);

    Connection::open(outbound)
        .unwrap()
        .execute_batch("drop table messages_out; create table unrelated (id text);")
        .unwrap();
    let error = scan_nanoclaw_source_backed(&root, lineage, |_| {
        emitted += 1;
        Ok(())
    })
    .unwrap_err();
    assert!(error.to_string().contains("messages_out"));
    assert_eq!(emitted, 0);
}

fn nanoclaw_broker_route(root: &Path) -> AuthorizedSourceRoute {
    AuthorizedSourceRoute {
        source_id: Uuid::new_v4(),
        provider: CaptureProvider::NanoClaw,
        source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
        family: CompleteContentSourceFamily::Sqlite,
        raw_source_path: root.to_path_buf(),
        source_root: root.parent().map(Path::to_path_buf),
        source_identity: Some("nanoclaw-root-safety".to_owned()),
        source_snapshot: SourceSnapshot::default(),
    }
}

fn complete_content_locator(
    locator: &crate::native_source::NativeLocator,
) -> CompleteContentSourceLocator {
    CompleteContentSourceLocator::new(locator.kind(), locator.value().to_vec()).unwrap()
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn source_root_safety_nanoclaw_snapshot_stays_exact_after_live_leaf_rewrite() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "broker-exact", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "inside-nanoclaw-snapshot");
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let source_locator = complete_content_locator(&locator);
    let event_id = Uuid::new_v4();
    let access = SourceAccessBroker::new()
        .admit_for_source_locators(
            nanoclaw_broker_route(&root),
            std::slice::from_ref(&source_locator),
            event_id,
        )
        .unwrap();

    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = 'OUTSIDE_NANOCLAW_MUST_NOT_ESCAPE' where rowid = 1",
            [],
        )
        .unwrap();

    let project = access
        .open_nanoclaw_project(
            std::slice::from_ref(&locator),
            CompleteContentSqliteQueryBudget::new(),
            event_id,
        )
        .unwrap();
    let resolved = project.resolve(&locator).unwrap().unwrap();
    assert_eq!(resolved.text, "inside-nanoclaw-snapshot");
    assert!(!resolved.text.contains("OUTSIDE_NANOCLAW"));
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn source_root_safety_nanoclaw_broker_rejects_concurrent_root_swap() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "broker-root", 1);
    let moved = temp.path().join("moved-broker-root");
    let replacement = create_project(&temp, "replacement-broker-root", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "inside-nanoclaw-root");
    let (replacement_inbound, _) = create_message_stores(&replacement, "session-0000");
    insert_inbound(
        &replacement_inbound,
        "inbound",
        1,
        1_000,
        "OUTSIDE_NANOCLAW_ROOT_MUST_NOT_ESCAPE",
    );
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let source_locator = complete_content_locator(&locator);
    let event_id = Uuid::new_v4();
    let route = nanoclaw_broker_route(&root);
    let _hook = set_nanoclaw_before_source_set_revalidation({
        let root = root.clone();
        move || {
            std::thread::spawn(move || {
                fs::rename(&root, moved).unwrap();
                fs::rename(replacement, root).unwrap();
            })
            .join()
            .unwrap();
        }
    });

    let error = SourceAccessBroker::new()
        .admit_for_source_locators(route, &[source_locator], event_id)
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    assert_eq!(error.event_id, event_id);
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn source_root_safety_nanoclaw_broker_rejects_concurrent_leaf_swap() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "broker-leaf", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "inside-nanoclaw-leaf");
    let moved = inbound.with_extension("moved");
    let replacement = inbound.with_extension("replacement");
    fs::copy(&inbound, &replacement).unwrap();
    Connection::open(&replacement)
        .unwrap()
        .execute(
            "update messages_in set content = 'OUTSIDE_NANOCLAW_LEAF_MUST_NOT_ESCAPE' where rowid = 1",
            [],
        )
        .unwrap();
    let locator = nanoclaw_message_locator(1, NanoClawMessageSource::Inbound, 1).unwrap();
    let source_locator = complete_content_locator(&locator);
    let event_id = Uuid::new_v4();
    let route = nanoclaw_broker_route(&root);
    let _hook = set_nanoclaw_before_source_set_revalidation({
        let inbound = inbound.clone();
        move || {
            std::thread::spawn(move || {
                fs::rename(&inbound, moved).unwrap();
                fs::rename(replacement, inbound).unwrap();
            })
            .join()
            .unwrap();
        }
    });

    let error = SourceAccessBroker::new()
        .admit_for_source_locators(route, &[source_locator], event_id)
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    assert_eq!(error.event_id, event_id);
}

#[test]
fn source_backed_component_mutation_is_rejected_before_any_page_is_emitted() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "finish-mutation", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "pre-finish-content");
    let mutate = inbound.clone();
    let _hook = set_before_source_backed_finish_hook(move || {
        Connection::open(mutate)
            .unwrap()
            .execute(
                "update messages_in set content = 'new-generation' where id = 'inbound'",
                [],
            )
            .unwrap();
    });
    let mut emitted = 0;

    let error = scan_nanoclaw_source_backed(&root, [0x31; 32], |_| {
        emitted += 1;
        Ok(())
    })
    .unwrap_err();

    assert!(error.to_string().contains("changed"), "{error}");
    assert_eq!(emitted, 0);
}

#[test]
fn source_backed_compound_inventory_certifies_central_and_session_sidecars() {
    for family in ["central", "session-component"] {
        for suffix in ["-wal", "-shm", "-journal"] {
            let temp = crate::test_support_paths::tempdir().unwrap();
            let root = create_project(&temp, &format!("{family}-sidecar-{}", &suffix[1..]), 1);
            let (inbound, _) = create_message_stores(&root, "session-0000");
            insert_inbound(&inbound, "inbound", 1, 1_000, "sidecar-inventory");
            let base = if family == "central" {
                root.join("data").join("v2.db")
            } else {
                inbound
            };
            let sidecar = {
                let mut path = base.as_os_str().to_os_string();
                path.push(suffix);
                PathBuf::from(path)
            };
            let _hook = set_before_source_backed_finish_hook(move || {
                fs::write(sidecar, b"new compound generation").unwrap();
            });
            let mut emitted = 0;

            let error = scan_nanoclaw_source_backed(&root, [0x32; 32], |_| {
                emitted += 1;
                Ok(())
            })
            .unwrap_err();

            assert!(
                error.to_string().contains("changed"),
                "{family} {suffix}: {error}"
            );
            assert_eq!(emitted, 0, "{family} {suffix}");
        }
    }
}

#[test]
fn source_backed_selected_root_replacement_has_no_pathname_fallback() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "selected-root", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "selected-original");
    let held = temp.path().join("selected-root-held");
    let replacement = create_project(&temp, "selected-root-replacement", 1);
    let (replacement_inbound, _) = create_message_stores(&replacement, "session-0000");
    insert_inbound(
        &replacement_inbound,
        "replacement",
        1,
        1_000,
        "must-not-emit-replacement",
    );
    let selected = root.clone();
    let _hook = set_before_source_backed_finish_hook(move || {
        fs::rename(&selected, &held).unwrap();
        fs::rename(&replacement, &selected).unwrap();
    });
    let mut emitted = 0;

    let error = scan_nanoclaw_source_backed(&root, [0x33; 32], |_| {
        emitted += 1;
        Ok(())
    })
    .unwrap_err();

    assert!(error.to_string().contains("changed"), "{error}");
    assert_eq!(emitted, 0);
}

#[test]
fn source_backed_central_guard_rejects_root_swap_before_query() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "central-bind", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "bound-original");
    let held = temp.path().join("central-bind-held");
    let replacement = create_project(&temp, "central-bind-replacement", 1);
    let selected = root.clone();
    let _hook = set_before_central_guard_open_hook(move || {
        fs::rename(&selected, &held).unwrap();
        fs::rename(&replacement, &selected).unwrap();
    });
    let mut emitted = 0;

    let error = scan_nanoclaw_source_backed(&root, [0x35; 32], |_| {
        emitted += 1;
        Ok(())
    })
    .unwrap_err();

    assert!(error.to_string().contains("changed"), "{error}");
    assert_eq!(emitted, 0);
}

#[test]
fn source_backed_central_parent_swap_is_rejected_before_publication() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "central-parent", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "original-parent");
    let replacement = create_project(&temp, "central-parent-replacement", 1);
    let (replacement_inbound, _) = create_message_stores(&replacement, "session-0000");
    insert_inbound(
        &replacement_inbound,
        "replacement",
        1,
        1_000,
        "replacement-parent-must-not-emit",
    );
    let selected_data = root.join("data");
    let held_data = temp.path().join("central-parent-held-data");
    let replacement_data = replacement.join("data");
    let _hook = set_before_central_guard_open_hook(move || {
        fs::rename(&selected_data, &held_data).unwrap();
        fs::rename(&replacement_data, &selected_data).unwrap();
    });
    let mut emitted = 0;

    let error = scan_nanoclaw_source_backed(&root, [0x36; 32], |_| {
        emitted += 1;
        Ok(())
    })
    .unwrap_err();

    assert!(error.to_string().contains("changed"), "{error}");
    assert_eq!(emitted, 0);
}

#[test]
fn source_backed_central_leaf_swap_is_rejected_before_publication() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "central-leaf", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "inbound", 1, 1_000, "original-leaf");
    let central = root.join("data").join("v2.db");
    let held = root.join("data").join("v2-original.db");
    let replacement = root.join("data").join("v2-replacement.db");
    fs::copy(&central, &replacement).unwrap();
    Connection::open(&replacement)
        .unwrap()
        .execute(
            "update sessions set thread_id = 'replacement-leaf-must-not-emit'",
            [],
        )
        .unwrap();
    let _hook = set_before_central_guard_open_hook({
        let central = central.clone();
        move || {
            fs::rename(&central, held).unwrap();
            fs::rename(replacement, central).unwrap();
        }
    });
    let mut emitted = 0;

    let error = scan_nanoclaw_source_backed(&root, [0x37; 32], |_| {
        emitted += 1;
        Ok(())
    })
    .unwrap_err();

    assert!(error.to_string().contains("changed"), "{error}");
    assert_eq!(emitted, 0);
}

#[test]
fn source_backed_reads_consistent_central_and_project_wal_without_persistent_writes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "wal-consistency", 0);
    let central_path = root.join("data").join("v2.db");
    let (inbound, outbound) = create_message_stores(&root, "session-wal");
    let central_writer = Connection::open(&central_path).unwrap();
    let central_mode: String = central_writer
        .query_row("pragma journal_mode=wal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(central_mode, "wal");
    central_writer
        .execute(
            "insert into sessions values (
                'session-wal', 'ag-1', 'mg-1', 'thread-wal', 'codex',
                'active', 'running', 1782259202000, 1782259200000
            )",
            [],
        )
        .unwrap();
    central_writer
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();

    let component_writer = Connection::open(&inbound).unwrap();
    let component_mode: String = component_writer
        .query_row("pragma journal_mode=wal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(component_mode, "wal");
    let full_body = format!(
        "{}nanoclaw-tail",
        "central-project-wal-content ".repeat(200)
    );
    component_writer
        .execute(
            "insert into messages_in values (
                'wal-message', 1, 'chat', 1000, 'done', 'message', 'chat-1',
                'telegram', 'thread', ?1, null, 0
            )",
            [full_body.as_str()],
        )
        .unwrap();
    component_writer
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
    let before = sqlite_persistent_disk_state(&[&central_path, &inbound, &outbound]);
    assert!(before.iter().any(|(path, state)| {
        path.as_os_str().to_string_lossy().ends_with("v2.db-wal") && state.is_some()
    }));
    assert!(before.iter().any(|(path, state)| {
        path.as_os_str()
            .to_string_lossy()
            .ends_with("inbound.db-wal")
            && state.is_some()
    }));
    let mut documents = Vec::new();

    let receipt = scan_nanoclaw_source_backed(&root, [0x34; 32], |page| {
        documents.extend(page.documents);
        Ok(())
    })
    .unwrap();

    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].body, full_body);
    assert!(documents[0].body.ends_with("nanoclaw-tail"));
    assert_eq!(receipt.source.counts().retained_records, 1);
    let exact =
        hydrate_nanoclaw_source_backed_exact(&root, [0x34; 32], &documents[0].locator).unwrap();
    assert_eq!(exact.text, documents[0].body);
    let after = sqlite_persistent_disk_state(&[&central_path, &inbound, &outbound]);
    assert_eq!(after, before);
}

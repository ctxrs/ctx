use std::{cell::Cell, fs, path::PathBuf};

#[cfg(target_os = "windows")]
use std::path::Path;

use ctx_history_core::CaptureProvider;
use serde_json::json;
use uuid::Uuid;

use super::{jsonl::finish_jsonl_read, AuthorizedSourceRoute, SourceAccessBroker};
#[cfg(unix)]
use crate::complete_content::CompleteContentBodyDigest;
#[cfg(unix)]
use crate::KIMI_CODE_CLI_SOURCE_FORMAT;
use crate::{
    complete_content::{
        CompleteContentError, CompleteContentErrorKind, CompleteContentSourceFamily, SourceSnapshot,
    },
    CODEX_SESSION_SOURCE_FORMAT, MISTRAL_VIBE_SOURCE_FORMAT, OPENCLAW_SOURCE_FORMAT,
};
#[cfg(unix)]
use rusqlite::Connection;

fn admit_jsonl(
    provider: CaptureProvider,
    source_format: &str,
    path: &std::path::Path,
    root: &std::path::Path,
) -> (Uuid, super::BrokeredSourceAccess) {
    let event_id = Uuid::new_v4();
    let access = SourceAccessBroker::new(crate::test_provider_sqlite_data_root())
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider,
                source_format: source_format.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: path.to_path_buf(),
                source_root: Some(root.to_path_buf()),
                source_identity: Some("test-source".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event_id,
        )
        .unwrap();
    (event_id, access)
}

#[test]
fn resolver_requests_are_path_free() {
    let source = include_str!("../resolver.rs");
    let request_start = source.find("pub struct CompleteMessageRequest").unwrap();
    let request_end = source[request_start..]
        .find("pub struct SourceVerification")
        .map(|offset| request_start + offset)
        .unwrap();
    let request = &source[request_start..request_end];
    assert!(!request.contains("PathBuf"));
    assert!(!request.contains("raw_source_path"));
    assert!(!request.contains("source_root"));
    assert!(request.contains("BrokeredSourceAccess"));
}

#[test]
fn authorized_route_debug_omits_paths_roots_and_identity() {
    let route = AuthorizedSourceRoute {
        source_id: Uuid::nil(),
        provider: CaptureProvider::Codex,
        source_format: "test-format".to_owned(),
        family: CompleteContentSourceFamily::Jsonl,
        raw_source_path: PathBuf::from("/secret/provider/session.jsonl"),
        source_root: Some(PathBuf::from("/secret/provider")),
        source_identity: Some("secret-source-identity".to_owned()),
        source_snapshot: SourceSnapshot::default(),
    };

    let debug = format!("{route:?}");
    assert!(debug.contains("AuthorizedSourceRoute"));
    assert!(debug.contains("test-format"));
    for secret in [
        "/secret/provider/session.jsonl",
        "/secret/provider",
        "secret-source-identity",
        "raw_source_path",
        "source_root",
        "source_identity",
    ] {
        assert!(!debug.contains(secret), "debug output leaked {secret}");
    }

    let prepared = SourceAccessBroker::new(crate::test_provider_sqlite_data_root())
        .prepare(route, Uuid::new_v4())
        .unwrap();
    let debug = format!("{prepared:?}");
    assert!(debug.contains("PreparedSourceAdmission"));
    assert!(debug.contains("reserved_snapshot_bytes"));
    for secret in [
        "/secret/provider/session.jsonl",
        "/secret/provider",
        "secret-source-identity",
    ] {
        assert!(!debug.contains(secret), "prepared debug leaked {secret}");
    }
}

#[cfg(unix)]
#[test]
fn sqlite_snapshot_reservation_counts_sidecars_and_rejects_a_changed_total() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let wal = temp.path().join("state.db-wal");
    let shm = temp.path().join("state.db-shm");
    fs::write(&path, b"main").unwrap();
    fs::write(&wal, b"wal-bytes").unwrap();
    fs::write(&shm, b"shm").unwrap();
    let event_id = Uuid::new_v4();
    let route = AuthorizedSourceRoute {
        source_id: Uuid::new_v4(),
        provider: CaptureProvider::Crush,
        source_format: "crush_sqlite".to_owned(),
        family: CompleteContentSourceFamily::Sqlite,
        raw_source_path: path,
        source_root: Some(temp.path().to_path_buf()),
        source_identity: Some("sqlite-source".to_owned()),
        source_snapshot: SourceSnapshot::default(),
    };
    let broker = SourceAccessBroker::new(crate::test_provider_sqlite_data_root());

    let prepared = broker.prepare(route, event_id).unwrap();
    let reserved = prepared.reserved_snapshot_bytes();
    assert_eq!(
        reserved,
        u64::try_from(b"main".len() + b"wal-bytes".len() + b"shm".len()).unwrap()
    );

    fs::write(&wal, b"wal-bytes-grew").unwrap();
    let error = broker
        .admit_prepared_for_source_locators(prepared, &[])
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
}

#[cfg(target_os = "linux")]
#[test]
fn sqlite_snapshot_budget_admits_valid_sparse_main_over_512_mib() {
    use std::os::unix::fs::MetadataExt;

    const FORMER_COMPONENT_LIMIT: u64 = 512 * 1024 * 1024;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("opencode.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "create table messages (body text not null);
             insert into messages values ('large-valid-main');",
        )
        .unwrap();
    drop(connection);
    let expected_length = FORMER_COMPONENT_LIMIT + 4_096;
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(expected_length)
        .unwrap();
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.len(), expected_length);
    assert!(
        metadata.blocks().saturating_mul(512) < 1024 * 1024,
        "the complete-content regression fixture must remain physically sparse"
    );
    let event_id = Uuid::new_v4();
    let route = AuthorizedSourceRoute {
        source_id: Uuid::new_v4(),
        provider: CaptureProvider::OpenCode,
        source_format: "opencode_sqlite".to_owned(),
        family: CompleteContentSourceFamily::Sqlite,
        raw_source_path: path,
        source_root: Some(temp.path().to_path_buf()),
        source_identity: Some("large-opencode-source".to_owned()),
        source_snapshot: SourceSnapshot::default(),
    };
    let broker = SourceAccessBroker::new(crate::test_provider_sqlite_data_root());

    let prepared = broker.prepare(route, event_id).unwrap();
    assert_eq!(prepared.reserved_snapshot_bytes(), expected_length);
    let access = broker
        .admit_prepared_for_source_locators(prepared, &[])
        .unwrap();
    let snapshot = access.open_sqlite_snapshot(event_id).unwrap();
    let body: String = snapshot
        .query_row("select body from messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(body, "large-valid-main");
    access.finish_sqlite_snapshot(snapshot, event_id).unwrap();
}

#[cfg(unix)]
#[test]
fn sqlite_snapshot_budget_rejects_cumulative_main_plus_wal_over_total() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("create table messages (body text not null);")
        .unwrap();
    drop(connection);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(super::SQLITE_SNAPSHOT_MAX_TOTAL_BYTES)
        .unwrap();
    fs::write(path.with_file_name("state.db-wal"), b"x").unwrap();
    let event_id = Uuid::new_v4();
    let route = AuthorizedSourceRoute {
        source_id: Uuid::new_v4(),
        provider: CaptureProvider::OpenCode,
        source_format: "opencode_sqlite".to_owned(),
        family: CompleteContentSourceFamily::Sqlite,
        raw_source_path: path,
        source_root: Some(temp.path().to_path_buf()),
        source_identity: Some("over-budget-opencode-source".to_owned()),
        source_snapshot: SourceSnapshot::default(),
    };

    let error = SourceAccessBroker::new(crate::test_provider_sqlite_data_root())
        .prepare(route, event_id)
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::ContentTooLarge);
}

#[test]
fn portable_read_seam_rejects_route_replaced_before_read() {
    let event_id = Uuid::new_v4();
    let replacement_present = Cell::new(true);
    let read_completed = Cell::new(false);

    let error = finish_jsonl_read(
        || {
            read_completed.set(true);
            Ok(vec![1_u8])
        },
        || {
            assert!(read_completed.get(), "route was checked before the read");
            if replacement_present.get() {
                Err(CompleteContentError::new(
                    CompleteContentErrorKind::SourceChanged,
                    event_id,
                ))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();

    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
}

#[test]
fn portable_read_seam_rejects_route_replaced_after_read_before_return() {
    let event_id = Uuid::new_v4();
    let replacement_present = Cell::new(false);

    let error = finish_jsonl_read(
        || {
            replacement_present.set(true);
            Ok(vec![1_u8])
        },
        || {
            if replacement_present.get() {
                Err(CompleteContentError::new(
                    CompleteContentErrorKind::SourceChanged,
                    event_id,
                ))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();

    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
}

#[cfg(target_os = "windows")]
#[test]
fn windows_jsonl_read_rejects_named_replacement_after_admission() {
    use crate::complete_content::CompleteContentBodyDigest;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("session.jsonl");
    let moved = temp.path().join("moved-session.jsonl");
    let replacement = temp.path().join("replacement.jsonl");
    let record = br#"{"value":"original"}"#;
    let mut line = record.to_vec();
    line.push(b'\n');
    fs::write(&path, &line).unwrap();
    fs::write(&replacement, br#"{"value":"changed"}"#).unwrap();
    let event_id = Uuid::new_v4();
    let access = SourceAccessBroker::new(crate::test_provider_sqlite_data_root())
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::Codex,
                source_format: crate::CODEX_SESSION_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: path.clone(),
                source_root: Some(temp.path().to_path_buf()),
                source_identity: Some("test-source".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event_id,
        )
        .unwrap();

    fs::rename(&path, &moved).unwrap();
    fs::rename(&replacement, &path).unwrap();
    let error = access
        .read_jsonl_record(
            0,
            u64::try_from(line.len()).unwrap(),
            &CompleteContentBodyDigest::from_bytes(record),
            event_id,
        )
        .unwrap_err();

    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
}

#[test]
fn exact_jsonl_auxiliary_replace_delete_and_append_fail_closed() {
    let temp = tempfile::tempdir().unwrap();

    let mistral_session = temp.path().join("mistral/session");
    fs::create_dir_all(&mistral_session).unwrap();
    let mistral_messages = mistral_session.join("messages.jsonl");
    let mistral_metadata = mistral_session.join("meta.json");
    fs::write(&mistral_messages, b"{}\n").unwrap();
    fs::write(&mistral_metadata, b"{}").unwrap();
    let (event_id, access) = admit_jsonl(
        CaptureProvider::MistralVibe,
        MISTRAL_VIBE_SOURCE_FORMAT,
        &mistral_messages,
        temp.path(),
    );
    let replaced = mistral_session.join("old-meta.json");
    fs::rename(&mistral_metadata, &replaced).unwrap();
    fs::write(&mistral_metadata, b"{}").unwrap();
    assert_eq!(
        access.revalidate_jsonl(event_id).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );

    let mistral_delete = temp.path().join("mistral-delete/session");
    fs::create_dir_all(&mistral_delete).unwrap();
    let delete_messages = mistral_delete.join("messages.jsonl");
    let delete_metadata = mistral_delete.join("meta.json");
    fs::write(&delete_messages, b"{}\n").unwrap();
    fs::write(&delete_metadata, b"{}").unwrap();
    let (event_id, access) = admit_jsonl(
        CaptureProvider::MistralVibe,
        MISTRAL_VIBE_SOURCE_FORMAT,
        &delete_messages,
        temp.path(),
    );
    fs::remove_file(&delete_metadata).unwrap();
    assert_eq!(
        access.revalidate_jsonl(event_id).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );

    let missing_session = temp.path().join("mistral-missing/session");
    fs::create_dir_all(&missing_session).unwrap();
    let missing_messages = missing_session.join("messages.jsonl");
    fs::write(&missing_messages, b"{}\n").unwrap();
    let error = SourceAccessBroker::new(crate::test_provider_sqlite_data_root())
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::MistralVibe,
                source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: missing_messages,
                source_root: Some(temp.path().to_path_buf()),
                source_identity: Some("test-source".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            Uuid::new_v4(),
        )
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);

    let openclaw_sessions = temp.path().join("openclaw/agents/main/sessions");
    fs::create_dir_all(&openclaw_sessions).unwrap();
    let openclaw_transcript = openclaw_sessions.join("session.jsonl");
    let openclaw_index = openclaw_sessions.join("sessions.json");
    fs::write(&openclaw_transcript, b"{}\n").unwrap();
    fs::write(&openclaw_index, json!({"session": {}}).to_string()).unwrap();
    let (event_id, access) = admit_jsonl(
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        &openclaw_transcript,
        temp.path(),
    );
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&openclaw_index)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    assert_eq!(
        access.revalidate_jsonl(event_id).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );

    let openclaw_absent = temp.path().join("openclaw-absent/agents/main/sessions");
    fs::create_dir_all(&openclaw_absent).unwrap();
    let absent_transcript = openclaw_absent.join("session.jsonl");
    let created_index = openclaw_absent.join("sessions.json");
    fs::write(&absent_transcript, b"{}\n").unwrap();
    let (event_id, access) = admit_jsonl(
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        &absent_transcript,
        temp.path(),
    );
    fs::write(&created_index, b"{}").unwrap();
    assert_eq!(
        access.revalidate_jsonl(event_id).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );
}

#[cfg(unix)]
#[test]
fn source_root_safety_exact_jsonl_auxiliary_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("mistral/session");
    fs::create_dir_all(&session).unwrap();
    let messages = session.join("messages.jsonl");
    let metadata = session.join("meta.json");
    let replacement = session.join("replacement.json");
    fs::write(&messages, b"{}\n").unwrap();
    fs::write(&metadata, b"{}").unwrap();
    fs::write(&replacement, b"{}").unwrap();
    let (event_id, access) = admit_jsonl(
        CaptureProvider::MistralVibe,
        MISTRAL_VIBE_SOURCE_FORMAT,
        &messages,
        temp.path(),
    );
    fs::remove_file(&metadata).unwrap();
    symlink(&replacement, &metadata).unwrap();
    assert_eq!(
        access.revalidate_jsonl(event_id).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );

    let fresh = temp.path().join("mistral-fresh/session");
    fs::create_dir_all(&fresh).unwrap();
    let fresh_messages = fresh.join("messages.jsonl");
    let fresh_metadata = fresh.join("meta.json");
    let fresh_replacement = fresh.join("replacement.json");
    fs::write(&fresh_messages, b"{}\n").unwrap();
    fs::write(&fresh_replacement, b"{}").unwrap();
    symlink(&fresh_replacement, &fresh_metadata).unwrap();
    let error = SourceAccessBroker::new(crate::test_provider_sqlite_data_root())
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::MistralVibe,
                source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: fresh_messages,
                source_root: Some(temp.path().to_path_buf()),
                source_identity: Some("test-source".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            Uuid::new_v4(),
        )
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceUnreadable);
}

#[cfg(unix)]
#[test]
fn source_root_safety_kimi_auxiliary_symlink_replacement_fails_closed() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".kimi-code");
    let session = root.join("sessions/work/session-1");
    let agent = session.join("agents/main");
    fs::create_dir_all(&agent).unwrap();
    let wire = agent.join("wire.jsonl");
    let state = session.join("state.json");
    let replacement = session.join("replacement.json");
    fs::write(&wire, b"{}\n").unwrap();
    fs::write(&state, b"{}").unwrap();
    fs::write(&replacement, b"{}").unwrap();
    fs::write(
        root.join("session_index.jsonl"),
        b"{\"sessionId\":\"session-1\"}\n",
    )
    .unwrap();
    let (event_id, access) = admit_jsonl(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        &wire,
        &root,
    );
    fs::remove_file(&state).unwrap();
    symlink(&replacement, &state).unwrap();
    assert_eq!(
        access.revalidate_jsonl(event_id).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );
}

#[test]
fn ordinary_jsonl_routes_do_not_gain_auxiliary_requirements() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("rollout.jsonl");
    fs::write(&source, b"{}\n").unwrap();
    let (event_id, access) = admit_jsonl(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &source,
        temp.path(),
    );
    assert!(access.exact_jsonl_binding().is_none());
    access.revalidate_jsonl(event_id).unwrap();
}

fn replace_tree_from_thread(named: PathBuf, moved: PathBuf, replacement: PathBuf) {
    std::thread::spawn(move || {
        fs::rename(&named, moved).unwrap();
        fs::rename(replacement, named).unwrap();
    })
    .join()
    .unwrap();
}

#[cfg(unix)]
#[test]
fn source_root_safety_ordinary_jsonl_retains_exact_bytes_but_rejects_leaf_swap() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("session.jsonl");
    let moved = temp.path().join("moved-session.jsonl");
    let replacement = temp.path().join("replacement.jsonl");
    let record = br#"{"content":"inside-leaf"}"#;
    let mut line = record.to_vec();
    line.push(b'\n');
    fs::write(&path, &line).unwrap();
    fs::write(
        &replacement,
        b"{\"content\":\"OUTSIDE_LEAF_MUST_NOT_ESCAPE\"}\n",
    )
    .unwrap();
    let (event_id, access) = admit_jsonl(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &path,
        temp.path(),
    );

    std::thread::spawn({
        let path = path.clone();
        move || {
            fs::rename(&path, moved).unwrap();
            fs::rename(replacement, path).unwrap();
        }
    })
    .join()
    .unwrap();

    let retained = access
        .read_jsonl_record(
            0,
            u64::try_from(line.len()).unwrap(),
            &CompleteContentBodyDigest::from_bytes(record),
            event_id,
        )
        .unwrap();
    assert_eq!(retained, line);
    assert!(!retained
        .windows(b"OUTSIDE_LEAF_MUST_NOT_ESCAPE".len())
        .any(|window| window == b"OUTSIDE_LEAF_MUST_NOT_ESCAPE"));
    assert_eq!(
        access.revalidate_jsonl(event_id).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );
}

#[cfg(unix)]
#[test]
fn source_root_safety_ordinary_jsonl_retains_exact_bytes_but_rejects_ancestor_swap() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("parent");
    let moved_parent = temp.path().join("moved-parent");
    let root = parent.join("root");
    let replacement_parent = temp.path().join("replacement-parent");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(replacement_parent.join("root")).unwrap();
    let path = root.join("session.jsonl");
    let record = br#"{"content":"inside-ancestor"}"#;
    let mut line = record.to_vec();
    line.push(b'\n');
    fs::write(&path, &line).unwrap();
    fs::write(
        replacement_parent.join("root/session.jsonl"),
        b"{\"content\":\"OUTSIDE_ANCESTOR_MUST_NOT_ESCAPE\"}\n",
    )
    .unwrap();
    let (event_id, access) = admit_jsonl(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &path,
        &root,
    );

    replace_tree_from_thread(parent, moved_parent, replacement_parent);

    let retained = access
        .read_jsonl_record(
            0,
            u64::try_from(line.len()).unwrap(),
            &CompleteContentBodyDigest::from_bytes(record),
            event_id,
        )
        .unwrap();
    assert_eq!(retained, line);
    assert!(!retained
        .windows(b"OUTSIDE_ANCESTOR_MUST_NOT_ESCAPE".len())
        .any(|window| window == b"OUTSIDE_ANCESTOR_MUST_NOT_ESCAPE"));
    assert_eq!(
        access.revalidate_jsonl(event_id).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );
}

#[cfg(unix)]
#[test]
fn source_root_safety_compound_auxiliary_ancestor_swap_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("mistral");
    let moved = temp.path().join("moved-mistral");
    let replacement = temp.path().join("replacement-mistral");
    let session = root.join("session");
    fs::create_dir_all(&session).unwrap();
    fs::create_dir_all(replacement.join("session")).unwrap();
    let messages = session.join("messages.jsonl");
    let metadata = session.join("meta.json");
    let record = br#"{"content":"inside-compound"}"#;
    let mut line = record.to_vec();
    line.push(b'\n');
    fs::write(&messages, &line).unwrap();
    fs::write(&metadata, b"{}").unwrap();
    fs::write(
        replacement.join("session/messages.jsonl"),
        b"{\"content\":\"OUTSIDE_AUXILIARY_MUST_NOT_ESCAPE\"}\n",
    )
    .unwrap();
    fs::write(replacement.join("session/meta.json"), b"{}").unwrap();
    let (event_id, access) = admit_jsonl(
        CaptureProvider::MistralVibe,
        MISTRAL_VIBE_SOURCE_FORMAT,
        &messages,
        temp.path(),
    );

    replace_tree_from_thread(root, moved, replacement);

    let retained = access
        .read_jsonl_record(
            0,
            u64::try_from(line.len()).unwrap(),
            &CompleteContentBodyDigest::from_bytes(record),
            event_id,
        )
        .unwrap();
    assert_eq!(retained, line);
    assert!(!retained
        .windows(b"OUTSIDE_AUXILIARY_MUST_NOT_ESCAPE".len())
        .any(|window| window == b"OUTSIDE_AUXILIARY_MUST_NOT_ESCAPE"));
    assert_eq!(
        access.revalidate_jsonl(event_id).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );
}

#[cfg(target_os = "windows")]
#[test]
fn source_root_safety_complete_content_rejects_unc_network_routes() {
    for path in [
        Path::new(r"\\server\share\session.jsonl"),
        Path::new(r"\\?\UNC\server\share\session.jsonl"),
    ] {
        let event_id = Uuid::new_v4();
        let error = SourceAccessBroker::new(crate::test_provider_sqlite_data_root())
            .admit(
                AuthorizedSourceRoute {
                    source_id: Uuid::new_v4(),
                    provider: CaptureProvider::Codex,
                    source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
                    family: CompleteContentSourceFamily::Jsonl,
                    raw_source_path: path.to_path_buf(),
                    source_root: None,
                    source_identity: Some("unc-source".to_owned()),
                    source_snapshot: SourceSnapshot::default(),
                },
                event_id,
            )
            .unwrap_err();
        assert_eq!(error.kind, CompleteContentErrorKind::SourceUnreadable);
        assert_eq!(error.event_id, event_id);
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
#[test]
fn source_root_safety_complete_content_unsupported_platform_is_typed() {
    let event_id = Uuid::new_v4();
    let error = SourceAccessBroker::new(crate::test_provider_sqlite_data_root())
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::Codex,
                source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: PathBuf::from("/provider/session.jsonl"),
                source_root: Some(PathBuf::from("/provider")),
                source_identity: Some("unsupported-source".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event_id,
        )
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::HydrationUnsupported);
    assert_eq!(error.event_id, event_id);
}

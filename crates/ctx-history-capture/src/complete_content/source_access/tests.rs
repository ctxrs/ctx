use std::{cell::Cell, fs, path::PathBuf};

use ctx_history_core::CaptureProvider;
use serde_json::json;
use uuid::Uuid;

use super::{jsonl::finish_jsonl_read, AuthorizedSourceRoute, SourceAccessBroker};
#[cfg(unix)]
use crate::KIMI_CODE_CLI_SOURCE_FORMAT;
use crate::{
    complete_content::{
        CompleteContentError, CompleteContentErrorKind, CompleteContentSourceFamily, SourceSnapshot,
    },
    CODEX_SESSION_SOURCE_FORMAT, MISTRAL_VIBE_SOURCE_FORMAT, OPENCLAW_SOURCE_FORMAT,
};

fn admit_jsonl(
    provider: CaptureProvider,
    source_format: &str,
    path: &std::path::Path,
    root: &std::path::Path,
) -> (Uuid, super::BrokeredSourceAccess) {
    let event_id = Uuid::new_v4();
    let access = SourceAccessBroker::new()
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
    let result_start = source.find("pub struct ResultContentRequest").unwrap();
    let request = &source[request_start..result_start];
    let result_end = source[result_start..]
        .find("pub struct ResolvedResultContent")
        .map(|offset| result_start + offset)
        .unwrap();
    let result = &source[result_start..result_end];
    for declaration in [request, result] {
        assert!(!declaration.contains("PathBuf"));
        assert!(!declaration.contains("raw_source_path"));
        assert!(!declaration.contains("source_root"));
        assert!(declaration.contains("BrokeredSourceAccess"));
    }
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
    let access = SourceAccessBroker::new()
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
    let error = SourceAccessBroker::new()
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
fn exact_jsonl_auxiliary_symlink_is_rejected_at_admission_and_revalidation() {
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
    let error = SourceAccessBroker::new()
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
fn kimi_auxiliary_symlink_replacement_fails_closed() {
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

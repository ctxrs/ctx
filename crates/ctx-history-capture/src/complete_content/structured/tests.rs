use std::{collections::BTreeSet, fs, path::Path, time::Duration};

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, Fidelity};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::complete_content::{CompleteContentSourceLocator, SourceSnapshot};

fn format_for(provider: CaptureProvider) -> &'static str {
    match provider {
        CaptureProvider::Auggie => "auggie_session_json",
        CaptureProvider::Continue => "continue_cli_sessions_json",
        CaptureProvider::OpenHands => "openhands_file_events",
        CaptureProvider::RovoDev => "rovodev_session_json_tree",
        CaptureProvider::Cline => "cline_task_directory_json",
        CaptureProvider::RooCode => "roo_task_directory_json",
        CaptureProvider::CodeBuddy => "codebuddy_history_json",
        _ => panic!("test requested a non-structured provider"),
    }
}

#[allow(clippy::too_many_arguments)]
fn request(
    provider: CaptureProvider,
    path: &Path,
    source_root: Option<&Path>,
    provider_session_id: &str,
    ordinal: u64,
    subrecord: u32,
    native_id: &str,
    record_bytes: &[u8],
    text: &str,
) -> CompleteMessageRequest {
    let indexed_limit_chars = 4;
    CompleteMessageRequest {
        event_id: Uuid::new_v4(),
        provider,
        source_format: format_for(provider).to_owned(),
        raw_source_path: path.to_path_buf(),
        source_root: source_root.map(Path::to_path_buf),
        source_identity: Some(format!("test:{}", provider.as_str())),
        source_family: Some(CompleteContentSourceFamily::Structured),
        source_locator: Some(
            CompleteContentSourceLocator::new(
                STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
                encode_structured_locator(provider, ordinal, subrecord, native_id).unwrap(),
            )
            .unwrap(),
        ),
        source_snapshot: SourceSnapshot {
            size_bytes: Some(record_bytes.len() as u64),
            modified_at_ms: None,
            sha256: Some(digest_bytes(record_bytes)),
        },
        provider_session_id: Some(provider_session_id.to_owned()),
        source_record_ordinal: ordinal,
        source_record_subrecord_index: subrecord,
        expected_provider_event_hash: native_id.to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(native_id.to_owned()),
        expected_record_digest: CompleteContentBodyDigest::parse(digest_bytes(record_bytes)),
        expected_body_digest: Some(CompleteContentBodyDigest::from_text(text)),
        indexed_text: text.chars().take(indexed_limit_chars).collect(),
        indexed_limit_chars,
    }
}

fn resolve_one_message(request: CompleteMessageRequest) -> CompleteMessage {
    StructuredCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap()
        .pop()
        .unwrap()
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

#[test]
fn capability_table_is_exhaustive_and_routes_exactly_seven_providers() {
    assert_eq!(STRUCTURED_COMPLETE_CONTENT_CAPABILITIES.len(), 41);
    let providers = STRUCTURED_COMPLETE_CONTENT_CAPABILITIES
        .iter()
        .map(|capability| capability.provider.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(providers.len(), 41);
    let supported = STRUCTURED_COMPLETE_CONTENT_CAPABILITIES
        .iter()
        .filter(|capability| {
            capability.status == StructuredCompleteContentCapabilityStatus::Supported
        })
        .map(|capability| capability.provider.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        supported,
        BTreeSet::from([
            "auggie",
            "codebuddy",
            "continue",
            "openhands",
            "rovodev",
            "cline",
            "roo_code",
        ])
    );
    assert!(STRUCTURED_COMPLETE_CONTENT_CAPABILITIES
        .iter()
        .all(|entry| {
            entry.status != StructuredCompleteContentCapabilityStatus::Unsupported
                && !entry.reason.is_empty()
        }));
}

#[test]
fn persisted_locator_is_bounded_path_free_and_only_attached_to_truncated_messages() {
    let complete_text = format!("unicode λ\n{}", "z".repeat(PROVIDER_MAX_TEXT_CHARS + 1));
    let record = br#"{"message":"native record"}"#;
    let mut event = ProviderEventEnvelope {
        provider_event_index: 12,
        provider_event_hash: Some("native-12".to_owned()),
        cursor: Some("local cursor that is not persisted in the locator".to_owned()),
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: DateTime::<Utc>::UNIX_EPOCH,
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: json!({"text": "bounded"}),
        metadata: json!({}),
    };
    attach_structured_complete_content_locator(
        CaptureProvider::RovoDev,
        &mut event,
        9,
        3,
        "native-12",
        record,
        &complete_text,
    )
    .unwrap();
    let value = event
        .metadata
        .get(COMPLETE_CONTENT_LOCATOR_METADATA_KEY)
        .unwrap();
    let encoded = serde_json::to_vec(value).unwrap();
    assert!(encoded.len() <= 4 * 1024);
    assert!(!String::from_utf8_lossy(&encoded).contains("/home/"));
    let persisted = PersistedCompleteContentLocatorV1::from_metadata_value(value).unwrap();
    assert_eq!(persisted.family(), CompleteContentSourceFamily::Structured);
    assert_eq!(persisted.native_record_id(), "native-12");
    let decoded = decode_structured_locator(persisted.source_locator().unwrap().value()).unwrap();
    assert_eq!(
        decoded,
        (CaptureProvider::RovoDev, 9, 3, "native-12".to_owned())
    );

    let mut short = event.clone();
    short.metadata = json!({});
    attach_structured_complete_content_locator(
        CaptureProvider::RovoDev,
        &mut short,
        9,
        3,
        "native-12",
        record,
        "short",
    )
    .unwrap();
    assert!(short
        .metadata
        .get(COMPLETE_CONTENT_LOCATOR_METADATA_KEY)
        .is_none());
}

#[test]
fn resolves_whole_json_provider_families_byte_exactly() {
    let directory = TempDir::new().unwrap();

    let auggie_text = "Auggie λ first\nsecond line";
    let auggie_bytes = format!(
        r#"{{"chatHistory":[{{"exchange":{{"request_id":"aug-1","request_message":{}}}}}]}}"#,
        serde_json::to_string(auggie_text).unwrap()
    )
    .into_bytes();
    let auggie_path = directory.path().join("auggie.json");
    write(&auggie_path, &auggie_bytes);
    assert_eq!(
        resolve_one_message(request(
            CaptureProvider::Auggie,
            &auggie_path,
            None,
            "auggie-session",
            0,
            0,
            "aug-1:request",
            &auggie_bytes,
            auggie_text,
        ))
        .text,
        auggie_text
    );

    let continue_text = "Continue 🦀\nkeeps exact lines";
    let continue_bytes = serde_json::to_vec(&json!({
        "history": [{"id": "continue-1", "message": {"role": "user", "content": continue_text}}]
    }))
    .unwrap();
    let continue_path = directory.path().join("continue-session.json");
    write(&continue_path, &continue_bytes);
    assert_eq!(
        resolve_one_message(request(
            CaptureProvider::Continue,
            &continue_path,
            None,
            "continue-session",
            0,
            0,
            "continue-1",
            &continue_bytes,
            continue_text,
        ))
        .text,
        continue_text
    );

    let rovo_text = "Rovo Dev\r\nmultiline Ω";
    let rovo_bytes = serde_json::to_vec(&json!({
        "message_history": [{"id": "rovo-1", "role": "assistant", "content": rovo_text}]
    }))
    .unwrap();
    let rovo_path = directory.path().join("session_context.json");
    write(&rovo_path, &rovo_bytes);
    assert_eq!(
        resolve_one_message(request(
            CaptureProvider::RovoDev,
            &rovo_path,
            None,
            "rovo-session",
            0,
            0,
            "rovo-1",
            &rovo_bytes,
            rovo_text,
        ))
        .text,
        rovo_text
    );
}

#[test]
fn resolves_one_file_and_compound_provider_families() {
    let directory = TempDir::new().unwrap();

    let openhands_text = "OpenHands exact\nmessage λ";
    let openhands_bytes = serde_json::to_vec(&json!({
        "id": "openhands-1",
        "timestamp": "2026-07-22T12:00:00Z",
        "kind": "MessageEvent",
        "llm_message": {"role": "assistant", "content": openhands_text}
    }))
    .unwrap();
    let openhands_path = directory
        .path()
        .join("profile/v1_conversations/session/events/event-1.json");
    write(&openhands_path, &openhands_bytes);
    assert_eq!(
        resolve_one_message(request(
            CaptureProvider::OpenHands,
            &openhands_path,
            None,
            "openhands-session",
            17,
            0,
            "openhands-1",
            &openhands_bytes,
            openhands_text,
        ))
        .text,
        openhands_text
    );

    let codebuddy_text = "CodeBuddy compound\nmessage π";
    let codebuddy_bytes = serde_json::to_vec(&json!({
        "message": {"content": codebuddy_text}
    }))
    .unwrap();
    let codebuddy_path = directory
        .path()
        .join("codebuddy/session/messages/message-7.json");
    write(&codebuddy_path, &codebuddy_bytes);
    assert_eq!(
        resolve_one_message(request(
            CaptureProvider::CodeBuddy,
            &codebuddy_path,
            None,
            "codebuddy-session",
            7,
            0,
            "codebuddy-session:message-7",
            &codebuddy_bytes,
            codebuddy_text,
        ))
        .text,
        codebuddy_text
    );

    for (provider, file_name, source, task_id) in [
        (
            CaptureProvider::Cline,
            "api_conversation_history.json",
            "api_conversation_history",
            "cline-task",
        ),
        (
            CaptureProvider::RooCode,
            "ui_messages.json",
            "ui_messages",
            "roo-task",
        ),
    ] {
        let text = format!("{} raw record\nwith escapes λ", provider.as_str());
        let native = format!("{}-native", provider.as_str());
        let raw_record = format!(
            r#"{{ "id": {}, "type": "ask", "role": "user", "content": {} }}"#,
            serde_json::to_string(&native).unwrap(),
            serde_json::to_string(&text).unwrap()
        );
        let file_bytes = format!(
            "{{\"metadata\":{{\"decoy\":\"messages\"}},\"messages\":[\n  {raw_record}\n]}}"
        )
        .into_bytes();
        let path = directory
            .path()
            .join(format!("tasks/{}/{file_name}", provider.as_str()));
        write(&path, &file_bytes);
        let native_id = format!("{task_id}:{source}:{native}");
        assert_eq!(
            resolve_one_message(request(
                provider,
                &path,
                None,
                task_id,
                4,
                0,
                &native_id,
                raw_record.as_bytes(),
                &text,
            ))
            .text,
            text
        );
    }
}

#[test]
fn openhands_recovery_matches_authoritative_current_and_legacy_decoding() {
    let directory = TempDir::new().unwrap();
    let cases = [
        (
            "current.json",
            json!({
                "id": "current-id",
                "timestamp": "2026-07-22T12:00:00Z",
                "kind": "MessageEvent",
                "source": "agent",
                "content": "current exact recovery text"
            }),
            "current-id",
            "current exact recovery text",
        ),
        (
            "legacy-fallback.json",
            json!({
                "timestamp": "2026-07-22T12:00:01Z",
                "source": "user",
                "llm_message": {
                    "role": "user",
                    "content": "legacy exact recovery text"
                }
            }),
            "legacy-fallback",
            "legacy exact recovery text",
        ),
    ];

    for (file_name, value, expected_id, expected_text) in cases {
        let path = directory
            .path()
            .join("profile/v1_conversations/session/events")
            .join(file_name);
        let bytes = serde_json::to_vec(&value).unwrap();
        write(&path, &bytes);
        let decoded = decode_openhands_event(&path, &bytes).unwrap();
        assert_eq!(decoded.event_id(), expected_id);
        assert_eq!(decoded.event_type(), EventType::Message);
        assert_eq!(decoded.text(), expected_text);

        let resolved = resolve_one_message(request(
            CaptureProvider::OpenHands,
            &path,
            None,
            "session",
            17,
            0,
            expected_id,
            &bytes,
            expected_text,
        ));
        assert_eq!(resolved.text, expected_text);
    }
}

#[test]
fn openhands_recovery_fails_closed_for_malformed_oversized_and_unbounded_events() {
    let directory = TempDir::new().unwrap();
    let malformed_path = directory
        .path()
        .join("profile/v1_conversations/session/events/malformed.json");
    let malformed = b"{not-json";
    write(&malformed_path, malformed);
    let malformed_failure = StructuredCompleteContentResolver::new()
        .resolve(&[request(
            CaptureProvider::OpenHands,
            &malformed_path,
            None,
            "session",
            18,
            0,
            "malformed",
            malformed,
            "malformed expected text",
        )])
        .unwrap_err();
    assert_eq!(
        malformed_failure.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    let oversized_path = directory
        .path()
        .join("profile/v1_conversations/session/events/oversized.json");
    let oversized = vec![b'x'; crate::MAX_PROVIDER_JSONL_LINE_BYTES + 1];
    write(&oversized_path, &oversized);
    let oversized_failure = StructuredCompleteContentResolver::new()
        .resolve(&[request(
            CaptureProvider::OpenHands,
            &oversized_path,
            None,
            "session",
            19,
            0,
            "oversized",
            &oversized,
            "oversized expected text",
        )])
        .unwrap_err();
    assert_eq!(
        oversized_failure.kind,
        CompleteContentErrorKind::ContentTooLarge
    );

    let bounded_path = directory
        .path()
        .join("profile/v1_conversations/session/events/bounded.json");
    let bounded_text = "bounded OpenHands recovery";
    let bounded = serde_json::to_vec(&json!({
        "id": "bounded",
        "timestamp": "2026-07-22T12:00:02Z",
        "kind": "MessageEvent",
        "source": "agent",
        "content": bounded_text,
        "extra": {"nested": {"value": true}}
    }))
    .unwrap();
    write(&bounded_path, &bounded);
    let bounded_request = || {
        request(
            CaptureProvider::OpenHands,
            &bounded_path,
            None,
            "session",
            20,
            0,
            "bounded",
            &bounded,
            bounded_text,
        )
    };
    let base = StructuredBounds::default();
    let depth_limited = StructuredCompleteContentResolver::with_bounds(StructuredBounds {
        max_json_depth: 1,
        ..base
    });
    assert_eq!(
        depth_limited
            .resolve(&[bounded_request()])
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::ContentTooLarge
    );

    let entry_limited = StructuredCompleteContentResolver::with_bounds(StructuredBounds {
        max_entries: 4,
        ..base
    });
    assert_eq!(
        entry_limited
            .resolve(&[bounded_request()])
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::ContentTooLarge
    );
}

#[test]
fn resolves_bounded_record_from_compound_file_larger_than_body_limit() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("api_conversation_history.json");
    let text = "small complete record in a large bounded compound file";
    let record = format!(
        r#"{{"id":"large-1","role":"user","content":{}}}"#,
        serde_json::to_string(text).unwrap()
    );
    let mut file = Vec::with_capacity(COMPLETE_CONTENT_MAX_BODY_BYTES + 1024);
    file.push(b'[');
    file.extend_from_slice(record.as_bytes());
    file.resize(COMPLETE_CONTENT_MAX_BODY_BYTES + 512, b' ');
    file.push(b']');
    write(&path, &file);
    assert_eq!(
        resolve_one_message(request(
            CaptureProvider::Cline,
            &path,
            None,
            "large-task",
            0,
            0,
            "large-task:api_conversation_history:large-1",
            record.as_bytes(),
            text,
        ))
        .text,
        text
    );
}

#[test]
fn follows_verified_custom_root_moves_and_real_json5_profile_roots() {
    let directory = TempDir::new().unwrap();
    let old_path = directory.path().join("old/session_context.json");
    let moved_root = directory.path().join("new/custom-profile/insiders");
    let moved_path = moved_root.join("session_context.json");
    let text = "moved source still verifies exactly";
    let bytes = serde_json::to_vec(&json!({
        "message_history": [{"id": "moved-1", "role": "user", "content": text}]
    }))
    .unwrap();
    write(&moved_path, &bytes);
    assert_eq!(
        resolve_one_message(request(
            CaptureProvider::RovoDev,
            &old_path,
            Some(&moved_root),
            "moved-session",
            0,
            0,
            "moved-1",
            &bytes,
            text,
        ))
        .text,
        text
    );

    let profile = directory.path().join("profiles.json5");
    let profile_text = format!(
        "// stable + insiders profiles\n{{ profiles: [{{ storagePath: {}, }},], }}",
        serde_json::to_string(moved_root.to_str().unwrap()).unwrap()
    );
    write(&profile, profile_text.as_bytes());
    assert_eq!(
        resolve_one_message(request(
            CaptureProvider::RovoDev,
            &profile,
            None,
            "moved-session",
            0,
            0,
            "moved-1",
            &bytes,
            text,
        ))
        .text,
        text
    );
}

#[test]
fn parses_xml_profile_roots_and_rejects_doctype_entities() {
    let directory = TempDir::new().unwrap();
    let profile_root = directory.path().join("profile & insiders");
    let source = profile_root.join("session_context.json");
    let text = "XML-selected complete body";
    let bytes = serde_json::to_vec(&json!({
        "message_history": [{"id": "xml-1", "content": text}]
    }))
    .unwrap();
    write(&source, &bytes);
    let escaped_root = profile_root.to_str().unwrap().replace('&', "&amp;");
    let profile = directory.path().join("profiles.xml");
    write(
        &profile,
        format!(r#"<profiles><profile globalStoragePath="{escaped_root}"/></profiles>"#).as_bytes(),
    );
    assert_eq!(
        resolve_one_message(request(
            CaptureProvider::RovoDev,
            &profile,
            None,
            "xml-session",
            0,
            0,
            "xml-1",
            &bytes,
            text,
        ))
        .text,
        text
    );

    write(
        &profile,
        br#"<!DOCTYPE x [<!ENTITY root "/tmp">]><profiles><path>&root;</path></profiles>"#,
    );
    let failure = StructuredCompleteContentResolver::new()
        .resolve(&[request(
            CaptureProvider::RovoDev,
            &profile,
            None,
            "xml-session",
            0,
            0,
            "xml-1",
            &bytes,
            text,
        )])
        .unwrap_err();
    assert_eq!(failure.kind, CompleteContentErrorKind::SourceChanged);
}

#[test]
fn detects_mutation_wrong_identity_and_wrong_body_digest() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("session_context.json");
    let text = "mutation-sensitive message";
    let bytes = serde_json::to_vec(&json!({
        "message_history": [{"id": "stable-1", "content": text}]
    }))
    .unwrap();
    write(&path, &bytes);

    let mut wrong_native = request(
        CaptureProvider::RovoDev,
        &path,
        None,
        "session",
        0,
        0,
        "stable-1",
        &bytes,
        text,
    );
    wrong_native.expected_native_record_id = Some("other".to_owned());
    assert_eq!(
        StructuredCompleteContentResolver::new()
            .resolve(&[wrong_native])
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    let mut wrong_body = request(
        CaptureProvider::RovoDev,
        &path,
        None,
        "session",
        0,
        0,
        "stable-1",
        &bytes,
        text,
    );
    wrong_body.expected_body_digest = Some(CompleteContentBodyDigest::from_text("different"));
    assert_eq!(
        StructuredCompleteContentResolver::new()
            .resolve(&[wrong_body])
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    let mutation_request = request(
        CaptureProvider::RovoDev,
        &path,
        None,
        "session",
        0,
        0,
        "stable-1",
        &bytes,
        text,
    );
    write(
        &path,
        br#"{"message_history":[{"id":"stable-1","content":"mutated"}]}"#,
    );
    assert_eq!(
        StructuredCompleteContentResolver::new()
            .resolve(&[mutation_request])
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::SourceChanged
    );
}

#[test]
fn multi_message_resolution_is_all_or_nothing() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("session_context.json");
    let first = "first complete message";
    let second = "second complete message";
    let bytes = serde_json::to_vec(&json!({
        "message_history": [
            {"id": "atomic-1", "content": first},
            {"id": "atomic-2", "content": second}
        ]
    }))
    .unwrap();
    write(&path, &bytes);
    let first_request = request(
        CaptureProvider::RovoDev,
        &path,
        None,
        "atomic-session",
        0,
        0,
        "atomic-1",
        &bytes,
        first,
    );
    let mut second_request = request(
        CaptureProvider::RovoDev,
        &path,
        None,
        "atomic-session",
        0,
        1,
        "atomic-2",
        &bytes,
        second,
    );
    second_request.expected_body_digest = Some(CompleteContentBodyDigest::from_text("wrong"));
    let second_event = second_request.event_id;
    let failure = StructuredCompleteContentResolver::new()
        .resolve(&[first_request, second_request])
        .unwrap_err();
    assert_eq!(failure.event_id, second_event);
    assert_eq!(
        failure.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

#[test]
fn enforces_file_depth_entry_and_deadline_bounds() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("root/session_context.json");
    let text = "bounded structured message";
    let bytes = serde_json::to_vec(&json!({
        "message_history": [{"id": "bounded-1", "content": text}]
    }))
    .unwrap();
    write(&path, &bytes);
    write(&directory.path().join("root/unrelated.json"), b"{}");
    let request_value = || {
        request(
            CaptureProvider::RovoDev,
            &directory.path().join("root"),
            None,
            "bounded-session",
            0,
            0,
            "bounded-1",
            &bytes,
            text,
        )
    };
    let base = StructuredBounds::default();
    let file_limited = StructuredCompleteContentResolver::with_bounds(StructuredBounds {
        max_files: 1,
        ..base
    });
    assert_eq!(
        file_limited.resolve(&[request_value()]).unwrap_err().kind,
        CompleteContentErrorKind::ContentTooLarge
    );

    let nested = directory.path().join("nested/a/b/session_context.json");
    write(&nested, &bytes);
    let depth_limited = StructuredCompleteContentResolver::with_bounds(StructuredBounds {
        max_depth: 0,
        ..base
    });
    let depth_request = request(
        CaptureProvider::RovoDev,
        &directory.path().join("nested"),
        None,
        "bounded-session",
        0,
        0,
        "bounded-1",
        &bytes,
        text,
    );
    assert_eq!(
        depth_limited.resolve(&[depth_request]).unwrap_err().kind,
        CompleteContentErrorKind::ContentTooLarge
    );

    let entry_limited = StructuredCompleteContentResolver::with_bounds(StructuredBounds {
        max_entries: 2,
        ..base
    });
    assert_eq!(
        entry_limited.resolve(&[request_value()]).unwrap_err().kind,
        CompleteContentErrorKind::ContentTooLarge
    );

    let deadline_limited = StructuredCompleteContentResolver::with_bounds(StructuredBounds {
        deadline: Duration::ZERO,
        ..base
    });
    assert_eq!(
        deadline_limited
            .resolve(&[request_value()])
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::SourceChanged
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinks_and_parent_traversal() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new().unwrap();
    let real = directory.path().join("real");
    let path = real.join("session_context.json");
    let text = "symlink-protected message";
    let bytes = serde_json::to_vec(&json!({
        "message_history": [{"id": "link-1", "content": text}]
    }))
    .unwrap();
    write(&path, &bytes);
    let link = directory.path().join("linked");
    symlink(&real, &link).unwrap();
    let linked_request = request(
        CaptureProvider::RovoDev,
        &link,
        None,
        "link-session",
        0,
        0,
        "link-1",
        &bytes,
        text,
    );
    assert_eq!(
        StructuredCompleteContentResolver::new()
            .resolve(&[linked_request])
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::SourceChanged
    );

    let traversal = real.join("subdir/../session_context.json");
    let traversal_request = request(
        CaptureProvider::RovoDev,
        &traversal,
        None,
        "link-session",
        0,
        0,
        "link-1",
        &bytes,
        text,
    );
    assert_eq!(
        StructuredCompleteContentResolver::new()
            .resolve(&[traversal_request])
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

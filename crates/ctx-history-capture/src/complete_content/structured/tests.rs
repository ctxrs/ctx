use std::{collections::BTreeSet, fs, path::Path, time::Duration};

use ctx_history_core::ContentRef;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
#[cfg(unix)]
use crate::complete_content::structured::source_access::{
    set_structured_admission_test_hook, StructuredAdmissionTestStage,
};
use crate::complete_content::{
    AuthorizedSourceRoute, CompleteContentSourceLocator, SourceAccessBroker, SourceSnapshot,
    VerifiedContentRouteStatus,
};

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

fn released_structured_locator_value(
    provider: CaptureProvider,
    ordinal: u64,
    subrecord: u32,
    native_id: &str,
) -> Vec<u8> {
    let provider = provider.as_str().as_bytes();
    let native_id = native_id.as_bytes();
    let mut value = Vec::with_capacity(4 + 1 + provider.len() + 8 + 4 + 2 + native_id.len());
    value.extend_from_slice(b"SC\0\x01");
    value.push(u8::try_from(provider.len()).unwrap());
    value.extend_from_slice(provider);
    value.extend_from_slice(&ordinal.to_be_bytes());
    value.extend_from_slice(&subrecord.to_be_bytes());
    value.extend_from_slice(&u16::try_from(native_id.len()).unwrap().to_be_bytes());
    value.extend_from_slice(native_id);
    value
}

#[allow(clippy::too_many_arguments)]
fn released_structured_result_locator_value(
    provider: CaptureProvider,
    ordinal: u64,
    source_subrecord: u32,
    history_item: u32,
    tool_state: u32,
    native_id: &str,
) -> Vec<u8> {
    let provider = provider.as_str().as_bytes();
    let native_id = native_id.as_bytes();
    let mut value =
        Vec::with_capacity(4 + 1 + provider.len() + 8 + 4 + 4 + 4 + 2 + native_id.len());
    value.extend_from_slice(b"SR\0\x01");
    value.push(u8::try_from(provider.len()).unwrap());
    value.extend_from_slice(provider);
    value.extend_from_slice(&ordinal.to_be_bytes());
    value.extend_from_slice(&source_subrecord.to_be_bytes());
    value.extend_from_slice(&history_item.to_be_bytes());
    value.extend_from_slice(&tool_state.to_be_bytes());
    value.extend_from_slice(&u16::try_from(native_id.len()).unwrap().to_be_bytes());
    value.extend_from_slice(native_id);
    value
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
    try_request(
        provider,
        path,
        source_root,
        provider_session_id,
        ordinal,
        subrecord,
        native_id,
        record_bytes,
        text,
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn try_request(
    provider: CaptureProvider,
    path: &Path,
    source_root: Option<&Path>,
    provider_session_id: &str,
    ordinal: u64,
    subrecord: u32,
    native_id: &str,
    record_bytes: &[u8],
    text: &str,
) -> std::result::Result<CompleteMessageRequest, CompleteContentError> {
    let indexed_limit_chars = 4;
    let event_id = Uuid::new_v4();
    let path = broker_test_path(path);
    let source_root = source_root.map(broker_test_path);
    let source_access = SourceAccessBroker::new().admit(
        AuthorizedSourceRoute {
            source_id: Uuid::new_v4(),
            provider,
            source_format: format_for(provider).to_owned(),
            family: CompleteContentSourceFamily::Structured,
            raw_source_path: path,
            source_root,
            source_identity: Some(format!("test:{}", provider.as_str())),
            source_snapshot: SourceSnapshot {
                size_bytes: Some(record_bytes.len() as u64),
                modified_at_ms: None,
                sha256: Some(digest_bytes(record_bytes)),
            },
        },
        event_id,
    )?;
    Ok(CompleteMessageRequest {
        event_id,
        provider,
        source_format: format_for(provider).to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Structured),
        content_profile: verified_content_profile(
            provider,
            format_for(provider),
            CompleteContentSourceFamily::Structured,
            VerifiedContentRole::MessageBody,
        )
        .unwrap()
        .to_owned(),
        source_locator: Some(
            CompleteContentSourceLocator::new(
                STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
                released_structured_locator_value(provider, ordinal, subrecord, native_id),
            )
            .unwrap(),
        ),
        provider_session_id: Some(provider_session_id.to_owned()),
        source_record_ordinal: ordinal,
        source_record_subrecord_index: subrecord,
        expected_provider_event_hash: native_id.to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(native_id.to_owned()),
        expected_record_digest: CompleteContentBodyDigest::parse(digest_bytes(record_bytes)),
        expected_content_ref: ContentRef::from_bytes(text.as_bytes()),
        indexed_text: text.chars().take(indexed_limit_chars).collect(),
        indexed_limit_chars,
    })
}

#[allow(clippy::too_many_arguments)]
fn result_request(
    provider: CaptureProvider,
    path: &Path,
    ordinal: u64,
    subrecord: u32,
    native_id: &str,
    record_bytes: &[u8],
    content: &str,
) -> ResultContentRequest {
    let event_id = Uuid::new_v4();
    let path = broker_test_path(path);
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider,
                source_format: format_for(provider).to_owned(),
                family: CompleteContentSourceFamily::Structured,
                raw_source_path: path.clone(),
                source_root: path.parent().map(Path::to_path_buf),
                source_identity: Some(format!("test:{}", provider.as_str())),
                source_snapshot: SourceSnapshot::default(),
            },
            event_id,
        )
        .unwrap();
    ResultContentRequest {
        event_id,
        provider,
        source_format: format_for(provider).to_owned(),
        source_access,
        source_family: CompleteContentSourceFamily::Structured,
        content_profile: verified_content_profile(
            provider,
            format_for(provider),
            CompleteContentSourceFamily::Structured,
            VerifiedContentRole::ResultBody,
        )
        .unwrap()
        .to_owned(),
        source_locator: CompleteContentSourceLocator::new(
            STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
            released_structured_locator_value(provider, ordinal, subrecord, native_id),
        )
        .unwrap(),
        source_record_ordinal: ordinal,
        source_record_subrecord_index: subrecord,
        expected_native_record_id: native_id.to_owned(),
        expected_record_digest: CompleteContentBodyDigest::parse(digest_bytes(record_bytes))
            .unwrap(),
        expected_content_ref: ContentRef::from_bytes(content.as_bytes()).unwrap(),
    }
}

fn broker_test_path(path: &Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    match path.strip_prefix("/var") {
        Ok(suffix) => Path::new("/private/var").join(suffix),
        Err(_) => path.to_path_buf(),
    }
    #[cfg(not(target_os = "macos"))]
    path.to_path_buf()
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
    let providers = VERIFIED_CONTENT_ROUTES
        .iter()
        .filter(|route| route.role == VerifiedContentRole::MessageBody)
        .map(|route| route.provider.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(providers.len(), 41);
    let supported = VERIFIED_CONTENT_ROUTES
        .iter()
        .filter(|route| {
            route.role == VerifiedContentRole::MessageBody
                && verified_content_route_supported(
                    route.provider,
                    route.source_format,
                    CompleteContentSourceFamily::Structured,
                    route.role,
                )
        })
        .map(|route| route.provider.as_str())
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
    assert!(VERIFIED_CONTENT_ROUTES.iter().all(|entry| {
        entry.platform_dispositions.iter().all(|disposition| {
            disposition.status == VerifiedContentRouteStatus::Supported
                || !disposition.reason.is_empty()
        })
    }));
}

#[test]
fn decodes_released_structured_locator_versions() {
    let message = released_structured_locator_value(CaptureProvider::RovoDev, 9, 3, "native-12");
    assert_eq!(
        decode_structured_locator(&message).unwrap(),
        (CaptureProvider::RovoDev, 9, 3, "native-12".to_owned())
    );

    let result = released_structured_result_locator_value(
        CaptureProvider::Continue,
        11,
        2,
        7,
        4,
        "history-7:tool-4:result",
    );
    assert_eq!(
        decode_structured_result_locator(&result).unwrap(),
        (
            CaptureProvider::Continue,
            11,
            2,
            7,
            4,
            "history-7:tool-4:result".to_owned(),
        )
    );
}

#[test]
fn resolves_exact_structured_result_subrecords() {
    let directory = TempDir::new().unwrap();
    let content = "structured result λ\nsecond line";
    let rovo_bytes = serde_json::to_vec(&json!({
        "message_history": [{
            "id": "rovo-result-1",
            "role": "user",
            "content": [{"type": "tool_result", "content": content}]
        }]
    }))
    .unwrap();
    let rovo_path = directory.path().join("session_context.json");
    write(&rovo_path, &rovo_bytes);

    let mut request = result_request(
        CaptureProvider::RovoDev,
        &rovo_path,
        0,
        0,
        "rovo-result-1",
        &rovo_bytes,
        content,
    );
    let resolved = ResultContentResolver::resolve_results(
        &StructuredCompleteContentResolver::new(),
        std::slice::from_ref(&request),
    );
    assert_eq!(resolved[0].as_ref().unwrap().content, content);

    let mut wrong_subrecord = request.clone();
    wrong_subrecord.source_record_subrecord_index = 1;
    assert_eq!(
        ResultContentResolver::resolve_results(
            &StructuredCompleteContentResolver::new(),
            &[wrong_subrecord]
        )[0]
        .as_ref()
        .unwrap_err()
        .kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    fs::write(&rovo_path, b"{\"message_history\":[]}").unwrap();
    request.source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: request.provider,
                source_format: request.source_format.clone(),
                family: CompleteContentSourceFamily::Structured,
                raw_source_path: rovo_path.clone(),
                source_root: rovo_path.parent().map(Path::to_path_buf),
                source_identity: Some("test:rovo-dev".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            request.event_id,
        )
        .unwrap();
    assert_eq!(
        ResultContentResolver::resolve_results(
            &StructuredCompleteContentResolver::new(),
            &[request]
        )[0]
        .as_ref()
        .unwrap_err()
        .kind,
        CompleteContentErrorKind::SourceChanged
    );
}

#[test]
fn resolves_openhands_and_task_json_results_by_native_record_identity() {
    let directory = TempDir::new().unwrap();
    let openhands_content = "OpenHands stdout\n";
    let openhands_bytes = serde_json::to_vec(&json!({
        "id": "openhands-result-1",
        "timestamp": "2026-07-22T12:00:00Z",
        "kind": "ObservationEvent",
        "source": "environment",
        "observation": {
            "kind": "ExecuteBashObservation",
            "content": openhands_content
        }
    }))
    .unwrap();
    let openhands_path = directory
        .path()
        .join("profile/v1_conversations/session/events/result.json");
    write(&openhands_path, &openhands_bytes);
    let openhands = result_request(
        CaptureProvider::OpenHands,
        &openhands_path,
        17,
        0,
        "openhands-result-1",
        &openhands_bytes,
        openhands_content,
    );
    assert_eq!(
        ResultContentResolver::resolve_results(
            &StructuredCompleteContentResolver::new(),
            &[openhands]
        )[0]
        .as_ref()
        .unwrap()
        .content,
        openhands_content
    );

    let task_content = "task command output";
    let raw_record = format!(
        r#"{{"id":"task-result-1","type":"command","text":{}}}"#,
        serde_json::to_string(task_content).unwrap()
    );
    let task_file = format!("[\n {raw_record}\n]").into_bytes();
    let task_path = directory.path().join("tasks/cline/ui_messages.json");
    write(&task_path, &task_file);
    let task = result_request(
        CaptureProvider::Cline,
        &task_path,
        4,
        0,
        "cline-task:ui_messages:task-result-1",
        raw_record.as_bytes(),
        task_content,
    );
    assert_eq!(
        ResultContentResolver::resolve_results(&StructuredCompleteContentResolver::new(), &[task])
            [0]
        .as_ref()
        .unwrap()
        .content,
        task_content
    );
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
fn continue_message_locator_uses_interleaved_citation_and_native_identity() {
    let directory = TempDir::new().unwrap();
    let text = "message after separately normalized result";
    let bytes = serde_json::to_vec(&json!({
        "history": [
            {
                "id": "call-item",
                "message": {"role": "assistant", "content": ""},
                "toolCallStates": [{
                    "toolCallId": "call-1",
                    "toolCall": {"function": {"name": "readFile"}},
                    "output": "result"
                }]
            },
            {"id": "message-item", "message": {"role": "user", "content": text}}
        ]
    }))
    .unwrap();
    let path = directory.path().join("continue-session.json");
    write(&path, &bytes);
    // Canonical subrecord 0 is the call, 1 its result, and 2 this message;
    // native history index remains 1 and is recovered by message identity.
    assert_eq!(
        resolve_one_message(request(
            CaptureProvider::Continue,
            &path,
            None,
            "continue-session",
            0,
            2,
            "message-item",
            &bytes,
            text,
        ))
        .text,
        text
    );
}

#[test]
fn continue_message_without_native_id_uses_released_payload_hash_authority() {
    let directory = TempDir::new().unwrap();
    let text = "Continue fallback-hash message";
    let item = json!({"message": {"role": "user", "content": text}});
    let bytes = serde_json::to_vec(&json!({"history": [item.clone()]})).unwrap();
    let path = directory.path().join("continue-fallback.json");
    write(&path, &bytes);

    let mut hydration = request(
        CaptureProvider::Continue,
        &path,
        None,
        "continue-fallback",
        0,
        0,
        "history:continue-fallback:0",
        &bytes,
        text,
    );
    let (event_type, payload) =
        crate::provider::providers::continue_cli::continue_history_item_canonical_payload(&item);
    assert_eq!(event_type, EventType::Message);
    hydration.expected_hash_authority = CompleteContentHashAuthority::NormalizedPayloadFallback;
    hydration.expected_provider_event_hash = crate::compute_payload_hash(&payload).unwrap();

    assert_eq!(resolve_one_message(hydration).text, text);
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
    let configured_root = broker_test_path(&moved_root);
    let profile_text = format!(
        "// stable + insiders profiles\n{{ profiles: [{{ storagePath: {}, }},], }}",
        serde_json::to_string(configured_root.to_str().unwrap()).unwrap()
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
    let escaped_root = broker_test_path(&profile_root)
        .to_str()
        .unwrap()
        .replace('&', "&amp;");
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
    let failure = try_request(
        CaptureProvider::RovoDev,
        &profile,
        None,
        "xml-session",
        0,
        0,
        "xml-1",
        &bytes,
        text,
    )
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
    wrong_body.expected_content_ref = ContentRef::from_bytes(b"different");
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
    let stable = StructuredCompleteContentResolver::new()
        .resolve(&[mutation_request])
        .unwrap();
    assert_eq!(stable[0].text, text);
    let changed = try_request(
        CaptureProvider::RovoDev,
        &path,
        None,
        "session",
        0,
        0,
        "stable-1",
        &bytes,
        text,
    )
    .unwrap();
    assert_eq!(
        StructuredCompleteContentResolver::new()
            .resolve(&[changed])
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
    second_request.source_access = first_request.source_access.clone();
    second_request.expected_content_ref = ContentRef::from_bytes(b"wrong");
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
    let linked_error = try_request(
        CaptureProvider::RovoDev,
        &link,
        None,
        "link-session",
        0,
        0,
        "link-1",
        &bytes,
        text,
    )
    .unwrap_err();
    assert_eq!(linked_error.kind, CompleteContentErrorKind::SourceChanged);

    let traversal = real.join("subdir/../session_context.json");
    let traversal_error = try_request(
        CaptureProvider::RovoDev,
        &traversal,
        None,
        "link-session",
        0,
        0,
        "link-1",
        &bytes,
        text,
    )
    .unwrap_err();
    assert_eq!(
        traversal_error.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

#[cfg(unix)]
struct StructuredAdmissionHookReset;

#[cfg(unix)]
impl Drop for StructuredAdmissionHookReset {
    fn drop(&mut self) {
        set_structured_admission_test_hook(None);
    }
}

#[cfg(unix)]
fn install_structured_admission_hook(
    hook: impl FnMut(&Path, StructuredAdmissionTestStage) + 'static,
) -> StructuredAdmissionHookReset {
    set_structured_admission_test_hook(Some(Box::new(hook)));
    StructuredAdmissionHookReset
}

#[cfg(unix)]
#[test]
fn rejects_root_replacement_after_capability_admission() {
    let directory = TempDir::new().unwrap();
    let root = directory.path().join("root");
    let moved = directory.path().join("moved-root");
    let replacement = directory.path().join("replacement-root");
    let text = "original root content";
    let bytes = serde_json::to_vec(&json!({
        "message_history": [{"id": "root-race", "content": text}]
    }))
    .unwrap();
    write(&root.join("session_context.json"), &bytes);
    write(
        &replacement.join("session_context.json"),
        br#"{"message_history":[{"id":"root-race","content":"replacement"}]}"#,
    );
    let root_for_hook = broker_test_path(&root);
    let moved_for_hook = broker_test_path(&moved);
    let replacement_for_hook = broker_test_path(&replacement);
    let mut fired = false;
    let _reset = install_structured_admission_hook(move |path, stage| {
        if !fired && stage == StructuredAdmissionTestStage::RootOpened && path == root_for_hook {
            fs::rename(&root_for_hook, &moved_for_hook).unwrap();
            fs::rename(&replacement_for_hook, &root_for_hook).unwrap();
            fired = true;
        }
    });
    let failure = try_request(
        CaptureProvider::RovoDev,
        &root,
        None,
        "root-race-session",
        0,
        0,
        "root-race",
        &bytes,
        text,
    )
    .unwrap_err();
    assert_eq!(failure.kind, CompleteContentErrorKind::SourceChanged);
}

#[cfg(unix)]
#[test]
fn rejects_ancestor_replacement_after_root_capability_admission() {
    let directory = TempDir::new().unwrap();
    let parent = directory.path().join("parent");
    let moved_parent = directory.path().join("moved-parent");
    let root = parent.join("root");
    let text = "original ancestor content";
    let bytes = serde_json::to_vec(&json!({
        "message_history": [{"id": "ancestor-race", "content": text}]
    }))
    .unwrap();
    write(&root.join("session_context.json"), &bytes);
    let root_for_hook = broker_test_path(&root);
    let parent_for_hook = broker_test_path(&parent);
    let moved_for_hook = broker_test_path(&moved_parent);
    let mut fired = false;
    let _reset = install_structured_admission_hook(move |path, stage| {
        if !fired && stage == StructuredAdmissionTestStage::RootOpened && path == root_for_hook {
            fs::rename(&parent_for_hook, &moved_for_hook).unwrap();
            write(
                &parent_for_hook.join("root/session_context.json"),
                br#"{"message_history":[{"id":"ancestor-race","content":"replacement"}]}"#,
            );
            fired = true;
        }
    });
    let failure = try_request(
        CaptureProvider::RovoDev,
        &root,
        None,
        "ancestor-race-session",
        0,
        0,
        "ancestor-race",
        &bytes,
        text,
    )
    .unwrap_err();
    assert_eq!(failure.kind, CompleteContentErrorKind::SourceChanged);
}

#[cfg(unix)]
#[test]
fn rejects_child_replacement_after_descriptor_relative_open() {
    let directory = TempDir::new().unwrap();
    let root = directory.path().join("root");
    let target = root.join("session_context.json");
    let moved = root.join("moved-original.json");
    let replacement = directory.path().join("replacement.json");
    let text = "original child content";
    let bytes = serde_json::to_vec(&json!({
        "message_history": [{"id": "child-race", "content": text}]
    }))
    .unwrap();
    write(&target, &bytes);
    write(
        &replacement,
        br#"{"message_history":[{"id":"child-race","content":"replacement"}]}"#,
    );
    let target_for_hook = broker_test_path(&target);
    let moved_for_hook = broker_test_path(&moved);
    let replacement_for_hook = broker_test_path(&replacement);
    let mut fired = false;
    let _reset = install_structured_admission_hook(move |path, stage| {
        if !fired && stage == StructuredAdmissionTestStage::ChildOpened && path == target_for_hook {
            fs::rename(&target_for_hook, &moved_for_hook).unwrap();
            fs::rename(&replacement_for_hook, &target_for_hook).unwrap();
            fired = true;
        }
    });
    let failure = try_request(
        CaptureProvider::RovoDev,
        &root,
        None,
        "child-race-session",
        0,
        0,
        "child-race",
        &bytes,
        text,
    )
    .unwrap_err();
    assert_eq!(failure.kind, CompleteContentErrorKind::SourceChanged);
}

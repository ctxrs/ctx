use std::{fs, io::Write};

use ctx_history_core::ContentRef;
use serde_json::{json, Value};

use super::*;
use crate::complete_content::{
    verified_content_profile_for_locator, AuthorizedSourceRoute, CompleteContentSourceLocator,
    SourceAccessBroker, SourceSnapshot,
};
use crate::test_support_paths::tempdir;

fn long_message(label: &str) -> String {
    format!("{}-{label}", "m".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 8))
}

fn message_value(id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "role": "assistant",
        "parts": [{"type": "text", "text": text}],
        "metadata": {"historySequence": 0},
        "workspaceId": "mux-locator-test"
    })
}

fn message_request(
    path: &std::path::Path,
    source_format: &str,
    locator_kind: &str,
    locator_value: Vec<u8>,
    record: &[u8],
    body: &str,
    native_record_id: &str,
) -> CompleteMessageRequest {
    let event_id = uuid::Uuid::new_v4();
    let source_snapshot = SourceSnapshot {
        size_bytes: Some(fs::metadata(path).unwrap().len()),
        ..SourceSnapshot::default()
    };
    let source_access = SourceAccessBroker::new(crate::test_provider_sqlite_data_root())
        .admit(
            AuthorizedSourceRoute {
                source_id: uuid::Uuid::new_v4(),
                provider: CaptureProvider::Mux,
                source_format: source_format.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: path.to_path_buf(),
                source_root: path.parent().map(std::path::Path::to_path_buf),
                source_identity: Some("mux-test-source".to_owned()),
                source_snapshot,
            },
            event_id,
        )
        .unwrap();
    CompleteMessageRequest {
        event_id,
        provider: CaptureProvider::Mux,
        source_format: source_format.to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: verified_content_profile_for_locator(
            CaptureProvider::Mux,
            source_format,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
            locator_kind,
        )
        .unwrap()
        .to_owned(),
        source_locator: CompleteContentSourceLocator::new(locator_kind, locator_value),
        provider_session_id: Some("mux-locator-test".to_owned()),
        source_record_ordinal: 0,
        source_record_subrecord_index: 0,
        expected_provider_event_hash: native_record_id.to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(native_record_id.to_owned()),
        expected_record_digest: Some(digest_bytes(record)),
        expected_content_ref: ContentRef::from_bytes(body.as_bytes()),
        indexed_text: body.chars().take(crate::PROVIDER_MAX_TEXT_CHARS).collect(),
        indexed_limit_chars: crate::PROVIDER_MAX_TEXT_CHARS,
    }
}

#[test]
fn chat_message_survives_append_but_not_record_rewrite() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("chat.jsonl");
    let body = long_message("chat");
    let record = serde_json::to_vec(&message_value("chat-message", &body)).unwrap();
    let mut source = record.clone();
    source.push(b'\n');
    fs::write(&path, &source).unwrap();
    let range = JsonlRange {
        byte_start: 0,
        byte_end_exclusive: source.len() as u64,
    };
    let mut request = message_request(
        &path,
        MUX_SOURCE_FORMAT,
        MUX_LOCATOR_KIND,
        MuxAddress::Chat(range).encode().to_vec(),
        &record,
        &body,
        "chat-message",
    );

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(file, "{}", json!({"id": "later", "role": "user"})).unwrap();
    file.sync_all().unwrap();
    drop(file);
    request.source_access = SourceAccessBroker::new(crate::test_provider_sqlite_data_root())
        .admit(
            AuthorizedSourceRoute {
                source_id: uuid::Uuid::new_v4(),
                provider: CaptureProvider::Mux,
                source_format: MUX_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: path.clone(),
                source_root: path.parent().map(std::path::Path::to_path_buf),
                source_identity: Some("mux-test-source".to_owned()),
                source_snapshot: SourceSnapshot {
                    size_bytes: Some(source.len() as u64),
                    ..SourceSnapshot::default()
                },
            },
            request.event_id,
        )
        .unwrap();
    assert_eq!(
        resolve_messages(std::slice::from_ref(&request)).unwrap()[0].text,
        body
    );

    let mut wrong_native_id = request.clone();
    wrong_native_id.expected_native_record_id = Some("different-message".to_owned());
    assert_eq!(
        resolve_messages(&[wrong_native_id]).unwrap_err().kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    let changed =
        serde_json::to_vec(&message_value("chat-message", &long_message("changed"))).unwrap();
    let mut changed_source = changed;
    changed_source.push(b'\n');
    fs::write(&path, changed_source).unwrap();
    assert_eq!(
        resolve_messages(&[request]).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );
}

#[test]
fn partial_message_fails_closed_after_snapshot_rewrite() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("partial.json");
    let body = long_message("partial");
    let record = serde_json::to_vec(&message_value("partial-message", &body)).unwrap();
    fs::write(&path, &record).unwrap();
    let request = message_request(
        &path,
        MUX_SOURCE_FORMAT,
        MUX_LOCATOR_KIND,
        MuxAddress::Partial {
            byte_len: record.len() as u64,
        }
        .encode()
        .to_vec(),
        &record,
        &body,
        "partial:partial-message",
    );
    assert_eq!(
        resolve_messages(std::slice::from_ref(&request)).unwrap()[0].text,
        body
    );

    fs::write(
        &path,
        serde_json::to_vec(&message_value("partial-message", &long_message("new"))).unwrap(),
    )
    .unwrap();
    assert_eq!(
        resolve_messages(&[request]).unwrap_err().kind,
        CompleteContentErrorKind::SourceChanged
    );
}

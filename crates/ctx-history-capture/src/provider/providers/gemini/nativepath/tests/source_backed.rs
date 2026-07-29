use ctx_history_core::{
    AgentType, CaptureProvider, LocatorRevisionPolicy, NativeRecordCoordinate, TypedKey,
};
use sha2::{Digest, Sha256};

use super::*;

#[test]
fn gemini_source_backed_cold_projection_is_stable_bounded_and_certified() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let long_message = format!(
        "gemini full sentinel {} gemini-tail-sentinel",
        "界".repeat(4_096)
    );
    let path = write_transcript(
        &root,
        &[
            header("source-backed-cold", "main"),
            json!({
                "id": "user-cold",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": long_message
            }),
            json!({
                "id": "state-cold",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "$set": {"summary": "cold certified state"}
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let mut reader = GeminiSourceBackedLeafReader::open(&source).unwrap();
    assert_eq!(reader.source().provider(), CaptureProvider::Gemini.as_str());
    assert_eq!(
        reader.source().source_format(),
        crate::GEMINI_CLI_SOURCE_FORMAT
    );
    assert_eq!(reader.session().native_session_id, "source-backed-cold");
    let source_id = reader.source().identity();
    let session_id = reader.session_id();
    let mut documents = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        assert!(page.documents.len() <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.expected_prefix_bytes <= page.next_prefix_bytes);
        assert_ne!(page.page_identity, [0; 32]);
        for document in &page.documents {
            assert!(!document.body.is_empty());
            assert_eq!(document.source.identity(), source_id);
            assert_eq!(document.session_id, session_id);
        }
        documents.extend(page.documents);
    }
    let leaf = reader.finish().unwrap();

    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0].body, long_message);
    assert!(documents[0].body.ends_with("gemini-tail-sentinel"));
    assert_eq!(leaf.source.identity(), source_id);
    assert_eq!(leaf.session_id, session_id);
    assert_eq!(leaf.parent_session_id, None);
    assert_eq!(leaf.root_session_id, session_id);
    assert_eq!(leaf.session.native_session_id, "source-backed-cold");
    assert_eq!(leaf.certificate.counts().complete_records, 3);
    assert_eq!(leaf.certificate.counts().retained_records, 2);
    assert_eq!(leaf.certificate.counts().rejected_records, 0);
    assert_eq!(leaf.certificate.counts().ignored_records, 1);
    assert_eq!(leaf.certificate.counts().indexed_documents, 2);
    assert_eq!(
        leaf.certificate.counts().certified_bytes,
        fs::metadata(&path).unwrap().len()
    );
    assert_eq!(
        leaf.certificate
            .frontier()
            .unwrap()
            .certified_prefix_bytes(),
        fs::metadata(&path).unwrap().len()
    );

    let mut replay = GeminiSourceBackedLeafReader::open(&source).unwrap();
    let mut replayed = Vec::new();
    while let Some(page) = replay.next_page().unwrap() {
        replayed.extend(page.documents);
    }
    let replay_leaf = replay.finish().unwrap();
    assert_eq!(replay_leaf.source.identity(), source_id);
    assert_eq!(replay_leaf.session_id, session_id);
    assert_eq!(
        replayed
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>(),
        documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        replayed
            .iter()
            .map(|document| document.locator.clone())
            .collect::<Vec<_>>(),
        documents
            .iter()
            .map(|document| document.locator.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn gemini_source_backed_projects_bounded_subagent_lineage_and_filters() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = root.join("tmp/project/chats/root-thread/child-thread.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        jsonl(&[
            header("child-thread", "subagent"),
            json!({
                "id": "child-message",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "content": "child lineage sentinel"
            }),
        ]),
    )
    .unwrap();
    let source = rediscover(&root, &path);

    let mut reader = GeminiSourceBackedLeafReader::open(&source).unwrap();
    let page = reader.next_page().unwrap().unwrap();
    assert_eq!(page.documents.len(), 1);
    let document = &page.documents[0];
    let parent_session_id = document.parent_session_id.unwrap();
    assert_eq!(document.root_session_id, parent_session_id);
    assert_ne!(document.session_id, parent_session_id);
    assert_eq!(
        document.provider_session_id.as_deref(),
        Some("child-thread")
    );
    assert_eq!(document.branch, None);
    let expected_source_path = source.path.to_string_lossy().into_owned();
    assert_eq!(
        document.source_path.as_deref(),
        Some(expected_source_path.as_str())
    );
    assert_eq!(document.agent_type, AgentType::Subagent.as_str());
    assert!(!document.is_primary);
    assert!(reader.next_page().unwrap().is_none());
    let leaf = reader.finish().unwrap();
    assert_eq!(leaf.parent_session_id, Some(parent_session_id));
    assert_eq!(leaf.root_session_id, parent_session_id);
}

#[test]
fn gemini_source_backed_exact_jsonl_locator_reopens_original_record_after_append() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let exact_text = "Gemini snowman ☃, quote \"exact\", path C:\\tmp";
    let path = write_transcript(
        &root,
        &[
            header("source-backed-exact", "main"),
            json!({
                "id": "exact-message",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": exact_text
            }),
        ],
    );
    let original = fs::read(&path).unwrap();
    let message_offset = original
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|offset| offset + 1)
        .unwrap();
    let exact_record = original[message_offset..].to_vec();
    let source = rediscover(&root, &path);
    let mut reader = GeminiSourceBackedLeafReader::open(&source).unwrap();
    let mut documents = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        documents.extend(page.documents);
    }
    reader.finish().unwrap();
    assert_eq!(documents.len(), 1);
    let document = &documents[0];

    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = document.locator.coordinate()
    else {
        panic!("expected a JSONL locator");
    };
    assert_eq!(*byte_offset, message_offset as u64);
    assert_eq!(*byte_length, exact_record.len() as u64);
    assert_eq!(*physical_ordinal, 1);
    assert_eq!(
        native_session_key.as_ref(),
        Some(&TypedKey::Utf8("source-backed-exact".to_owned()))
    );
    assert_eq!(
        native_event_key.as_ref(),
        Some(&TypedKey::Utf8("exact-message".to_owned()))
    );
    assert_eq!(
        document.locator.revision_policy(),
        LocatorRevisionPolicy::StableRecordEvidence
    );
    let exact_record_digest: [u8; 32] = Sha256::digest(&exact_record).into();
    assert_eq!(document.locator.record_digest(), &exact_record_digest);

    let hydrated = hydrate_gemini_source_backed_record(&source, &document.locator).unwrap();
    assert_eq!(hydrated.provider_bytes, exact_text.as_bytes());
    assert_eq!(hydrated.decoded_display_text.as_deref(), Some(exact_text));

    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&jsonl(&[json!({
            "id": "appended-message",
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "gemini",
            "content": "later append"
        })]))
        .unwrap();
    let appended_source = rediscover(&root, &path);
    let hydrated_after_append =
        hydrate_gemini_source_backed_record(&appended_source, &document.locator).unwrap();
    assert_eq!(hydrated_after_append.provider_bytes, exact_text.as_bytes());
    assert_eq!(
        hydrated_after_append.decoded_display_text.as_deref(),
        Some(exact_text)
    );
}

#[test]
fn source_backed_gemini_adapter_has_no_preview_or_store_body_fallback() {
    let source = include_str!("../source_backed.rs");
    assert!(!source.contains("MAX_BODY_PREVIEW_CHARS"));
    assert!(!source.contains("ctx_history_store"));
    assert!(!source.contains("event.preview"));
}

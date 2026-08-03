use super::*;

use std::path::Path;

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceAppend, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceFrontier, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};

fn source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_test",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8("jsonl-pin").unwrap())
            .unwrap(),
    )
    .unwrap()
}

fn record(source: &SourceKey, nonce: u64, body: &str) -> CoreRecord {
    let session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let item_key = NativeItemKey::native_id("message", TypedKey::U64(nonce)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        nonce,
        "message",
        "primary",
        true,
        "event-export-test-v1",
        body,
    )
    .unwrap();
    record.occurred_at_unix_ms = Some(1_000 + nonce as i64);
    record
}

fn publish(root: &Path, revision: u8, source: &SourceKey, records: &[CoreRecord]) {
    let certificate = certificate(source, revision, records.len());
    let mut writer = GenerationWriter::open(root, WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer.certify_source(certificate).unwrap();
    writer.commit(|_| true).unwrap();
}

fn certificate(source: &SourceKey, revision: u8, records: usize) -> CertifiedSource {
    let observation =
        SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "event-export-test-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: records as u64,
            retained_records: records as u64,
            indexed_documents: records as u64,
            certified_bytes: records as u64 * 10,
            ..ScannedSourceCounts::default()
        },
        Some(
            SourceFrontier::new(
                "jsonl-byte-offset",
                TypedKey::U64(records as u64 * 10),
                records as u64 * 10,
                [revision; 32],
            )
            .unwrap(),
        ),
    )
    .unwrap()
}

fn append(root: &Path, revision: u8, source: &SourceKey, prior: usize, record: CoreRecord) {
    let mut writer = GenerationWriter::open(root, WriterOptions::default()).unwrap();
    let base = writer.begin_source_append(source.clone()).unwrap().clone();
    writer.add_core_record(record).unwrap();
    let frontier = base.frontier().unwrap();
    let proof = CertifiedSourceAppend::certify(
        &base,
        certificate(source, revision, prior + 1),
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .unwrap();
    writer.certify_source_append(proof).unwrap();
    writer.commit(|_| true).unwrap();
}

#[test]
fn exact_page_usage_counts_the_final_newline() {
    let encoded = encode_page(
        &"a".repeat(64),
        &[serde_json::json!({"ctx_event_id": "event"})],
        None,
        true,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(value["usage"]["items"], 1);
    assert_eq!(value["usage"]["bytes"], encoded.len());
    assert_eq!(encoded.last(), Some(&b'\n'));
}

#[test]
fn exact_page_usage_converges_across_digit_boundaries() {
    for count in [0, 1, 9, 10, 99, 100] {
        let events = (0..count)
            .map(|index| serde_json::json!({"ctx_event_id": index, "text": "雪"}))
            .collect::<Vec<_>>();
        let encoded = encode_page(&"b".repeat(64), &events, Some("cursor"), false).unwrap();
        let value: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["usage"]["items"], count);
        assert_eq!(value["usage"]["bytes"], encoded.len());
    }
}

#[test]
fn jsonl_holds_one_pin_across_append_rewrite_and_delete_publications() {
    let temp = tempfile::tempdir().unwrap();
    let index_root = temp.path().join("search/lexical");
    let source = source();
    let original = (0..5)
        .map(|nonce| record(&source, nonce, &format!("old-{nonce}")))
        .collect::<Vec<_>>();
    publish(&index_root, 1, &source, &original);
    let held = VerifiedIndex::open(&index_root).unwrap();
    let selection = CoreEventRangeSelection::new(1_000, 2_000, Vec::<String>::new()).unwrap();
    let replacement = (10..13)
        .map(|nonce| record(&source, nonce, &format!("new-{nonce}")))
        .collect::<Vec<_>>();
    let mut publications = 0;
    let mut output = Vec::new();
    write_jsonl_pages(
        &held,
        &selection,
        None,
        None,
        1,
        64 * 1024,
        &mut output,
        || {
            if publications == 0 {
                append(
                    &index_root,
                    2,
                    &source,
                    original.len(),
                    record(&source, 99, "appended"),
                );
                publish(&index_root, 3, &source, &replacement);
                publish(&index_root, 4, &source, &[]);
            }
            publications += 1;
        },
    )
    .unwrap();
    let ids = output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let value: Value = serde_json::from_slice(line).unwrap();
            value["ctx_event_id"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    let expected = original
        .iter()
        .map(|record| record.event_id.as_uuid().to_string())
        .collect::<Vec<_>>();
    assert_eq!(ids, expected);
    assert!(publications >= original.len());
}

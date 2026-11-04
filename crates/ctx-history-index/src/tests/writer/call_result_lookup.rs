use super::*;
use ctx_history_core::{ActivityJsonCapture, ActivityResult, ActivityTextCapture};

#[test]
fn result_call_lookup_is_exact_source_scoped_and_generation_owned() {
    let temp = tempdir().unwrap();
    let source = source("item-id-journal.jsonl");
    let call_id = TypedKey::utf8("shared-call").unwrap();
    let mut result = document(&source, 1, "output");
    let session = result.session_id;
    let activity = result.content.activity.as_mut().unwrap();
    activity.provider_call_id = Some(call_id.clone());
    activity.result = Some(ActivityResult {
        status: None,
        completed_at_unix_ms: None,
        duration_ns: None,
        text: ActivityTextCapture::NormalizedBody,
        structured_content: ActivityJsonCapture::Absent,
    });
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(result.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = writer.base_event_identity_lookup();
    assert!(base.contains(result.event_id.as_uuid()).unwrap());
    assert!(base.contains_call_result(session, &call_id).unwrap());
    assert!(!base
        .contains_call_result(session, &TypedKey::utf8("different-call").unwrap())
        .unwrap());
    let other_session = document_for_session(&source, "another-session", 1, "body").session_id;
    assert!(!base.contains_call_result(other_session, &call_id).unwrap());
    drop(writer);

    // Replacement removes the membership from the new generation, while an
    // already captured lookup continues to describe its immutable base.
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "replacement without a result"))
        .unwrap();
    writer.certify_source(certificate(&source, 2, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(!writer
        .base_event_identity_lookup()
        .contains_call_result(session, &call_id)
        .unwrap());
    assert!(base.contains_call_result(session, &call_id).unwrap());
}

#[test]
fn missing_projection_and_oversized_result_keys_cannot_prove_absence() {
    let temp = tempdir().unwrap();
    let source = source("call-key-boundary.jsonl");
    let session = document(&source, 1, "output").session_id;
    let overhead = session.encode_canonical().unwrap().len()
        + serde_json::to_vec(&TypedKey::Utf8(String::new()))
            .unwrap()
            .len();
    let max_chars = tantivy::tokenizer::MAX_TOKEN_LEN - overhead;
    let ids = [
        TypedKey::Utf8("a".repeat(max_chars)),
        TypedKey::Utf8("a".repeat(max_chars + 1)),
        TypedKey::Bytes(vec![255; 20_000]),
    ];
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for (index, call_id) in ids.iter().enumerate() {
        let mut result = document(&source, index as u64 + 1, "retained output");
        let activity = result.content.activity.as_mut().unwrap();
        activity.provider_call_id = Some(call_id.clone());
        activity.result = Some(ActivityResult {
            status: None,
            completed_at_unix_ms: None,
            duration_ns: None,
            text: ActivityTextCapture::NormalizedBody,
            structured_content: ActivityJsonCapture::Absent,
        });
        writer.add_core_record(result).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let mut lookup = writer.base_event_identity_lookup();
    for call_id in ids {
        assert!(lookup.contains_call_result(session, &call_id).unwrap());
    }
    // The largest supported term is exact; the next byte is unknown even for
    // an ID that was never indexed. No query may infer absence from truncation.
    let absent = TypedKey::Utf8("b".repeat(max_chars));
    assert!(!lookup.contains_call_result(session, &absent).unwrap());
    assert!(lookup
        .contains_call_result(session, &TypedKey::Utf8("b".repeat(max_chars + 1)))
        .unwrap());
    lookup.provider_call_result_field = None;
    assert!(lookup.contains_call_result(session, &absent).unwrap());
}

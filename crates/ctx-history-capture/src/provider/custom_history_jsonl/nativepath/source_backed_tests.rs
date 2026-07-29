use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{
    ContentSourceResolver, EventHydrationRequest, HydrationFailureKind, NativeRecordCoordinate,
    SessionHydrationRequest, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde_json::{json, Value};

use super::source_backed::*;
use crate::{test_support_paths::tempdir, MAX_PROVIDER_JSONL_LINE_BYTES};

fn manifest() -> Value {
    json!({
        "record_type": "manifest",
        "schema_version": "ctx-history-jsonl-v1",
        "producer": "source-backed-test",
    })
}

fn source() -> Value {
    json!({
        "record_type": "source",
        "source_id": "source-a",
        "provider_key": "demo-agent",
        "source_format": "demo-jsonl",
        "raw_source_path": "/provider/demo/session.jsonl",
    })
}

fn session(id: &str, parent: Option<&str>, is_primary: bool) -> Value {
    json!({
        "record_type": "session",
        "source_id": "source-a",
        "session_id": id,
        "parent_session_id": parent,
        "agent_type": if is_primary { "primary" } else { "subagent" },
        "is_primary": is_primary,
        "started_at": "2026-07-28T12:00:00Z",
        "cwd": "/work/custom-history",
    })
}

fn event(index: u64, id: &str, session_id: &str, text: &str) -> Value {
    json!({
        "record_type": "event",
        "source_id": "source-a",
        "session_id": session_id,
        "event_index": index,
        "event_id": id,
        "event_type": "message",
        "role": if index.is_multiple_of(2) { "user" } else { "assistant" },
        "occurred_at": format!("2026-07-28T12:00:{:02}Z", index.min(59)),
        "payload": {"text": text},
    })
}

fn touch(index: u64, event_index: u64, path: &str) -> Value {
    json!({
        "record_type": "file_touch",
        "source_id": "source-a",
        "session_id": "child",
        "touch_index": index,
        "event_index": event_index,
        "path": path,
        "occurred_at": "2026-07-28T12:01:00Z",
    })
}

fn write_records(path: &Path, records: &[Value]) -> Vec<Vec<u8>> {
    let lines = records
        .iter()
        .map(|record| {
            let mut line = serde_json::to_vec(record).unwrap();
            line.push(b'\n');
            line
        })
        .collect::<Vec<_>>();
    let bytes = lines.iter().flatten().copied().collect::<Vec<_>>();
    fs::write(path, bytes).unwrap();
    lines
}

fn append_record(path: &Path, record: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(record).unwrap();
    line.push(b'\n');
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&line).unwrap();
    file.sync_all().unwrap();
    line
}

fn collect(
    input: &CustomHistorySourceBackedInput,
    prior: Option<&ctx_history_core::CertifiedSource>,
) -> (
    CustomHistorySourceBackedOutcome,
    Vec<LexicalDocument>,
    Vec<usize>,
) {
    let mut documents = Vec::new();
    let mut page_bounds = Vec::new();
    let outcome = scan_custom_history_source_backed_explicit(input, prior, |page| {
        page_bounds.push(page.documents.len());
        documents.extend(page.documents);
        Ok(())
    })
    .unwrap();
    (outcome, documents, page_bounds)
}

fn present(outcome: CustomHistorySourceBackedOutcome) -> CustomHistorySourceBackedReceipt {
    match outcome {
        CustomHistorySourceBackedOutcome::Present(receipt) => receipt,
        CustomHistorySourceBackedOutcome::Missing { .. } => panic!("expected present source"),
    }
}

#[test]
fn cold_noop_and_append_emit_stable_ids_in_bounded_pages() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("explicit.jsonl");
    let long = format!(
        "full-body-sentinel-{}-custom-tail-sentinel",
        "x".repeat(8_192)
    );
    let mut records = vec![
        manifest(),
        source(),
        session("root", None, true),
        session("child", Some("root"), false),
    ];
    for index in 0..70 {
        records.push(event(
            index,
            &format!("event-{index}"),
            "child",
            if index == 0 { &long } else { "ordinary" },
        ));
    }
    write_records(&path, &records);
    let input = CustomHistorySourceBackedInput::explicit(&path, [7; 32]);

    let (cold_outcome, cold_documents, cold_pages) = collect(&input, None);
    let cold = present(cold_outcome);
    assert!(matches!(
        cold.disposition,
        CustomHistorySourceBackedDisposition::Cold
    ));
    assert_eq!(cold_documents.len(), 70);
    assert_eq!(
        cold.certificate.counts().certified_bytes,
        fs::metadata(&path).unwrap().len()
    );
    assert!(cold_pages.len() >= 2);
    assert!(cold_pages.iter().all(|documents| *documents <= 64));
    assert_eq!(cold_documents[0].body, long);
    assert!(cold_documents[0].body.ends_with("custom-tail-sentinel"));
    assert_eq!(cold_documents[0].agent_type, "subagent");
    assert!(!cold_documents[0].is_primary);
    assert_eq!(
        cold_documents[0].source_path.as_deref(),
        Some("/provider/demo/session.jsonl")
    );
    assert_eq!(
        cold_documents[0].parent_session_id,
        Some(cold_documents[0].root_session_id)
    );
    assert_ne!(
        cold_documents[0].root_session_id,
        cold_documents[0].session_id
    );
    let cold_ids = cold_documents
        .iter()
        .map(|document| (document.session_id, document.event_id))
        .collect::<Vec<_>>();
    let TypedKey::Bytes(checkpoint) = cold.certificate.frontier().unwrap().checkpoint() else {
        panic!("custom checkpoint must be typed bytes");
    };
    assert!(!String::from_utf8_lossy(checkpoint).contains("bounded-preview-sentinel"));

    let (rebuilt_outcome, rebuilt_documents, _) = collect(&input, None);
    present(rebuilt_outcome);
    assert_eq!(
        rebuilt_documents
            .iter()
            .map(|document| (document.session_id, document.event_id))
            .collect::<Vec<_>>(),
        cold_ids
    );
    assert_eq!(rebuilt_documents[0].source, cold_documents[0].source);

    #[cfg(unix)]
    let _forbid_open = crate::provider_sources::forbid_ordinary_file_content_open(&path);
    let (noop_outcome, noop_documents, noop_pages) = collect(&input, Some(&cold.certificate));
    let noop = present(noop_outcome);
    assert!(matches!(
        noop.disposition,
        CustomHistorySourceBackedDisposition::Unchanged
    ));
    assert!(noop_documents.is_empty());
    assert!(noop_pages.is_empty());

    #[cfg(unix)]
    drop(_forbid_open);
    append_record(&path, &event(70, "event-70", "child", "appended event"));
    append_record(&path, &touch(0, 70, "src/appended.rs"));
    let (append_outcome, append_documents, _) = collect(&input, Some(&noop.certificate));
    let append = present(append_outcome);
    assert!(matches!(
        append.disposition,
        CustomHistorySourceBackedDisposition::Append
    ));
    assert_eq!(append_documents.len(), 1);
    assert_eq!(append_documents[0].body, "appended event");
    assert_eq!(append_documents[0].touched_files, vec!["src/appended.rs"]);
    assert!(revalidate_custom_history_source_backed(&input, &append.certificate).unwrap());
}

#[test]
fn rewrite_and_truncate_are_replacements_but_keep_native_ids_stable() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("rewrite.jsonl");
    let base = vec![
        manifest(),
        source(),
        session("root", None, true),
        event(0, "stable-event", "root", "original body"),
    ];
    write_records(&path, &base);
    let input = CustomHistorySourceBackedInput::explicit(&path, [8; 32]);
    let (cold_outcome, cold_documents, _) = collect(&input, None);
    let cold = present(cold_outcome);

    let rewritten = vec![
        manifest(),
        source(),
        session("root", None, true),
        event(0, "stable-event", "root", "rewritten body"),
    ];
    write_records(&path, &rewritten);
    let (rewrite_outcome, rewrite_documents, _) = collect(&input, Some(&cold.certificate));
    let rewrite = present(rewrite_outcome);
    assert!(matches!(
        rewrite.disposition,
        CustomHistorySourceBackedDisposition::Replacement
    ));
    assert_eq!(rewrite_documents[0].event_id, cold_documents[0].event_id);
    assert_eq!(
        rewrite_documents[0].session_id,
        cold_documents[0].session_id
    );
    assert_eq!(rewrite_documents[0].body, "rewritten body");

    write_records(&path, &[manifest(), source(), session("root", None, true)]);
    let (truncate_outcome, truncate_documents, _) = collect(&input, Some(&rewrite.certificate));
    let truncate = present(truncate_outcome);
    assert!(matches!(
        truncate.disposition,
        CustomHistorySourceBackedDisposition::Replacement
    ));
    assert!(truncate_documents.is_empty());
    assert_eq!(truncate.certificate.counts().indexed_documents, 0);
}

#[test]
fn malformed_complete_record_is_rejected_and_incomplete_tail_waits_for_append() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("tail.jsonl");
    let complete_records = vec![
        manifest(),
        source(),
        session("root", None, true),
        event(0, "complete", "root", "complete event"),
    ];
    let mut bytes = write_records(&path, &complete_records)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    bytes.extend_from_slice(b"{malformed-json}\n");
    let tail = serde_json::to_vec(&event(1, "tail", "root", "completed after append")).unwrap();
    bytes.extend_from_slice(&tail[..tail.len() - 1]);
    fs::write(&path, &bytes).unwrap();
    let input = CustomHistorySourceBackedInput::explicit(&path, [9; 32]);

    let (cold_outcome, cold_documents, _) = collect(&input, None);
    let cold = present(cold_outcome);
    assert_eq!(cold_documents.len(), 1);
    assert_eq!(cold.certificate.counts().rejected_records, 1);
    assert!(cold.certificate.counts().certified_bytes < fs::metadata(&path).unwrap().len());

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"}\n").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let (append_outcome, append_documents, _) = collect(&input, Some(&cold.certificate));
    let append = present(append_outcome);
    assert_eq!(
        append.certificate.counts().certified_bytes,
        fs::metadata(&path).unwrap().len()
    );
    assert!(matches!(
        append.disposition,
        CustomHistorySourceBackedDisposition::Append
    ));
    assert_eq!(append_documents.len(), 1);
    assert_eq!(append_documents[0].body, "completed after append");
    assert_eq!(append.certificate.counts().rejected_records, 1);
}

#[test]
fn exact_resolver_hydrates_grouped_records_and_rejects_stale_locator() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("hydrate.jsonl");
    let records = vec![
        manifest(),
        source(),
        session("root", None, true),
        event(0, "event-a", "root", "alpha exact"),
        event(1, "event-b", "root", "beta exact"),
    ];
    write_records(&path, &records);
    let input = CustomHistorySourceBackedInput::explicit(&path, [10; 32]);
    let (outcome, documents, _) = collect(&input, None);
    let receipt = present(outcome);
    let resolver = CustomHistorySourceBackedResolver::new([receipt.route.clone()]).unwrap();
    let requests = documents
        .iter()
        .map(|document| {
            EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
        })
        .collect::<Vec<_>>();

    let first = resolver.hydrate_event(&requests[0]).unwrap();
    assert_eq!(first.provider_bytes, b"alpha exact");
    let session_request =
        SessionHydrationRequest::new(documents[0].session_id, requests.clone()).unwrap();
    let hydrated = resolver.hydrate_session(&session_request).unwrap();
    assert_eq!(
        hydrated
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![b"alpha exact".as_slice(), b"beta exact".as_slice()]
    );
    assert!(documents.iter().all(|document| matches!(
        document.locator.coordinate(),
        NativeRecordCoordinate::Jsonl {
            native_session_key: Some(TypedKey::Composite(_)),
            native_event_key: Some(_),
            ..
        }
    )));

    let rewritten = vec![
        manifest(),
        source(),
        session("root", None, true),
        event(0, "event-a", "root", "omega stale"),
        event(1, "event-b", "root", "beta exact"),
    ];
    write_records(&path, &rewritten);
    let stale = resolver.hydrate_event(&requests[0]).unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);
}

#[test]
fn lexical_body_prefers_full_payload_over_native_preview_and_hydrates_identically() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preview.jsonl");
    let full = format!("custom-full-{}-custom-preview-tail", "p".repeat(8_192));
    let mut record = event(0, "event-full", "root", &full);
    record["preview"] = Value::String("native preview only".to_owned());
    write_records(
        &path,
        &[manifest(), source(), session("root", None, true), record],
    );
    let input = CustomHistorySourceBackedInput::explicit(&path, [13; 32]);
    let (outcome, documents, _) = collect(&input, None);
    let receipt = present(outcome);
    assert_eq!(documents[0].body, full);
    assert!(documents[0].body.ends_with("custom-preview-tail"));

    let resolver = CustomHistorySourceBackedResolver::new([receipt.route]).unwrap();
    let request =
        EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone()).unwrap();
    assert_eq!(
        resolver.hydrate_event(&request).unwrap().provider_bytes,
        documents[0].body.as_bytes()
    );
}

#[test]
fn source_backed_custom_adapter_has_no_preview_or_store_body_fallback() {
    let source = [
        include_str!("source_backed.rs"),
        include_str!("source_backed/parser.rs"),
        include_str!("source_backed/resolver.rs"),
    ]
    .concat();
    assert!(!source.contains("MAX_BODY_PREVIEW_CHARS"));
    assert!(!source.contains("ctx_history_store"));
}

#[test]
fn explicit_inventory_ignores_siblings_and_certifies_deletion() {
    let temp = tempdir().unwrap();
    let selected = temp.path().join("selected.jsonl");
    let sibling = temp.path().join("sibling.jsonl");
    write_records(
        &selected,
        &[
            manifest(),
            source(),
            session("root", None, true),
            event(0, "selected", "root", "selected-only"),
        ],
    );
    write_records(
        &sibling,
        &[
            manifest(),
            source(),
            session("root", None, true),
            event(0, "sibling", "root", "must-not-be-discovered"),
        ],
    );
    let input = CustomHistorySourceBackedInput::explicit(&selected, [11; 32]);
    let (outcome, documents, _) = collect(&input, None);
    let receipt = present(outcome);
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].body, "selected-only");

    fs::remove_file(&selected).unwrap();
    let (missing, emitted, pages) = collect(&input, Some(&receipt.certificate));
    assert!(emitted.is_empty());
    assert!(pages.is_empty());
    let CustomHistorySourceBackedOutcome::Missing {
        inventory,
        deletion: Some(deletion),
    } = missing
    else {
        panic!("explicit deletion must carry finite inventory evidence");
    };
    assert!(deletion.verifies(&inventory));
    assert_eq!(inventory.observed_sources(), 0);

    let directory_input =
        CustomHistorySourceBackedInput::explicit(temp.path().to_path_buf(), [12; 32]);
    assert!(observe_custom_history_source_backed_explicit(&directory_input).is_err());
}

#[test]
fn test_fixture_records_stay_within_the_production_line_bound() {
    let encoded = serde_json::to_vec(&event(0, "bounded", "root", "fixture")).unwrap();
    assert!(encoded.len() < MAX_PROVIDER_JSONL_LINE_BYTES);
}

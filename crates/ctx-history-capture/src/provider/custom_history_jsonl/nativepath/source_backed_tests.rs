use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{CaptureProvider, CoreRecord, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

use super::source_backed::*;
use crate::{
    provider::source_backed::{
        assert_carried_route_failure, refresh_source_backed_generation,
        register_custom_history_source_backed_route, SourceBackedProviderRegistry,
        SourceBackedSourceFailureClass,
    },
    test_support_paths::tempdir,
    CaptureError, ProviderCatalogSupport, ProviderImportSupport, ProviderSource,
    ProviderSourceFailureKind, ProviderSourceKind, ProviderSourceStatus,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

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

fn edge(edge_id: &str) -> Value {
    json!({
        "record_type": "edge",
        "source_id": "source-a",
        "from_session_id": "root",
        "to_session_id": "child",
        "edge_id": edge_id,
        "edge_type": "parent_child",
    })
}

fn explicit_provider_source(path: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Custom,
        path: path.to_path_buf(),
        exists: true,
        source_format: "ctx_history_jsonl_v1",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
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
    Vec<CoreRecord>,
    Vec<usize>,
) {
    let mut documents = Vec::new();
    let mut page_bounds = Vec::new();
    let outcome = scan_custom_history_source_backed_explicit(input, prior, |_, page| {
        page_bounds.push(page.records.len());
        documents.extend(page.records);
        Ok(())
    })
    .unwrap();
    (outcome, documents, page_bounds)
}

fn body(record: &CoreRecord) -> &str {
    record.content.normalized_body.as_deref().unwrap()
}

fn present(outcome: CustomHistorySourceBackedOutcome) -> CustomHistorySourceBackedReceipt {
    match outcome {
        CustomHistorySourceBackedOutcome::Present(receipt) => *receipt,
        CustomHistorySourceBackedOutcome::Missing { .. } => panic!("expected present source"),
    }
}

fn structural_manifest_failure(
    path: &Path,
    lineage: [u8; 32],
) -> (ProviderSourceFailureKind, String) {
    let input = CustomHistorySourceBackedInput::explicit(path, lineage);
    let error = scan_custom_history_source_backed_explicit(&input, None, |_, _| Ok(()))
        .expect_err("structurally invalid manifest must fail its source");
    match error {
        CustomHistorySourceBackedError::Capture(CaptureError::ProviderSource {
            provider,
            path: failed_path,
            kind,
            detail,
        }) => {
            assert_eq!(provider, CaptureProvider::Custom.as_str());
            assert_eq!(failed_path, path);
            (kind, detail)
        }
        error => panic!("expected a typed provider source failure, got {error:?}"),
    }
}

#[test]
fn missing_manifest_is_a_malformed_source_failure() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("missing-manifest.jsonl");
    fs::write(&path, []).unwrap();

    let (kind, detail) = structural_manifest_failure(&path, [20; 32]);
    assert_eq!(kind, ProviderSourceFailureKind::InvalidSource);
    assert!(detail.contains("missing manifest record"), "{detail}");
}

#[test]
fn unsupported_manifest_is_an_unsupported_schema_failure() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("unsupported-manifest.jsonl");
    write_records(
        &path,
        &[json!({
            "record_type": "manifest",
            "schema_version": "ctx-history-jsonl-v999",
        })],
    );

    let (kind, detail) = structural_manifest_failure(&path, [21; 32]);
    assert_eq!(kind, ProviderSourceFailureKind::SchemaIncompatible);
    assert!(detail.contains("ctx-history-jsonl-v999"), "{detail}");
}

#[test]
fn duplicate_manifest_is_a_malformed_source_failure() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("duplicate-manifest.jsonl");
    write_records(&path, &[manifest(), manifest()]);

    let (kind, detail) = structural_manifest_failure(&path, [22; 32]);
    assert_eq!(kind, ProviderSourceFailureKind::InvalidSource);
    assert!(
        detail.contains("duplicate manifest record at line 2"),
        "{detail}"
    );
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
    assert_eq!(body(&cold_documents[0]), long);
    assert!(body(&cold_documents[0]).ends_with("custom-tail-sentinel"));
    assert_eq!(cold_documents[0].agent_type, "subagent");
    assert!(!cold_documents[0].is_primary);
    assert!(!serde_json::to_string(&cold_documents[0])
        .unwrap()
        .contains("/provider/demo/session.jsonl"));
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
    assert_eq!(body(&append_documents[0]), "appended event");
    assert!(append_documents[0].repository_file_observations.is_empty());
    assert!(revalidate_custom_history_source_backed(&input, &append.certificate).unwrap());
}

#[test]
fn append_that_closes_an_old_forward_reference_is_a_replacement() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("forward-reference.jsonl");
    write_records(
        &path,
        &[
            manifest(),
            source(),
            event(0, "forward-event", "late-session", "now retained"),
        ],
    );
    let input = CustomHistorySourceBackedInput::explicit(&path, [17; 32]);

    let (cold_outcome, cold_documents, _) = collect(&input, None);
    let cold = present(cold_outcome);
    assert!(cold_documents.is_empty());
    assert_eq!(cold.certificate.counts().indexed_documents, 0);

    append_record(&path, &session("late-session", None, true));
    reset_custom_history_source_backed_work();
    let (closure_outcome, closure_documents, _) = collect(&input, Some(&cold.certificate));
    let closure = present(closure_outcome);
    assert!(matches!(
        closure.disposition,
        CustomHistorySourceBackedDisposition::Replacement
    ));
    assert_eq!(closure_documents.len(), 1);
    assert_eq!(body(&closure_documents[0]), "now retained");
    assert_eq!(
        custom_history_source_backed_work().retained_events_before_prior_prefix,
        1
    );

    append_record(
        &path,
        &event(1, "ordinary-append", "late-session", "ordinary append"),
    );
    let (append_outcome, append_documents, _) = collect(&input, Some(&closure.certificate));
    let append = present(append_outcome);
    assert!(matches!(
        append.disposition,
        CustomHistorySourceBackedDisposition::Append
    ));
    assert_eq!(append_documents.len(), 1);
    assert_eq!(body(&append_documents[0]), "ordinary append");
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
    assert_eq!(body(&rewrite_documents[0]), "rewritten body");

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
    assert_eq!(body(&append_documents[0]), "completed after append");
    assert_eq!(append.certificate.counts().rejected_records, 1);
}

#[test]
fn projected_records_are_complete_and_locator_free() {
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
    let (outcome, records, _) = collect(&input, None);
    present(outcome);
    assert_eq!(
        records.iter().map(body).collect::<Vec<_>>(),
        vec!["alpha exact", "beta exact"]
    );
    assert!(records
        .iter()
        .all(|record| record.native_event_id.is_some()));
    let Some(TypedKey::Composite(identity)) = records[0].native_event_id.as_ref() else {
        panic!("custom Core event identity must retain source selector parts");
    };
    assert_eq!(
        &identity[..2],
        &[
            TypedKey::utf8("demo-agent").unwrap(),
            TypedKey::utf8("source-a").unwrap(),
        ]
    );
    assert_eq!(identity[2], TypedKey::utf8("event_id:event-a").unwrap());
    let encoded = serde_json::to_string(&records).unwrap();
    assert!(!encoded.contains("source_path"));
    assert!(!encoded.contains("locator"));

    let rewritten = vec![
        manifest(),
        source(),
        session("root", None, true),
        event(0, "event-a", "root", "omega stale"),
        event(1, "event-b", "root", "beta exact"),
    ];
    write_records(&path, &rewritten);
    assert_eq!(body(&records[0]), "alpha exact");
}

#[test]
fn registered_route_preserves_lifecycle_and_reads_only_core_after_publication() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("registered.jsonl");
    let records = vec![
        manifest(),
        source(),
        session("root", None, true),
        event(0, "event-a", "root", "alpha exact"),
        event(1, "event-b", "root", "beta exact"),
    ];
    write_records(&path, &records);
    let input = CustomHistorySourceBackedInput::explicit(&path, [14; 32]);
    let (_, records, _) = collect(&input, None);
    let event_ids = records
        .iter()
        .map(|record| record.event_id)
        .collect::<Vec<_>>();

    let mut registry = SourceBackedProviderRegistry::new();
    register_custom_history_source_backed_route(
        &mut registry,
        explicit_provider_source(&path),
        [14; 32],
    )
    .unwrap();
    let index_root = temp.path().join("index");

    reset_custom_history_source_backed_work();
    let cold =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 2);
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(custom_history_source_backed_work().projection_parses, 1);
    let cold_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(
        body(
            &cold_index
                .core_record_by_id(event_ids[0].as_uuid())
                .unwrap()
                .unwrap()
        ),
        "alpha exact"
    );

    reset_custom_history_source_backed_work();
    let exact =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(exact.commit.indexed_documents, 2);
    assert_eq!(exact.sources, cold.sources);
    assert_eq!(custom_history_source_backed_work().projection_parses, 0);

    append_record(&path, &event(2, "event-c", "root", "gamma family append"));
    let (_, appended_records, _) = collect(&input, None);
    let appended_event_id = appended_records
        .iter()
        .find(|record| body(record) == "gamma family append")
        .unwrap()
        .event_id;
    reset_custom_history_source_backed_work();
    let appended =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(appended.commit.indexed_documents, 3);
    assert_eq!(custom_history_source_backed_work().projection_parses, 1);
    assert_eq!(custom_history_source_backed_work().source_read_passes, 1);
    let appended_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(
        body(
            &appended_index
                .core_record_by_id(event_ids[0].as_uuid())
                .unwrap()
                .unwrap()
        ),
        "alpha exact"
    );
    assert_eq!(
        body(
            &appended_index
                .core_record_by_id(appended_event_id.as_uuid())
                .unwrap()
                .unwrap()
        ),
        "gamma family append"
    );
    drop(appended_index);

    write_records(
        &path,
        &[
            manifest(),
            source(),
            session("root", None, true),
            event(0, "event-a", "root", "alpha replacement"),
            event(1, "event-b", "root", "beta exact"),
        ],
    );
    reset_custom_history_source_backed_work();
    let replacement =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(replacement.commit.indexed_documents, 2);
    assert_ne!(replacement.sources, appended.sources);
    assert_eq!(custom_history_source_backed_work().projection_parses, 1);
    assert_eq!(
        body(
            &VerifiedIndex::open_pinned(&index_root)
                .unwrap()
                .core_record_by_id(event_ids[0].as_uuid())
                .unwrap()
                .unwrap()
        ),
        "alpha replacement"
    );

    fs::remove_file(&path).unwrap();
    reset_custom_history_source_backed_work();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert!(deleted.sources.is_empty());
    assert_eq!(deleted.removals.len(), 1);
    assert_eq!(custom_history_source_backed_work().projection_parses, 0);
    assert!(VerifiedIndex::open_pinned(&index_root)
        .unwrap()
        .core_record_by_id(event_ids[0].as_uuid())
        .unwrap()
        .is_none());
}

#[test]
fn registered_route_replaces_when_append_closes_a_forward_reference() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("registered-forward-reference.jsonl");
    write_records(
        &path,
        &[
            manifest(),
            source(),
            event(0, "forward-event", "late-session", "family now retained"),
        ],
    );
    let input = CustomHistorySourceBackedInput::explicit(&path, [24; 32]);
    let mut registry = SourceBackedProviderRegistry::new();
    register_custom_history_source_backed_route(
        &mut registry,
        explicit_provider_source(&path),
        [24; 32],
    )
    .unwrap();
    let index_root = temp.path().join("forward-index");

    let cold =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 0);
    assert_eq!(cold.sources.len(), 1);

    reset_custom_history_source_backed_work();
    let exact =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(exact.sources, cold.sources);
    assert_eq!(custom_history_source_backed_work().projection_parses, 0);

    append_record(&path, &session("late-session", None, true));
    let (_, closure_records, _) = collect(&input, None);
    let forward_event_id = closure_records[0].event_id;
    reset_custom_history_source_backed_work();
    let closure =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(closure.commit.indexed_documents, 1);
    let closure_work = custom_history_source_backed_work();
    assert_eq!(closure_work.projection_parses, 1);
    assert_eq!(closure_work.source_read_passes, 1);
    assert_eq!(closure_work.retained_events_before_prior_prefix, 1);
    assert_eq!(
        body(
            &VerifiedIndex::open_pinned(&index_root)
                .unwrap()
                .core_record_by_id(forward_event_id.as_uuid())
                .unwrap()
                .unwrap()
        ),
        "family now retained"
    );

    append_record(
        &path,
        &event(
            1,
            "ordinary-after-closure",
            "late-session",
            "family ordinary append",
        ),
    );
    let (_, appended_records, _) = collect(&input, None);
    let appended_event_id = appended_records
        .iter()
        .find(|record| body(record) == "family ordinary append")
        .unwrap()
        .event_id;
    reset_custom_history_source_backed_work();
    let appended =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(appended.commit.indexed_documents, 2);
    assert_eq!(custom_history_source_backed_work().projection_parses, 1);
    assert_eq!(custom_history_source_backed_work().source_read_passes, 1);
    assert_eq!(
        body(
            &VerifiedIndex::open_pinned(&index_root)
                .unwrap()
                .core_record_by_id(appended_event_id.as_uuid())
                .unwrap()
                .unwrap()
        ),
        "family ordinary append"
    );
}

#[test]
fn structural_manifest_failures_retain_the_published_generation_and_restore() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("retained-across-invalidity.jsonl");
    let valid_lines = write_records(
        &path,
        &[
            manifest(),
            source(),
            session("root", None, true),
            event(0, "retained-event", "root", "retained across invalidity"),
        ],
    );
    let valid_bytes = valid_lines.concat();
    let mut registry = SourceBackedProviderRegistry::new();
    register_custom_history_source_backed_route(
        &mut registry,
        explicit_provider_source(&path),
        [23; 32],
    )
    .unwrap();
    let index_root = temp.path().join("retained-index");

    let initial =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    let initial_generation = initial.commit.generation_id.clone();
    assert_eq!(initial.commit.indexed_documents, 1);

    let mut incompatible_manifest = manifest();
    incompatible_manifest["schema_version"] = json!("ctx-history-jsonl-v2");
    write_records(
        &path,
        &[
            incompatible_manifest,
            source(),
            session("root", None, true),
            event(0, "incompatible-event", "root", "must not publish"),
        ],
    );
    let incompatible =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_carried_route_failure(
        &incompatible,
        &initial_generation,
        SourceBackedSourceFailureClass::Incompatible,
    );

    fs::write(&path, []).unwrap();
    let failed =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_carried_route_failure(
        &failed,
        &initial_generation,
        SourceBackedSourceFailureClass::Unreadable,
    );
    let retained = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(retained.generation_id(), initial_generation);
    assert_eq!(retained.document_count(), 1);
    assert_eq!(retained.manifest().sources, initial.sources);
    drop(retained);

    fs::write(&path, valid_bytes).unwrap();
    let restored =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(restored.commit.indexed_documents, 1);
    assert_eq!(restored.sources.len(), 1);
    assert_ne!(restored.commit.generation_id, initial_generation);
    let recovered = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(recovered.generation_id(), restored.commit.generation_id);
    assert_eq!(recovered.document_count(), 1);
}

#[test]
fn deep_chain_session_catalog_and_event_roots_are_linear() {
    const SESSIONS: usize = 1_000;
    const EVENTS: usize = 1_000;

    let temp = tempdir().unwrap();
    let path = temp.path().join("deep-chain.jsonl");
    let mut records = Vec::with_capacity(2 + SESSIONS + EVENTS);
    records.push(manifest());
    records.push(source());
    for index in 0..SESSIONS {
        let session_id = format!("session-{index:04}");
        let parent = (index != 0).then(|| format!("session-{:04}", index - 1));
        records.push(session(&session_id, parent.as_deref(), index == 0));
    }
    for index in 0..EVENTS {
        records.push(event(
            u64::try_from(index).unwrap(),
            &format!("event-{index:04}"),
            "session-0999",
            "deep event",
        ));
    }
    write_records(&path, &records);
    let input = CustomHistorySourceBackedInput::explicit(&path, [15; 32]);

    reset_custom_history_source_backed_work();
    let (_, documents, _) = collect(&input, None);
    assert_eq!(documents.len(), EVENTS);
    assert!(documents
        .iter()
        .all(|document| document.root_session_id == documents[0].root_session_id));
    let work = custom_history_source_backed_work();
    assert_eq!(work.projection_parses, 1);
    assert_eq!(work.source_read_passes, 1);
    assert_eq!(work.provider_records_parsed, 2 + SESSIONS + EVENTS);
    assert_eq!(work.session_nodes, SESSIONS);
    assert_eq!(work.session_dependencies, SESSIONS - 1);
    assert_eq!(work.session_root_nodes, SESSIONS);
    assert_eq!(work.event_root_lookups, EVENTS);
    assert_eq!(work.resident_event_body_bytes, 0);
}

#[test]
fn event_bodies_live_in_the_spool_or_one_bounded_emission_page() {
    const EVENTS: usize = 512;
    const BODY_BYTES: usize = 8 * 1024;

    let temp = tempdir().unwrap();
    let path = temp.path().join("bounded-spool.jsonl");
    let expected_body = "b".repeat(BODY_BYTES);
    let mut records = Vec::with_capacity(3 + EVENTS);
    records.extend([manifest(), source(), session("root", None, true)]);
    for index in 0..EVENTS {
        records.push(event(
            u64::try_from(index).unwrap(),
            &format!("event-{index:03}"),
            "root",
            &expected_body,
        ));
    }
    write_records(&path, &records);
    let input = CustomHistorySourceBackedInput::explicit(&path, [16; 32]);

    reset_custom_history_source_backed_work();
    let (_, documents, pages) = collect(&input, None);
    let work = custom_history_source_backed_work();

    assert_eq!(documents.len(), EVENTS);
    assert!(documents
        .iter()
        .all(|document| body(document) == expected_body));
    assert!(pages.len() > 1);
    assert_eq!(work.projection_parses, 1);
    assert_eq!(work.source_read_passes, 1);
    assert_eq!(work.provider_records_parsed, 3 + EVENTS);
    assert_eq!(work.catalog_records, 3 + EVENTS);
    assert!(work.catalog_metadata_bytes <= CUSTOM_HISTORY_CATALOG_MAX_METADATA_BYTES);
    assert_eq!(
        work.spooled_event_body_bytes,
        EVENTS.saturating_mul(BODY_BYTES)
    );
    assert_eq!(work.resident_event_body_bytes, 0);
    assert!(
        work.peak_resident_event_body_bytes
            <= CUSTOM_PAGE_MAX_RETAINED_BYTES.saturating_add(BODY_BYTES)
    );
    assert!(work.peak_resident_event_body_bytes < work.spooled_event_body_bytes);
    assert!(work.peak_provider_record_bytes <= MAX_PROVIDER_JSONL_LINE_BYTES);
}

#[test]
fn catalog_record_and_metadata_bounds_accept_n_and_reject_n_plus_one() {
    const RECORDS: usize = 8;

    let temp = tempdir().unwrap();
    let path = temp.path().join("catalog-bounds.jsonl");
    let mut records = vec![manifest(), source(), session("root", None, true)];
    for index in 0..RECORDS - records.len() {
        records.push(event(
            u64::try_from(index).unwrap(),
            &format!("event-{index}"),
            "root",
            "bounded",
        ));
    }
    write_records(&path, &records);
    let input = CustomHistorySourceBackedInput::explicit(&path, [18; 32]);

    reset_custom_history_source_backed_work();
    let counts = validate_custom_history_catalog_bounds(
        &input,
        RECORDS,
        CUSTOM_HISTORY_CATALOG_MAX_METADATA_BYTES,
    )
    .unwrap();
    assert_eq!(counts.complete_records, RECORDS as u64);
    let exact_work = custom_history_source_backed_work();
    assert_eq!(exact_work.source_read_passes, 1);
    assert_eq!(exact_work.catalog_records, RECORDS);
    assert!(exact_work.catalog_metadata_bytes > 0);
    assert_eq!(exact_work.resident_event_body_bytes, 0);

    reset_custom_history_source_backed_work();
    validate_custom_history_catalog_bounds(&input, RECORDS, exact_work.catalog_metadata_bytes)
        .unwrap();
    assert_eq!(
        custom_history_source_backed_work().catalog_metadata_bytes,
        exact_work.catalog_metadata_bytes
    );

    reset_custom_history_source_backed_work();
    let metadata_error = validate_custom_history_catalog_bounds(
        &input,
        RECORDS,
        exact_work.catalog_metadata_bytes - 1,
    )
    .unwrap_err();
    assert!(matches!(
        metadata_error,
        CustomHistorySourceBackedError::Bounds {
            limit: CustomHistorySourceBackedBound::CatalogMetadataBytes,
            maximum,
            observed,
        } if maximum == exact_work.catalog_metadata_bytes - 1
            && observed == exact_work.catalog_metadata_bytes
    ));
    assert!(
        custom_history_source_backed_work().catalog_metadata_bytes
            < exact_work.catalog_metadata_bytes
    );

    records.push(event(99, "event-over-bound", "root", "not admitted"));
    write_records(&path, &records);
    reset_custom_history_source_backed_work();
    let record_error = validate_custom_history_catalog_bounds(
        &input,
        RECORDS,
        CUSTOM_HISTORY_CATALOG_MAX_METADATA_BYTES,
    )
    .unwrap_err();
    assert!(matches!(
        record_error,
        CustomHistorySourceBackedError::Bounds {
            limit: CustomHistorySourceBackedBound::CatalogRecords,
            maximum: RECORDS,
            observed,
        } if observed == RECORDS + 1
    ));
    let rejected_work = custom_history_source_backed_work();
    assert_eq!(rejected_work.source_read_passes, 1);
    assert_eq!(rejected_work.catalog_records, RECORDS);
    assert_eq!(rejected_work.provider_records_parsed, RECORDS);
    assert_eq!(rejected_work.resident_event_body_bytes, 0);
}

#[test]
fn oversized_edge_id_fails_typed_bounds_before_catalog_retention() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("edge-bound.jsonl");
    write_records(
        &path,
        &[
            manifest(),
            source(),
            session("root", None, true),
            session("child", Some("root"), false),
            edge(&"e".repeat(
                crate::provider::custom_history_jsonl::CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES + 1,
            )),
        ],
    );
    let input = CustomHistorySourceBackedInput::explicit(&path, [19; 32]);

    reset_custom_history_source_backed_work();
    let error = validate_custom_history_catalog_bounds(
        &input,
        CUSTOM_HISTORY_CATALOG_MAX_RECORDS,
        CUSTOM_HISTORY_CATALOG_MAX_METADATA_BYTES,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CustomHistorySourceBackedError::Bounds {
            limit: CustomHistorySourceBackedBound::EdgeIdBytes,
            maximum,
            observed,
        } if maximum
            == crate::provider::custom_history_jsonl::CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES
            && observed == maximum + 1
    ));
    let work = custom_history_source_backed_work();
    assert_eq!(work.source_read_passes, 1);
    assert_eq!(work.catalog_records, 5);
    assert_eq!(work.provider_records_parsed, 5);
    assert_eq!(work.resident_event_body_bytes, 0);
}

#[test]
fn core_body_prefers_full_payload_over_native_preview() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("preview.jsonl");
    let full = format!("custom-full-{}-custom-preview-tail", "p".repeat(16_512));
    assert!(full.len() > 16_000);
    let mut record = event(0, "event-full", "root", &full);
    record["preview"] = Value::String("native preview only".to_owned());
    write_records(
        &path,
        &[manifest(), source(), session("root", None, true), record],
    );
    let input = CustomHistorySourceBackedInput::explicit(&path, [13; 32]);
    let (outcome, documents, _) = collect(&input, None);
    present(outcome);
    assert_eq!(body(&documents[0]), full);
    assert!(body(&documents[0]).ends_with("custom-preview-tail"));
}

#[test]
fn source_backed_custom_adapter_has_no_preview_or_store_body_fallback() {
    let source = [
        include_str!("source_backed.rs"),
        include_str!("source_backed/parser.rs"),
    ]
    .concat();
    assert!(!source.contains("MAX_BODY_PREVIEW_CHARS"));
    assert!(!source.contains("ctx_history_store"));
    assert!(!source.contains("SourceRecordLocator"));
    assert!(!source.contains("hydrate_"));
    assert!(source.contains("scan_optimized_leaf"));
    assert!(source.contains("base_source_path"));
    assert!(source.contains("JsonlFamilyTerminalProof::exact_file"));
    assert!(!source.contains("fn revalidate_leaf"));

    let registration = include_str!("../../source_backed/registration/families/jsonl/other.rs");
    assert!(registration.contains("custom_history_jsonl_family_adapter"));
    assert!(registration.contains("jsonl_family_driver"));
    assert!(!registration.contains("SourceBackedRouteDriver::new"));
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
    assert_eq!(body(&documents[0]), "selected-only");

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

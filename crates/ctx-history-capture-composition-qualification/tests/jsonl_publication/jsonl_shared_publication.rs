use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{CaptureProvider, CoreDiscoveryExclusion, CoreRecord, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

use crate::{
    provider::source_backed::{
        assert_carried_route_failure, family::jsonl::set_after_jsonl_semantic_preflight_hook,
        refresh_source_backed_generation, register_custom_history_source_backed_route,
        register_landed_source_backed_route, SourceBackedProviderRegistry,
        SourceBackedRouteSelection, SourceBackedSourceFailureClass,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

const OPENCLAW_SOURCE_FORMAT: &str = "openclaw_session_jsonl_tree";
const PI_SOURCE_FORMAT: &str = "pi_session_jsonl";
const CUSTOM_HISTORY_SOURCE_FORMAT: &str = "ctx_history_jsonl_v2";

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn all_indexed_records(index: &Path) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open(index).unwrap();
    let mut records = verified
        .manifest()
        .sources
        .iter()
        .flat_map(|source| {
            verified
                .core_source_event_page(source.observation().source(), None, 64)
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.core_record)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|record| (record.provider_session_id.clone(), record.event_sequence));
    records
}

fn openclaw_lifecycle_transcript(root: &Path) -> std::path::PathBuf {
    root.join("agents/main/sessions/lifecycle.jsonl")
}

fn write_openclaw_lifecycle_transcript(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_openclaw_lifecycle_transcript(path: &Path, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

fn openclaw_registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::OpenClaw,
            path: root.to_path_buf(),
            exists: true,
            source_format: OPENCLAW_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn retrieval_excluded(record: &CoreRecord) -> bool {
    record.content.discovery_exclusion == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
}

fn custom_manifest() -> Value {
    json!({
        "record_type": "manifest",
        "schema_version": "ctx-history-jsonl-v2",
        "producer": "source-backed-test",
    })
}

fn custom_source() -> Value {
    json!({
        "record_type": "source",
        "source_id": "source-a",
        "provider_key": "demo-agent",
        "source_format": "demo-jsonl",
        "raw_source_path": "/provider/demo/session.jsonl",
    })
}

fn custom_session(id: &str, parent: Option<&str>, primary: bool) -> Value {
    json!({
        "record_type": "session",
        "source_id": "source-a",
        "provider_session_id": id,
        "parent_provider_session_id": parent,
        "root_provider_session_id": parent,
        "agent_scope": if primary { "primary" } else { "subagent" },
        "started_at": "2026-07-28T12:00:00Z",
        "cwd": "/work/custom-history",
    })
}

fn custom_event(index: u64, id: &str, provider_session_id: &str, text: &str) -> Value {
    json!({
        "record_type": "event",
        "source_id": "source-a",
        "provider_session_id": provider_session_id,
        "event_index": index,
        "event_id": id,
        "event_type": "message",
        "role": if index.is_multiple_of(2) { "user" } else { "assistant" },
        "occurred_at": format!("2026-07-28T12:00:{:02}Z", index.min(59)),
        "payload": {"text": text},
    })
}

fn custom_provider_source(path: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Custom,
        path: path.to_path_buf(),
        exists: true,
        source_format: CUSTOM_HISTORY_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

fn write_custom_records(path: &Path, records: &[Value]) -> Vec<Vec<u8>> {
    let lines = records
        .iter()
        .map(|record| {
            let mut line = serde_json::to_vec(record).unwrap();
            line.push(b'\n');
            line
        })
        .collect::<Vec<_>>();
    fs::write(path, lines.concat()).unwrap();
    lines
}

fn append_custom_record(path: &Path, record: &Value) {
    let mut line = serde_json::to_vec(record).unwrap();
    line.push(b'\n');
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&line).unwrap();
    file.sync_all().unwrap();
}

fn custom_records(index: &Path) -> Vec<CoreRecord> {
    all_indexed_records(index)
}

fn write_pi_session(path: &Path, session_id: &str, parent: Option<&Path>, body: &str) {
    let header = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": session_id,
        "timestamp": "2026-01-02T03:04:05Z",
        "cwd": "/tmp/pi",
        "parentSession": parent.map(|path| path.to_string_lossy().into_owned()),
    });
    let message = serde_json::json!({
        "type": "message",
        "id": "copied-entry",
        "timestamp": "2026-01-02T03:04:06Z",
        "message": {"role": "user", "content": body},
    });
    fs::write(path, format!("{header}\n{message}\n")).unwrap();
}

fn write_omp_pi_session(path: &Path, title: &str, body: &str) {
    let mut title_slot = serde_json::to_string(&serde_json::json!({
        "type": "title",
        "v": 1,
        "title": title,
        "updatedAt": "2026-08-20T15:12:20.989Z",
        "pad": "",
        "source": "user",
    }))
    .unwrap();
    let padding = 255_usize.checked_sub(title_slot.len()).unwrap();
    title_slot.insert_str(title_slot.len() - 2, &" ".repeat(padding));
    assert_eq!(title_slot.len(), 255);

    let header = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": "omp-title-slot-session",
        "timestamp": "2026-08-20T15:12:20.990Z",
        "cwd": "/workspace/omp-title-slot",
    });
    let message = serde_json::json!({
        "type": "message",
        "id": "omp-title-slot-message",
        "timestamp": "2026-08-20T15:12:21.000Z",
        "message": {"role": "user", "content": body},
    });
    fs::write(path, format!("{title_slot}\n{header}\n{message}\n")).unwrap();
}

fn pi_registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Pi,
            path: root.to_path_buf(),
            exists: true,
            source_format: PI_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn published_session(records: &[CoreRecord], provider_session_id: &str) -> Vec<CoreRecord> {
    let mut selected = records
        .iter()
        .filter(|record| record.provider_session_id.as_deref() == Some(provider_session_id))
        .cloned()
        .collect::<Vec<_>>();
    selected.sort_by_key(|record| record.event_sequence);
    selected
}

#[test]
fn custom_history_registered_route_preserves_lifecycle_and_core_publication() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("registered.jsonl");
    write_custom_records(
        &path,
        &[
            custom_manifest(),
            custom_source(),
            custom_session("root", None, true),
            custom_event(0, "event-a", "root", "alpha exact"),
            custom_event(1, "event-b", "root", "beta exact"),
        ],
    );
    let mut registry = SourceBackedProviderRegistry::new();
    register_custom_history_source_backed_route(
        &mut registry,
        custom_provider_source(&path),
        [14; 32],
    )
    .unwrap();
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 2);
    assert_eq!(cold.sources.len(), 1);
    let cold_records = custom_records(&index_root);
    assert_eq!(cold_records.len(), 2);
    assert!(cold_records
        .iter()
        .all(|record| record.provider_session_id.as_deref() == Some("root")));
    assert_eq!(
        cold_records[0].content.normalized_body.as_deref(),
        Some("alpha exact")
    );

    let exact = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(exact.commit.indexed_documents, 2);
    assert_eq!(exact.sources, cold.sources);
    assert_eq!(custom_records(&index_root), cold_records);

    append_custom_record(
        &path,
        &custom_event(2, "event-c", "root", "gamma family append"),
    );
    let appended =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(appended.commit.indexed_documents, 3);
    let appended_records = custom_records(&index_root);
    assert_eq!(appended_records.len(), 3);
    assert_eq!(
        appended_records[2].content.normalized_body.as_deref(),
        Some("gamma family append")
    );
    assert_eq!(
        appended_records[0].content.normalized_body.as_deref(),
        Some("alpha exact")
    );

    write_custom_records(
        &path,
        &[
            custom_manifest(),
            custom_source(),
            custom_session("root", None, true),
            custom_event(0, "event-a", "root", "alpha replacement"),
            custom_event(1, "event-b", "root", "beta exact"),
        ],
    );
    let replacement =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(replacement.commit.indexed_documents, 2);
    let replacement_records = custom_records(&index_root);
    assert_eq!(replacement_records.len(), 2);
    assert_eq!(
        replacement_records[0].content.normalized_body.as_deref(),
        Some("alpha replacement")
    );

    fs::remove_file(&path).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(deleted.sources.is_empty());
    assert_eq!(deleted.removals.len(), 1);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().document_count(),
        0
    );
}

#[test]
fn custom_history_registered_route_replaces_when_forward_reference_closes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("registered-forward-reference.jsonl");
    write_custom_records(
        &path,
        &[
            custom_manifest(),
            custom_source(),
            custom_event(0, "forward-event", "late-session", "family now retained"),
        ],
    );
    let mut registry = SourceBackedProviderRegistry::new();
    register_custom_history_source_backed_route(
        &mut registry,
        custom_provider_source(&path),
        [24; 32],
    )
    .unwrap();
    let index_root = temp.path().join("forward-index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 0);
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().document_count(),
        0
    );

    append_custom_record(&path, &custom_session("late-session", None, true));
    let closure =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(closure.commit.indexed_documents, 1);
    let closure_records = custom_records(&index_root);
    assert_eq!(closure_records.len(), 1);
    assert_eq!(
        closure_records[0].content.normalized_body.as_deref(),
        Some("family now retained")
    );

    append_custom_record(
        &path,
        &custom_event(
            1,
            "ordinary-after-closure",
            "late-session",
            "family ordinary append",
        ),
    );
    let appended =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(appended.commit.indexed_documents, 2);
    let appended_records = custom_records(&index_root);
    assert_eq!(appended_records.len(), 2);
    assert_eq!(
        appended_records[1].content.normalized_body.as_deref(),
        Some("family ordinary append")
    );
}

#[test]
fn custom_history_structural_manifest_failures_retain_generation_and_restore() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("retained-across-invalidity.jsonl");
    let valid_lines = write_custom_records(
        &path,
        &[
            custom_manifest(),
            custom_source(),
            custom_session("root", None, true),
            custom_event(0, "retained-event", "root", "retained across invalidity"),
        ],
    );
    let valid_bytes = valid_lines.concat();
    let mut registry = SourceBackedProviderRegistry::new();
    register_custom_history_source_backed_route(
        &mut registry,
        custom_provider_source(&path),
        [23; 32],
    )
    .unwrap();
    let index_root = temp.path().join("retained-index");

    let initial =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let initial_generation = initial.commit.generation_id.clone();
    assert_eq!(initial.commit.indexed_documents, 1);

    let mut incompatible_manifest = custom_manifest();
    incompatible_manifest["schema_version"] = json!("ctx-history-jsonl-v1");
    write_custom_records(
        &path,
        &[
            incompatible_manifest,
            custom_source(),
            custom_session("root", None, true),
            custom_event(0, "incompatible-event", "root", "must not publish"),
        ],
    );
    let incompatible =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_carried_route_failure(
        &incompatible,
        &initial_generation,
        SourceBackedSourceFailureClass::Incompatible,
    );

    fs::write(&path, []).unwrap();
    let failed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_carried_route_failure(
        &failed,
        &initial_generation,
        SourceBackedSourceFailureClass::Unreadable,
    );
    let retained = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(retained.generation_id(), initial_generation);
    assert_eq!(retained.document_count(), 1);
    assert_eq!(retained.manifest().sources, initial.sources);

    fs::write(&path, valid_bytes).unwrap();
    let restored =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(restored.commit.indexed_documents, 1);
    assert_eq!(restored.sources.len(), 1);
    assert_ne!(restored.commit.generation_id, initial_generation);
    let recovered = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(recovered.generation_id(), restored.commit.generation_id);
    assert_eq!(recovered.document_count(), 1);
}

#[test]
fn openclaw_cold_noop_and_append_refreshes_preserve_the_source() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = openclaw_lifecycle_transcript(&root);
    let first = serde_json::json!({
        "type": "message",
        "id": "lifecycle-first",
        "timestamp": "2026-08-06T12:00:00Z",
        "message": {"role": "user", "content": "OpenClaw cold record"}
    });
    let second = serde_json::json!({
        "type": "message",
        "id": "lifecycle-second",
        "timestamp": "2026-08-06T12:00:01Z",
        "message": {"role": "assistant", "content": "OpenClaw appended record"}
    });
    write_openclaw_lifecycle_transcript(&transcript, &[first]);
    let registry = openclaw_registry(&root);
    let index = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, 1);
    let cold_records = all_indexed_records(&index);
    assert_eq!(cold_records.len(), 1);

    let noop = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(noop.sources.len(), 1);
    assert_eq!(noop.sources[0].counts().complete_records, 1);
    assert_eq!(all_indexed_records(&index), cold_records);

    append_openclaw_lifecycle_transcript(&transcript, &second);
    let appended = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(appended.sources.len(), 1);
    assert_eq!(appended.sources[0].counts().complete_records, 2);
    let appended_records = all_indexed_records(&index);
    assert_eq!(appended_records.len(), 2);
    assert!(appended_records.iter().any(|record| {
        record.content.normalized_body.as_deref() == Some("OpenClaw appended record")
    }));
}

#[test]
fn openclaw_preflight_preserves_unique_append_and_carries_cross_boundary_duplicate_failure() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = openclaw_lifecycle_transcript(&root);
    let first_call = serde_json::json!({
        "type": "message",
        "id": "call-a",
        "timestamp": "2026-08-06T12:00:00Z",
        "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "call-a", "name": "exec", "arguments": {"command": "ctx search a"}}]}
    });
    let first_result = serde_json::json!({
        "type": "message",
        "id": "result-a",
        "timestamp": "2026-08-06T12:00:01Z",
        "message": {"role": "toolResult", "toolCallId": "call-a", "content": "result a", "details": {"status": "completed", "exitCode": 0}}
    });
    let second_call = serde_json::json!({
        "type": "message",
        "id": "call-b",
        "timestamp": "2026-08-06T12:00:02Z",
        "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "call-b", "name": "exec", "arguments": {"command": "ctx search b"}}]}
    });
    let second_result = serde_json::json!({
        "type": "message",
        "id": "result-b",
        "timestamp": "2026-08-06T12:00:03Z",
        "message": {"role": "toolResult", "toolCallId": "call-b", "content": "result b", "details": {"status": "completed", "exitCode": 0}}
    });
    let duplicate_result = serde_json::json!({
        "type": "message",
        "id": "result-a-duplicate",
        "timestamp": "2026-08-06T12:00:04Z",
        "message": {"role": "toolResult", "toolCallId": "call-a", "content": "result a again", "details": {"status": "completed", "exitCode": 0}}
    });
    write_openclaw_lifecycle_transcript(&transcript, &[first_call, first_result]);
    let registry = openclaw_registry(&root);
    let index = temp.path().join("index");

    let initial = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(initial.failed_routes.is_empty());

    append_openclaw_lifecycle_transcript(&transcript, &second_call);
    append_openclaw_lifecycle_transcript(&transcript, &second_result);
    let appended = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(appended.failed_routes.is_empty());
    let appended_records = all_indexed_records(&index);
    assert_eq!(appended_records.len(), 4);
    for body in ["result a", "result b"] {
        let result = appended_records
            .iter()
            .find(|record| record.content.normalized_body.as_deref() == Some(body))
            .unwrap();
        assert!(!retrieval_excluded(result), "{body}");
    }

    append_openclaw_lifecycle_transcript(&transcript, &duplicate_result);
    let replaced = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(replaced.failed_routes.len(), 1);
    assert_eq!(
        replaced.failed_routes[0].class,
        SourceBackedSourceFailureClass::Unreadable
    );
    assert!(replaced.failed_routes[0].carried_forward);
    let replaced_records = all_indexed_records(&index);
    assert_eq!(replaced_records, appended_records);
    let unique_result = replaced_records
        .iter()
        .find(|record| record.content.normalized_body.as_deref() == Some("result b"))
        .unwrap();
    assert!(!retrieval_excluded(unique_result));
}

#[test]
fn openclaw_cross_record_duplicate_calls_fail_closed_across_append_boundary() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = openclaw_lifecycle_transcript(&root);
    let first_call = serde_json::json!({
        "type": "message",
        "id": "first-call-record",
        "timestamp": "2026-08-06T12:00:00Z",
        "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "duplicate-call", "name": "exec", "arguments": {"command": "ctx search first"}}]}
    });
    let duplicate_call = serde_json::json!({
        "type": "message",
        "id": "second-call-record",
        "timestamp": "2026-08-06T12:00:01Z",
        "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "duplicate-call", "name": "exec", "arguments": {"command": "ctx search second"}}]}
    });
    write_openclaw_lifecycle_transcript(&transcript, &[first_call]);
    let registry = openclaw_registry(&root);
    let index = temp.path().join("index");

    let initial = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(initial.failed_routes.is_empty());
    let initial_records = all_indexed_records(&index);
    assert_eq!(initial_records.len(), 1);

    append_openclaw_lifecycle_transcript(&transcript, &duplicate_call);
    let duplicate = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(duplicate.failed_routes.len(), 1);
    assert_eq!(
        duplicate.failed_routes[0].class,
        SourceBackedSourceFailureClass::Unreadable
    );
    assert!(duplicate.failed_routes[0].carried_forward);
    assert_eq!(all_indexed_records(&index), initial_records);
}

#[test]
fn openclaw_projector_preflight_rejects_same_length_interpass_rewrite() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = openclaw_lifecycle_transcript(&root);
    write_openclaw_lifecycle_transcript(
        &transcript,
        &[serde_json::json!({
            "type": "message",
            "id": "stable-record",
            "timestamp": "2026-08-06T12:00:00Z",
            "message": {"role": "user", "content": "stable baseline"}
        })],
    );
    let registry = openclaw_registry(&root);
    let index = temp.path().join("index");
    let initial = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(initial.failed_routes.is_empty());

    append_openclaw_lifecycle_transcript(
        &transcript,
        &serde_json::json!({
            "type": "message",
            "id": "racing-record",
            "timestamp": "2026-08-06T12:00:01Z",
            "message": {"role": "assistant", "content": "race-before"}
        }),
    );
    let hook_path = fs::canonicalize(&transcript).unwrap();
    set_after_jsonl_semantic_preflight_hook(hook_path.clone(), move || {
        let before = fs::read_to_string(&hook_path).unwrap();
        let after = before.replace("race-before", "race-after!");
        assert_eq!(before.len(), after.len());
        assert_ne!(before, after);
        fs::write(hook_path, after).unwrap();
    });

    let failed = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(failed.failed_routes.len(), 1);
    assert_eq!(
        failed.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert!(failed.failed_routes[0].carried_forward);
    let retained = all_indexed_records(&index);
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0].content.normalized_body.as_deref(),
        Some("stable baseline")
    );
}

#[test]
fn openclaw_unsupported_sqlite_root_failure_is_carried_and_restored() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = openclaw_lifecycle_transcript(&root);
    write_openclaw_lifecycle_transcript(
        &transcript,
        &[serde_json::json!({
            "type": "message",
            "id": "stable-record",
            "timestamp": "2026-08-06T12:00:00Z",
            "message": {"role": "user", "content": "stable baseline"}
        })],
    );
    let registry = openclaw_registry(&root);
    let index = temp.path().join("index");
    let initial = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    let initial_generation = initial.commit.generation_id.clone();
    assert_eq!(initial.commit.indexed_documents, 1);

    fs::remove_file(&transcript).unwrap();
    fs::create_dir_all(root.join("agent")).unwrap();
    fs::write(root.join("agent/openclaw-agent.sqlite"), b"sqlite sentinel").unwrap();

    let failed = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_carried_route_failure(
        &failed,
        &initial_generation,
        SourceBackedSourceFailureClass::Unreadable,
    );
    let retained = VerifiedIndex::open(&index).unwrap();
    assert_eq!(retained.generation_id(), initial_generation);
    assert_eq!(retained.document_count(), 1);
    assert_eq!(retained.manifest().sources, initial.sources);

    fs::remove_file(root.join("agent/openclaw-agent.sqlite")).unwrap();
    write_openclaw_lifecycle_transcript(
        &transcript,
        &[serde_json::json!({
            "type": "message",
            "id": "stable-record",
            "timestamp": "2026-08-06T12:00:00Z",
            "message": {"role": "user", "content": "stable baseline"}
        })],
    );
    let restored = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(restored.commit.indexed_documents, 1);
    assert_eq!(restored.sources.len(), 1);
    assert_ne!(restored.commit.generation_id, initial_generation);
    assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 1);
}

#[test]
fn pi_parent_session_path_preserves_literal_parent_identity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_temp = crate::test_support_paths::tempdir().unwrap();
    let parent_path = temp.path().join("parent.jsonl");
    let child_path = temp.path().join("child.jsonl");
    write_pi_session(&parent_path, "pi-parent", None, "parent retained message");
    let parent_path = fs::canonicalize(parent_path).unwrap();
    write_pi_session(
        &child_path,
        "pi-child",
        Some(&parent_path),
        "child retained message",
    );

    let index = index_temp.path().join("index");
    refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options()).unwrap();
    let records = all_indexed_records(&index);
    let mut parent = published_session(&records, "pi-parent");
    let mut child = published_session(&records, "pi-child");
    let parent = parent.remove(0);
    let child = child.remove(0);

    assert_eq!(child.parent_session_id, Some(parent.session_id));
    assert_eq!(child.root_session_id, Some(parent.session_id));
    assert!(super::has_literal_fact(
        &parent,
        ctx_history_core::LiteralFactKind::SessionCwd,
        "/tmp/pi"
    ));
    assert!(super::has_literal_fact(
        &child,
        ctx_history_core::LiteralFactKind::SessionCwd,
        "/tmp/pi"
    ));
    assert_eq!(
        parent.native_event_id,
        Some(TypedKey::utf8("copied-entry").unwrap())
    );
    assert_eq!(parent.native_event_id, child.native_event_id);
    assert_ne!(parent.event_id, child.event_id);
    assert_eq!(child.provider_session_id.as_deref(), Some("pi-child"));
    assert_eq!(parent.provider_session_id.as_deref(), Some("pi-parent"));
}

#[test]
fn pi_omp_title_slot_cold_repeat_and_rewrite_are_rejection_free() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_temp = crate::test_support_paths::tempdir().unwrap();
    let transcript = temp.path().join("omp-title-slot.jsonl");
    write_omp_pi_session(&transcript, "alpha", "stable OMP message");
    let registry = pi_registry(temp.path());
    let index = index_temp.path().join("index");

    let cold = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(cold.commit.indexed_documents, 1);
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, 3);
    assert_eq!(cold.sources[0].counts().retained_records, 1);
    assert_eq!(cold.sources[0].counts().rejected_records, 0);
    assert_eq!(cold.sources[0].counts().ignored_records, 2);
    let cold_generation = cold.commit.generation_id.clone();
    let cold_records = all_indexed_records(&index);

    let unchanged = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.commit.generation_id, cold_generation);
    assert_eq!(all_indexed_records(&index), cold_records);

    write_omp_pi_session(&transcript, "bravo", "stable OMP message");
    let rewritten = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(rewritten.failed_routes.is_empty());
    assert_ne!(rewritten.commit.generation_id, cold_generation);
    assert_eq!(rewritten.commit.indexed_documents, 1);
    assert_eq!(rewritten.sources[0].counts().rejected_records, 0);
    assert_eq!(rewritten.sources[0].counts().ignored_records, 2);
    assert_eq!(all_indexed_records(&index), cold_records);

    let bytes = fs::read(&transcript).unwrap();
    let title_end = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    let mut transcript_file = OpenOptions::new().append(true).open(&transcript).unwrap();
    transcript_file.write_all(&bytes[..title_end]).unwrap();
    transcript_file.sync_all().unwrap();
    let late_title = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(late_title.failed_routes.is_empty());
    assert_eq!(late_title.commit.indexed_documents, 1);
    assert_eq!(late_title.sources[0].counts().rejected_records, 1);
    assert_eq!(late_title.sources[0].counts().ignored_records, 2);
    assert_eq!(all_indexed_records(&index), cold_records);
}

#[test]
fn pi_declared_missing_parent_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_temp = crate::test_support_paths::tempdir().unwrap();
    let child_path = temp.path().join("child.jsonl");
    let missing_parent = temp.path().join("missing-parent.jsonl");
    write_pi_session(
        &child_path,
        "pi-child-with-missing-parent",
        Some(&missing_parent),
        "must not publish as a root",
    );

    let error = refresh_source_backed_generation(
        index_temp.path().join("index"),
        &pi_registry(temp.path()),
        writer_options(),
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("declares unresolved parentSession"));
}

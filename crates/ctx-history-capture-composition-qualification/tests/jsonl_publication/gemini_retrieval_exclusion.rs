use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{CaptureProvider, CoreRecord};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

use crate::{
    provider::source_backed::family::jsonl::set_after_jsonl_semantic_preflight_hook,
    refresh_source_backed_generation, register_landed_source_backed_route, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    SourceBackedProviderRegistry, SourceBackedRouteSelection, SourceBackedSourceFailureClass,
};

fn transcript_path(root: &Path) -> std::path::PathBuf {
    root.join("tmp/project/chats/neutral-session.jsonl")
}

fn header() -> Value {
    json!({
        "sessionId": "neutral-gemini-session",
        "startTime": "2026-08-16T00:00:00Z",
        "kind": "main"
    })
}

fn message(id: &str, timestamp: &str, role: &str, text: &str) -> Value {
    json!({
        "id": id,
        "timestamp": timestamp,
        "type": role,
        "content": text
    })
}

fn write_transcript(path: &Path, rows: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_transcript(path: &Path, row: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, row).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Gemini,
            path: root.to_path_buf(),
            exists: true,
            source_format: ctx_history_provider_gemini::GEMINI_CLI_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    assert_eq!(registry.routes().len(), 1);
    registry
}

fn indexed_records(index: &Path) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open(index).unwrap();
    let source = verified
        .manifest()
        .sources
        .iter()
        .find(|source| source.observation().source().provider() == CaptureProvider::Gemini.as_str())
        .unwrap()
        .observation()
        .source()
        .clone();
    let mut records = verified
        .core_source_event_page(&source, None, 64)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.core_record)
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.event_sequence);
    records
}

fn certified_prefix_bytes(index: &Path) -> u64 {
    let verified = VerifiedIndex::open(index).unwrap();
    verified
        .manifest()
        .sources
        .iter()
        .find(|source| source.observation().source().provider() == CaptureProvider::Gemini.as_str())
        .unwrap()
        .frontier()
        .expect("Gemini publication must persist a checkpoint frontier")
        .certified_prefix_bytes()
}

fn assert_literal_bodies(records: &[CoreRecord], expected: &[&str]) {
    assert_eq!(
        records
            .iter()
            .map(|record| record.content.normalized_body.as_deref().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn gemini_route_publishes_cold_append_and_recovers_from_carried_checkpoint() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let transcript = transcript_path(&root);
    let index = temp.path().join("gemini-index");
    write_transcript(
        &transcript,
        &[
            header(),
            message(
                "literal-first",
                "2026-08-16T00:00:01Z",
                "user",
                "literal first",
            ),
        ],
    );
    let registry = registry(&root);
    let options = || WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };

    let cold = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(cold.successful_route_ids.len(), 1);
    let cold_records = indexed_records(&index);
    assert_literal_bodies(&cold_records, &["literal first"]);
    let cold_checkpoint = certified_prefix_bytes(&index);
    assert_eq!(cold_checkpoint, fs::metadata(&transcript).unwrap().len());

    append_transcript(
        &transcript,
        &message(
            "literal-second",
            "2026-08-16T00:00:02Z",
            "gemini",
            "literal second",
        ),
    );
    let appended = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(appended.failed_routes.is_empty());
    let appended_records = indexed_records(&index);
    assert_literal_bodies(&appended_records, &["literal first", "literal second"]);
    assert_eq!(appended_records[0].event_id, cold_records[0].event_id);
    let appended_checkpoint = certified_prefix_bytes(&index);
    assert!(appended_checkpoint > cold_checkpoint);
    assert_eq!(
        appended_checkpoint,
        fs::metadata(&transcript).unwrap().len()
    );

    append_transcript(
        &transcript,
        &message(
            "literal-racing",
            "2026-08-16T00:00:03Z",
            "gemini",
            "race-before",
        ),
    );
    let hook_path = fs::canonicalize(&transcript).unwrap();
    set_after_jsonl_semantic_preflight_hook(hook_path.clone(), move || {
        let before = fs::read_to_string(&hook_path).unwrap();
        let after = before.replace("race-before", "race-after!");
        assert_eq!(before.len(), after.len());
        assert_ne!(before, after);
        fs::write(&hook_path, after).unwrap();
    });

    let failed = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(matches!(
        failed.failed_routes.as_slice(),
        [failure]
            if failure.class == SourceBackedSourceFailureClass::SourceChanged
                && failure.carried_forward
    ));
    assert_eq!(certified_prefix_bytes(&index), appended_checkpoint);
    assert_eq!(indexed_records(&index), appended_records);

    let recovered = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(recovered.failed_routes.is_empty());
    let recovered_records = indexed_records(&index);
    assert_literal_bodies(
        &recovered_records,
        &["literal first", "literal second", "race-after!"],
    );
    assert_eq!(recovered_records[0].event_id, cold_records[0].event_id);
    assert_eq!(
        certified_prefix_bytes(&index),
        fs::metadata(&transcript).unwrap().len()
    );
}

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{CaptureProvider, CoreRecord};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

const MUX_SOURCE_FORMAT: &str = "mux_session_jsonl_tree";

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn mux_registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Mux,
            path: root.to_path_buf(),
            exists: true,
            source_format: MUX_SOURCE_FORMAT,
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

fn mux_message(workspace: &str, id: &str, sequence: u64, text: &str) -> Value {
    json!({
        "workspaceId": workspace,
        "id": id,
        "role": "user",
        "parts": [{"type": "text", "text": text}],
        "metadata": {"historySequence": sequence},
    })
}

fn write_jsonl(path: &Path, records: &[Value]) {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_jsonl(path: &Path, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

fn mux_records(index: &Path) -> Vec<CoreRecord> {
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
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.event_sequence);
    records
}

#[test]
fn mux_compound_route_cold_noop_companion_replacement_and_deletion() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("mux-sessions");
    let session = root.join("route-session");
    fs::create_dir_all(&session).unwrap();
    let archive = session.join("chat-archive.jsonl");
    let chat = session.join("chat.jsonl");
    write_jsonl(
        &archive,
        &[mux_message(
            "route-session",
            "archive-event",
            0,
            "archived cold record",
        )],
    );
    write_jsonl(
        &chat,
        &[mux_message(
            "route-session",
            "chat-event",
            1,
            "active cold record",
        )],
    );
    let registry = mux_registry(&root);
    let index = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(cold.sources.len(), 1);
    let cold_records = mux_records(&index);
    assert_eq!(cold_records.len(), 2);
    assert_eq!(
        cold_records
            .iter()
            .map(|record| record.content.meaningful_text())
            .collect::<Vec<_>>(),
        ["archived cold record", "active cold record"]
    );

    let noop = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(noop.failed_routes.is_empty());
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(noop.sources, cold.sources);
    assert_eq!(mux_records(&index), cold_records);

    append_jsonl(
        &chat,
        &mux_message(
            "route-session",
            "appended-event",
            2,
            "active appended record",
        ),
    );
    let appended = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(appended.failed_routes.is_empty());
    assert_ne!(appended.commit.generation_id, noop.commit.generation_id);
    assert_eq!(appended.sources.len(), 1);
    let appended_records = mux_records(&index);
    assert_eq!(appended_records.len(), 3);
    assert_eq!(appended_records[..2], cold_records);
    assert_eq!(
        appended_records[2].content.meaningful_text(),
        "active appended record"
    );

    write_jsonl(
        &chat,
        &[mux_message(
            "route-session",
            "chat-event",
            1,
            "active replacement record",
        )],
    );
    let replacement =
        refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(replacement.failed_routes.is_empty());
    assert_ne!(
        replacement.commit.generation_id,
        appended.commit.generation_id
    );
    assert_eq!(replacement.sources.len(), 1);
    assert!(replacement.sources[0]
        .observation()
        .source()
        .exact_descriptor_eq(cold.sources[0].observation().source()));
    let replacement_records = mux_records(&index);
    assert_eq!(replacement_records.len(), 2);
    assert_eq!(replacement_records[0].event_id, cold_records[0].event_id);
    assert_eq!(replacement_records[1].event_id, cold_records[1].event_id);
    assert_eq!(
        replacement_records[1].content.meaningful_text(),
        "active replacement record"
    );

    fs::remove_file(archive).unwrap();
    fs::remove_file(chat).unwrap();
    let deleted = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(deleted.failed_routes.is_empty());
    assert!(deleted.sources.is_empty());
    assert_eq!(deleted.removals.len(), 1);
    assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 0);
}

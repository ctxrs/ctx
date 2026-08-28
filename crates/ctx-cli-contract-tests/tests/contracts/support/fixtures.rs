use ctx_history_index::{CoreRecord, GenerationWriter, VerifiedIndex, WriterOptions};
use rusqlite::Connection;
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn provider_history_fixture(name: &str) -> String {
    materialized_fixture("provider-history", name)
}

pub(crate) fn custom_history_fixture(name: &str) -> String {
    materialized_fixture("custom-history-jsonl", name)
}

pub(crate) fn materialized_fixture(category: &str, name: &str) -> String {
    let source = match category {
        "provider-history" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider-history")
            .join(name),
        "custom-history-jsonl" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/custom-history-jsonl")
            .join(name),
        "provider" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider")
            .join(name),
        _ => panic!("unknown fixture category {category}"),
    };
    let materialized_root = std::env::var_os("TEST_TMPDIR")
        .map(|path| PathBuf::from(path).join("test-data/materialized-fixtures"))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap()
                .join("target/test-data/materialized-fixtures")
        });
    fs::create_dir_all(&materialized_root).unwrap();
    let unique = format!(
        "{}-{}-{}-{}",
        category,
        name.replace(['/', '\\', '.'], "_"),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let private_root = materialized_root.join(unique);
    fs::create_dir_all(&private_root).unwrap();
    let mut target = private_root.join("fixture");
    if source.is_file() {
        if let Some(extension) = source.extension() {
            target.set_extension(extension);
        }
    }
    if source.is_dir() {
        copy_dir_all(&source, &target);
    } else {
        fs::copy(&source, &target).unwrap();
    }
    target.to_str().unwrap().to_owned()
}

pub(crate) fn write_sqlite_fixture_from_sql(sql_fixture: &str, db_path: &Path) {
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let sql = fs::read_to_string(provider_history_fixture(sql_fixture)).unwrap();
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(&sql).unwrap();
}

pub(crate) fn initialize_generation_only_core(data_root: &Path) -> String {
    let index_root = data_root.join("search").join("lexical");
    let writer = GenerationWriter::open(
        &index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let core_receipt = writer.commit(|_| true).unwrap();
    let verified = VerifiedIndex::open_pinned(index_root).unwrap();
    assert_eq!(verified.generation_id(), core_receipt.generation_id);
    core_receipt.generation_id
}

pub(crate) fn initialize_authoritative_empty_core(data_root: &Path) -> String {
    let generation_id = initialize_generation_only_core(data_root);
    let route_identity = "ab".repeat(32);
    let receipt = json!({
        "published_generation": generation_id,
        "generation_changed": true,
        "current": {
            "current_source_count": 0,
            "current_indexed_documents": 0,
            "current_complete_records": 0,
            "current_retained_records": 0,
            "current_rejected_records": 0,
            "current_ignored_records": 0,
            "current_certified_source_bytes": 0,
            "current_sources_with_rejections": 0,
            "removed_source_count": 0,
        },
        "outcome": "completed",
        "selected_route_total": 1,
        "successful_route_total": 1,
        "source_failure_total": 0,
        "source_failures_omitted": 0,
        "rejected_record_total": 0,
        "rejection_diagnostics_omitted": 0,
        "route_results": {(route_identity): ["s", true]},
        "zero_source_authority": {
            "generation_id": generation_id,
            "route_kinds": "e",
        },
        "catalog_route_bindings": {},
    });
    republish_active_generation_metadata(
        data_root,
        &generation_id,
        serde_json::to_vec(&json!({
            "version": 3,
            "request_id": "mcp-authoritative-empty-fixture",
            "operation": "refresh",
            "refresh_scope": {"kind": "all"},
            "receipt": receipt,
            "route_observations": [null],
            "route_controls": {},
        }))
        .unwrap(),
    );
    generation_id
}

pub(crate) fn republish_active_generation_metadata(
    data_root: &Path,
    generation_id: &str,
    metadata: Vec<u8>,
) {
    let index_root = data_root.join("search").join("lexical");
    GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap()
        .republish_current_publication_metadata(generation_id, metadata)
        .unwrap();
}

pub(crate) fn write_codex_message_fixture(root: &Path, session_id: &str, message: &str) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let path = root.join(format!("rollout-{session_id}.jsonl"));
    let records = [
        json!({
            "timestamp": "2026-08-02T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": "2026-08-02T12:00:00Z",
                "cwd": "/workspace/huge-grapheme",
                "originator": "codex_cli_rs",
                "cli_version": "0.1.0",
                "source": "cli",
                "model_provider": "openai"
            }
        }),
        json!({
            "timestamp": "2026-08-02T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": message
                }]
            }
        }),
    ];
    let encoded = records
        .iter()
        .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
        .collect::<String>();
    fs::write(&path, encoded).unwrap();
    path
}

pub(crate) fn provider_core_records(data_root: &Path, provider: &str) -> Vec<CoreRecord> {
    let index = VerifiedIndex::open_pinned(data_root.join("search/lexical")).unwrap();
    let sources = index
        .manifest()
        .sources
        .iter()
        .map(|certificate| certificate.observation().source())
        .filter(|source| source.provider() == provider)
        .cloned()
        .collect::<Vec<_>>();
    let mut records = Vec::new();
    for source in sources {
        let mut cursor = None;
        loop {
            let page = index
                .core_source_event_page(&source, cursor.as_ref(), 4_096)
                .unwrap();
            records.extend(page.items.into_iter().map(|item| item.core_record));
            if page.terminal {
                break;
            }
            cursor = page.next_cursor;
        }
    }
    records
}

pub(crate) fn provider_core_counts(data_root: &Path, provider: &str) -> (usize, usize) {
    let records = provider_core_records(data_root, provider);
    let sessions = records
        .iter()
        .map(|record| record.session_id.as_uuid())
        .collect::<BTreeSet<_>>()
        .len();
    (sessions, records.len())
}

pub(crate) fn copy_dir_all(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let entry_path = entry.path();
        let target = to.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_all(&entry_path, &target);
        } else {
            fs::copy(entry_path, target).unwrap();
        }
    }
}

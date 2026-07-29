use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};
use ctx_history_search::sql_compatibility_path;
use ctx_history_store::{CommittedCoreGeneration, SourceBackedRelationalProjection};
use rusqlite::Connection;
use std::{
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
    let mut target = materialized_root.join(unique);
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

pub(crate) fn initialize_generation_only_sql_projection(data_root: &Path) -> String {
    let index_root = data_root.join("search").join("lexical");
    let writer = GenerationWriter::open(
        &index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap();
    let core_receipt = writer.commit(|_| true).unwrap();
    let verified = VerifiedIndex::open(index_root).unwrap();
    assert_eq!(verified.generation_id(), core_receipt.generation_id);

    let generation = CommittedCoreGeneration {
        generation_id: core_receipt.generation_id.clone(),
        manifest_json: serde_json::to_vec(verified.manifest()).unwrap(),
        indexed_documents: core_receipt.indexed_documents,
        certified_sources: core_receipt.certified_sources,
        certified_source_bytes: core_receipt.certified_source_bytes,
    };
    let mut projection =
        SourceBackedRelationalProjection::open(sql_compatibility_path(data_root)).unwrap();
    let relational_receipt = projection.rebuild(&generation, std::iter::empty()).unwrap();
    assert_eq!(
        relational_receipt.core_generation_id,
        core_receipt.generation_id
    );
    core_receipt.generation_id
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

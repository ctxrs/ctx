use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};
use ctx_history_relational::{
    CommittedCoreGeneration, RelationalSourceHealth, RelationalSourceMetadata,
    SourceBackedRelationalProjection,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
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

    let manifest = verified.manifest();
    let sources = manifest
        .sources
        .iter()
        .map(|certificate| RelationalSourceMetadata {
            source: certificate.observation().source().clone(),
            parser_revision: certificate.parser_revision().to_owned(),
            revision_digest: Sha256::digest(serde_json::to_vec(certificate).unwrap()).into(),
            indexed_event_count: certificate.counts().indexed_documents,
            health: RelationalSourceHealth::Ready,
        })
        .collect();
    let generation = CommittedCoreGeneration {
        generation_id: verified.generation_id().to_owned(),
        manifest_version: manifest.manifest_version,
        core_record_version: manifest.core_record_version,
        core_record_contract_fingerprint: manifest.core_record_contract_fingerprint.clone(),
        lexical_schema_version: manifest.lexical_schema_version,
        policy_schema_hash: manifest.policy_schema_hash.clone(),
        indexed_documents: manifest.indexed_documents,
        sources,
    };
    let mut projection =
        SourceBackedRelationalProjection::open(data_root.join("relational.sqlite")).unwrap();
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

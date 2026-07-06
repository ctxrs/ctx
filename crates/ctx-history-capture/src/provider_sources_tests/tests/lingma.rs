#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn lingma_discovery_uses_waylog_default_local_db_paths() {
    let temp = tempfile::tempdir().unwrap();
    let stable = temp
        .path()
        .join(".lingma/vscode/sharedClientCache/cache/db/local.db");
    let insiders = temp
        .path()
        .join(".lingma/vscode-insiders/sharedClientCache/cache/db/local.db");
    write_lingma_discovery_db(&stable);
    write_lingma_discovery_db(&insiders);

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Lingma);
    for path in [&stable, &insiders] {
        let source = sources
            .iter()
            .find(|source| source.path == *path)
            .unwrap_or_else(|| panic!("missing Lingma source {path:?} in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "lingma_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }
}

pub(crate) fn write_lingma_discovery_db(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE chat_record (
            session_id TEXT,
            request_id TEXT,
            chat_prompt TEXT,
            summary TEXT,
            error_result TEXT,
            gmt_create INTEGER,
            extra TEXT
        );
        "#,
    )
    .unwrap();
}

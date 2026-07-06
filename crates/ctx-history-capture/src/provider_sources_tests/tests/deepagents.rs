#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn deepagents_discovery_uses_default_sessions_db() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join(".deepagents/.state/sessions.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();

    let empty_source =
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::DeepAgents)
            .into_iter()
            .find(|source| source.path == db)
            .unwrap();
    assert_eq!(empty_source.status, ProviderSourceStatus::Missing);

    std::fs::write(&db, b"not sqlite").unwrap();
    let unreadable_source =
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::DeepAgents)
            .into_iter()
            .find(|source| source.path == db)
            .unwrap();
    assert_eq!(unreadable_source.status, ProviderSourceStatus::Unknown);

    std::fs::copy(
        shared_provider_history_fixture("deepagents/v1/sessions.db"),
        &db,
    )
    .unwrap();
    let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::DeepAgents)
        .into_iter()
        .find(|source| source.path == db)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "deepagents_sessions_sqlite");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

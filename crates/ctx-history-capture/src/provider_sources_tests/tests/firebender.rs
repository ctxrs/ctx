#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn firebender_discovery_uses_current_project_chat_history_db() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let nested = project.join("src/module");
    let db = project.join(".idea/firebender/chat_history.db");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    Connection::open(&db)
        .unwrap()
        .execute_batch(
            r#"
            CREATE TABLE chat_sessions (
                id TEXT PRIMARY KEY,
                messages_json TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
    let _cwd = CwdGuard::set(&nested);

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Firebender);
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::Firebender && source.path == db)
        .unwrap_or_else(|| panic!("missing Firebender cwd source in {sources:#?}"));

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "firebender_chat_history_sqlite");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

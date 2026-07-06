#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn cline_discovery_uses_env_data_dirs() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let custom = temp.path().join("custom-cline-data");
    write_task_json_discovery_task(&custom, "cline-env-task", "api_conversation_history.json");
    let _data_dir = EnvGuard::set("CLINE_DATA_DIR", custom.as_os_str());
    let _cline_dir = EnvGuard::remove("CLINE_DIR");
    let _session_dir = EnvGuard::remove("CLINE_SESSION_DATA_DIR");
    let _db_dir = EnvGuard::remove("CLINE_DB_DATA_DIR");

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Cline);
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::Cline && source.path == custom)
        .unwrap();

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

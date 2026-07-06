#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn goose_discovery_uses_path_root_data_sessions_db() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("goose-root");
    let sessions = root.join("data/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("sessions.db"), b"sqlite fixture marker").unwrap();
    let _path_root = EnvGuard::set("GOOSE_PATH_ROOT", &root);

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Goose);
    let source = sources
        .iter()
        .find(|source| source.path == sessions.join("sessions.db"))
        .unwrap_or_else(|| panic!("missing Goose path-root source in {sources:#?}"));
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "goose_sessions_sqlite");
}

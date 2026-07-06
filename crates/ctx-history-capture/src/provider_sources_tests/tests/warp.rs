#[allow(unused_imports)]
use super::*;

#[cfg(unix)]
#[test]
pub(crate) fn warp_discovery_uses_documented_state_and_localappdata_paths() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let xdg_state = temp.path().join("xdg-state");
    let local_app_data = temp.path().join("local-app-data");
    let linux_db = xdg_state.join("warp-terminal/warp.sqlite");
    let windows_db = local_app_data.join("warp/Warp/data/warp.sqlite");
    std::fs::create_dir_all(linux_db.parent().unwrap()).unwrap();
    std::fs::create_dir_all(windows_db.parent().unwrap()).unwrap();
    std::fs::write(&linux_db, b"sqlite fixture marker").unwrap();
    std::fs::write(&windows_db, b"sqlite fixture marker").unwrap();
    let _xdg_state = EnvGuard::set("XDG_STATE_HOME", xdg_state.as_os_str());
    let _local_app_data = EnvGuard::set("LOCALAPPDATA", local_app_data.as_os_str());

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Warp);
    for path in [&linux_db, &windows_db] {
        let source = sources
            .iter()
            .find(|source| source.path == *path)
            .unwrap_or_else(|| panic!("missing Warp source {path:?} in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "warp_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());
    }
}

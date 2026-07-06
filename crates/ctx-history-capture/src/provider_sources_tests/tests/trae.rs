#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn trae_discovery_uses_workspace_storage_roots_as_native_sources() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let appdata = temp.path().join("appdata");
    let _appdata = EnvGuard::set("APPDATA", appdata.as_os_str());

    let standard_mac_root = temp
        .path()
        .join("Library/Application Support/Trae/User/workspaceStorage");
    let mac_root = temp
        .path()
        .join("Library/Application Support/Trae CN/User/workspaceStorage");
    let standard_appdata_root = appdata.join("Trae/User/workspaceStorage");
    let appdata_root = appdata.join("Trae CN/User/workspaceStorage");
    for root in [
        &standard_mac_root,
        &mac_root,
        &standard_appdata_root,
        &appdata_root,
    ] {
        write_trae_discovery_db(&root.join("workspace-hash/state.vscdb"));
    }

    let empty_root = temp
        .path()
        .join("Library/Application Support/Trae/User/workspaceStorage-empty");
    write_trae_non_chat_state_db(&empty_root.join("workspace-hash/state.vscdb"));
    assert_eq!(
        has_trae_state_vscdb_chat_history(&empty_root, 10_000),
        BoundedProbe::NotFound
    );

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Trae);
    for path in [
        &standard_mac_root,
        &mac_root,
        &standard_appdata_root,
        &appdata_root,
    ] {
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::Trae && source.path == *path)
            .unwrap_or_else(|| panic!("missing Trae source {path:?} in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, TRAE_STATE_VSCDB_SOURCE_FORMAT);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());
        assert!(source.unsupported_reason.is_none());
    }
}

pub(crate) fn write_trae_discovery_db(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
        rusqlite::params![
            "memento/icube-ai-agent-storage",
            r#"{"list":[{"id":"input-1","messages":[{"role":"user","content":"trae discovery"}]}]}"#
        ],
    )
    .unwrap();
}

pub(crate) fn write_trae_non_chat_state_db(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES ('workbench.view.extension', '{}')",
        [],
    )
    .unwrap();
}

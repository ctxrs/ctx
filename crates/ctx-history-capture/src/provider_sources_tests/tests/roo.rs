#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn roo_discovery_uses_custom_storage_setting() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let custom = temp.path().join("roo-custom-storage");
    write_task_json_discovery_task(&custom, "roo-custom-task", "history_item.json");
    let settings = temp.path().join(".config/Code/User/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{"roo-cline.customStoragePath":"~/roo-custom-storage"}"#,
    )
    .unwrap();
    let _roo_code = EnvGuard::remove("ROO_CODE_DATA_DIR");
    let _roo = EnvGuard::remove("ROO_DATA_DIR");
    let _roo_cline = EnvGuard::remove("ROO_CLINE_DATA_DIR");

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::RooCode);
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::RooCode && source.path == custom)
        .unwrap();

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

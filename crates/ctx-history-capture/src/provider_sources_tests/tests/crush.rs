#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn crush_discovery_uses_global_config_data_directory() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("crush.json");
    let data_dir = temp.path().join("custom-crush-data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("crush.db"), b"sqlite fixture marker").unwrap();
    std::fs::write(
        &config,
        format!(
            "{{\"options\":{{\"data_directory\":\"{}\"}}}}",
            data_dir.display()
        ),
    )
    .unwrap();
    let _config = EnvGuard::set("CRUSH_GLOBAL_CONFIG", &config);
    let _data = EnvGuard::remove("CRUSH_GLOBAL_DATA");

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Crush);
    let source = sources
        .iter()
        .find(|source| source.path == data_dir.join("crush.db"))
        .unwrap_or_else(|| panic!("missing Crush config source in {sources:#?}"));
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "crush_sqlite");
}

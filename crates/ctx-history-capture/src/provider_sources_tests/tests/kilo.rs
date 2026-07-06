#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn kilo_discovery_uses_xdg_kilo_db_env_override_and_channel_dbs() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let _kilo_db = EnvGuard::remove("KILO_DB");
    let _xdg_data = EnvGuard::remove("XDG_DATA_HOME");
    let _config_dir = EnvGuard::remove("KILO_CONFIG_DIR");
    let _disable_channel = EnvGuard::remove("KILO_DISABLE_CHANNEL_DB");

    let data_dir = temp.path().join(".local/share/kilo");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("kilo.db"), b"sqlite fixture marker").unwrap();
    std::fs::write(data_dir.join("kilo-dev.db"), b"sqlite fixture marker").unwrap();
    std::fs::write(data_dir.join("opencode-dev.db"), b"ignored").unwrap();

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
    assert_eq!(
        sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>(),
        vec![data_dir.join("kilo.db"), data_dir.join("kilo-dev.db")]
    );
    assert!(sources
        .iter()
        .all(|source| source.status == ProviderSourceStatus::Available));

    let xdg_data = temp.path().join("xdg-data");
    let xdg_kilo = xdg_data.join("kilo");
    std::fs::create_dir_all(&xdg_kilo).unwrap();
    std::fs::write(xdg_kilo.join("kilo.db"), b"sqlite fixture marker").unwrap();
    let _xdg_data_set = EnvGuard::set("XDG_DATA_HOME", xdg_data.as_os_str());
    let _config_dir_set = EnvGuard::set("KILO_CONFIG_DIR", temp.path().join("config"));

    let xdg_sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
    assert_eq!(xdg_sources[0].path, xdg_kilo.join("kilo.db"));
    assert_ne!(
        xdg_sources[0].path,
        temp.path().join("config").join("kilo.db")
    );

    let _relative_db = EnvGuard::set("KILO_DB", "relative-kilo.db");
    std::fs::write(xdg_kilo.join("relative-kilo.db"), b"sqlite fixture marker").unwrap();
    let relative_sources =
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
    assert_eq!(relative_sources.len(), 1);
    assert_eq!(relative_sources[0].path, xdg_kilo.join("relative-kilo.db"));
    assert_eq!(relative_sources[0].status, ProviderSourceStatus::Available);

    let absolute_db = temp.path().join("absolute-kilo.db");
    std::fs::write(&absolute_db, b"sqlite fixture marker").unwrap();
    let _absolute_db = EnvGuard::set("KILO_DB", absolute_db.as_os_str());
    let absolute_sources =
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
    assert_eq!(absolute_sources.len(), 1);
    assert_eq!(absolute_sources[0].path, absolute_db);
    assert_eq!(absolute_sources[0].status, ProviderSourceStatus::Available);
}

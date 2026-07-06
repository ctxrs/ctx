#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn kimi_discovery_uses_home_env_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let kimi_home = temp.path().join("kimi-home");
    write_kimi_discovery_wire(&kimi_home);
    let _home = EnvGuard::set("KIMI_CODE_HOME", kimi_home.as_os_str());

    let sources = discover_provider_sources(temp.path());
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::KimiCodeCli && source.path == kimi_home)
        .unwrap_or_else(|| panic!("missing Kimi Code CLI source in {sources:#?}"));
    assert_eq!(source.status, ProviderSourceStatus::Available);
    let crush = temp.path().join(".local/share/crush");
    std::fs::create_dir_all(&crush).unwrap();
    std::fs::write(crush.join("crush.db"), b"sqlite fixture marker").unwrap();
    let crush_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Crush)
        .unwrap();
    assert_eq!(crush_source.status, ProviderSourceStatus::Available);
    assert_eq!(crush_source.source_format, "crush_sqlite");

    let goose = temp.path().join(".local/share/goose/sessions");
    std::fs::create_dir_all(&goose).unwrap();
    std::fs::write(goose.join("sessions.db"), b"sqlite fixture marker").unwrap();
    let goose_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Goose)
        .unwrap();
    assert_eq!(goose_source.status, ProviderSourceStatus::Available);
    assert_eq!(goose_source.source_format, "goose_sessions_sqlite");
}

pub(crate) fn write_kimi_discovery_wire(home: &Path) {
    let agent = home.join("sessions/wd_project_abc123/kimi-session/agents/main");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("wire.jsonl"), "{}\n").unwrap();
}

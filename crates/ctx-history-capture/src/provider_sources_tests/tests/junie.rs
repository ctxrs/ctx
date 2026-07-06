#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn junie_discovery_uses_default_sessions_and_env_overrides() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let _sessions_dir = EnvGuard::remove("JUNIE_SESSIONS_DIR");
    let _junie_home = EnvGuard::remove("JUNIE_HOME");

    let default_sessions = temp.path().join(".junie/sessions");
    std::fs::create_dir_all(&default_sessions).unwrap();
    let empty_source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Junie)
        .into_iter()
        .find(|source| source.path == default_sessions)
        .unwrap();
    assert_eq!(empty_source.status, ProviderSourceStatus::Empty);
    assert_eq!(
        empty_source.source_format,
        "junie_session_events_jsonl_tree"
    );

    write_junie_discovery_session(&default_sessions, "session-260607-110000-default");
    let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Junie)
        .into_iter()
        .find(|source| source.path == default_sessions)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);

    let env_sessions = temp.path().join("junie-env-sessions");
    write_junie_discovery_session(&env_sessions, "session-260607-110001-env");
    let _sessions_dir = EnvGuard::set("JUNIE_SESSIONS_DIR", env_sessions.as_os_str());

    let junie_home = temp.path().join("junie-home");
    let home_sessions = junie_home.join("sessions");
    write_junie_discovery_session(&home_sessions, "session-260607-110002-home");
    let _junie_home = EnvGuard::set("JUNIE_HOME", junie_home.as_os_str());

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Junie);
    for path in [&env_sessions, &home_sessions] {
        let source = sources
            .iter()
            .find(|source| source.path == *path)
            .unwrap_or_else(|| panic!("missing Junie source {path:?} in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "junie_session_events_jsonl_tree");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }
}

pub(crate) fn write_junie_discovery_session(sessions: &Path, session_id: &str) {
    std::fs::create_dir_all(sessions.join(session_id)).unwrap();
    std::fs::write(
        sessions.join("index.jsonl"),
        format!(r#"{{"sessionId":"{session_id}","createdAt":1783339200000}}"#),
    )
    .unwrap();
    std::fs::write(
        sessions.join(session_id).join("events.jsonl"),
        "{\"kind\":\"UserPromptEvent\",\"prompt\":\"Junie discovery\"}\n",
    )
    .unwrap();
}

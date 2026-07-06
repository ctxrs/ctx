#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn mistral_vibe_discovery_uses_default_and_home_env_sessions() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let _home = EnvGuard::remove("VIBE_HOME");

    let default_sessions = temp.path().join(".vibe/logs/session");
    std::fs::create_dir_all(&default_sessions).unwrap();
    let empty_source =
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::MistralVibe)
            .into_iter()
            .find(|source| source.path == default_sessions)
            .unwrap();
    assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

    write_mistral_vibe_discovery_session(&default_sessions);
    let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::MistralVibe)
        .into_iter()
        .find(|source| source.path == default_sessions)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "mistral_vibe_session_jsonl_tree");
    assert_eq!(source.import_support, ProviderImportSupport::Native);

    let custom_home = temp.path().join("custom-vibe");
    let custom_sessions = custom_home.join("logs/session");
    write_mistral_vibe_discovery_session(&custom_sessions);
    let _home = EnvGuard::set("VIBE_HOME", custom_home.as_os_str());
    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::MistralVibe);
    assert!(sources.iter().any(|source| {
        source.path == custom_sessions && source.status == ProviderSourceStatus::Available
    }));
}

pub(crate) fn write_mistral_vibe_discovery_session(sessions: &Path) {
    let session = sessions.join("session_20260704_120000_vibe1234");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(
        session.join("meta.json"),
        r#"{"session_id":"mistral-vibe-discovery","start_time":"2026-07-04T12:00:00Z","end_time":null,"git_commit":null,"git_branch":null,"environment":{"working_directory":"/workspace"},"username":"fixture"}"#,
    )
    .unwrap();
    std::fs::write(session.join("messages.jsonl"), "{}\n").unwrap();
}

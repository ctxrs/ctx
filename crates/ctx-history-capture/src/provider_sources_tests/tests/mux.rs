#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn mux_discovery_uses_default_and_mux_root_sessions() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let _home = EnvGuard::remove("MUX_ROOT");

    let default_sessions = temp.path().join(".mux/sessions");
    std::fs::create_dir_all(&default_sessions).unwrap();
    let empty_source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Mux)
        .into_iter()
        .find(|source| source.path == default_sessions)
        .unwrap();
    assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

    write_mux_discovery_session(&default_sessions);
    let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Mux)
        .into_iter()
        .find(|source| source.path == default_sessions)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "mux_session_jsonl_tree");
    assert_eq!(source.import_support, ProviderImportSupport::Native);

    let custom_home = temp.path().join("custom-mux");
    let custom_sessions = custom_home.join("sessions");
    write_mux_discovery_session(&custom_sessions);
    let _home = EnvGuard::set("MUX_ROOT", custom_home.as_os_str());
    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Mux);
    assert!(sources.iter().any(|source| {
        source.path == custom_sessions && source.status == ProviderSourceStatus::Available
    }));
}

pub(crate) fn write_mux_discovery_session(sessions: &Path) {
    let session = sessions.join("mux-discovery");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(
        session.join("chat.jsonl"),
        r#"{"id":"msg-mux-discovery","role":"user","parts":[{"type":"text","text":"mux discovery"}],"metadata":{"historySequence":0},"workspaceId":"mux-discovery"}"#,
    )
    .unwrap();
}

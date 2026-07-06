#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn codebuddy_discovery_uses_localappdata_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let local_app_data = temp.path().join("local-app-data");
    let codebuddy = local_app_data.join("CodeBuddyExtension");
    let session = codebuddy
        .join("CodeBuddyIDE/default/history/11112222333344445555666677778888/session-alpha");
    std::fs::create_dir_all(session.join("messages")).unwrap();
    std::fs::write(
        session.join("index.json"),
        r#"{"messages":[{"id":"msg-1","role":"user"}]}"#,
    )
    .unwrap();
    std::fs::write(
        session.join("messages/msg-1.json"),
        r#"{"message":"hello"}"#,
    )
    .unwrap();
    let _local_app_data = EnvGuard::set("LOCALAPPDATA", local_app_data.as_os_str());

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::CodeBuddy);
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::CodeBuddy && source.path == codebuddy)
        .unwrap_or_else(|| panic!("missing CodeBuddy LOCALAPPDATA source in {sources:#?}"));

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

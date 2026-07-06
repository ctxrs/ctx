#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn qwen_discovery_uses_runtime_and_home_env_overrides() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path().join("qwen-runtime");
    write_qwen_discovery_chat(&runtime.join("projects"));
    let qwen_home = temp.path().join("qwen-home");
    write_qwen_discovery_chat(&qwen_home.join("projects"));
    let _runtime = EnvGuard::set("QWEN_RUNTIME_DIR", runtime.as_os_str());
    let _home = EnvGuard::set("QWEN_HOME", qwen_home.as_os_str());

    let sources = discover_provider_sources(temp.path());
    for path in [runtime.join("projects"), qwen_home.join("projects")] {
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::QwenCode && source.path == path)
            .unwrap_or_else(|| panic!("missing Qwen Code source for {path:?}: {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }
}

pub(crate) fn write_qwen_discovery_chat(projects: &Path) {
    let chats = projects.join("project/chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(chats.join("qwen-discovery.jsonl"), "{}\n").unwrap();
}

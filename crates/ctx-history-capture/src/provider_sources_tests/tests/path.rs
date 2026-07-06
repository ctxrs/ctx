#[allow(unused_imports)]
use super::*;

pub(crate) struct CwdGuard {
    pub(crate) original: PathBuf,
}

impl CwdGuard {
    pub(crate) fn set(path: &Path) -> Self {
        let original = env::current_dir().unwrap();
        env::set_current_dir(path).unwrap();
        Self { original }
    }
}

#[test]
pub(crate) fn continue_discovery_uses_global_dir_env_sessions_subdir() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let continue_home = temp.path().join("continue-home");
    let sessions = continue_home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("session.json"), "{}\n").unwrap();
    let _global_dir = EnvGuard::set("CONTINUE_GLOBAL_DIR", continue_home.as_os_str());

    let sources = discover_provider_sources(temp.path());
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::Continue && source.path == sessions)
        .unwrap();

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "continue_cli_sessions_json");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

#[test]
pub(crate) fn default_location_probe_does_not_fallback_to_path_existence_for_unhandled_providers() {
    let temp = tempfile::tempdir().unwrap();
    let existing = temp.path().join("shell-history");
    std::fs::write(&existing, "{}\n").unwrap();
    let location = ProviderDefaultLocation {
        path_components: &["shell-history"],
        source_format: "shell_history",
        source_kind: ProviderSourceKind::NativeHistory,
    };

    assert_eq!(
        default_location_import_probe(CaptureProvider::Shell, &location, &existing),
        BoundedProbe::NotFound
    );
}

pub(crate) fn write_task_json_discovery_task(root: &Path, task_id: &str, file_name: &str) {
    let task = root.join("tasks").join(task_id);
    std::fs::create_dir_all(&task).unwrap();
    std::fs::write(task.join(file_name), "[]").unwrap();
}

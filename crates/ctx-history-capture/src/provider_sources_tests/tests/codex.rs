#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn codex_default_source_is_empty_until_jsonl_sessions_exist() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| {
            source.provider == CaptureProvider::Codex
                && source.source_format == "codex_session_jsonl_tree"
        })
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Empty);

    std::fs::write(sessions.join("session.jsonl"), "{}\n").unwrap();
    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| {
            source.provider == CaptureProvider::Codex
                && source.source_format == "codex_session_jsonl_tree"
        })
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
}

#[cfg(unix)]
#[test]
pub(crate) fn default_source_probe_reports_unreadable_directory_as_unknown() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let original_permissions = std::fs::metadata(&sessions).unwrap().permissions();
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o000)).unwrap();

    if std::fs::read_dir(&sessions).is_ok() {
        std::fs::set_permissions(&sessions, original_permissions).unwrap();
        return;
    }

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| {
            source.provider == CaptureProvider::Codex
                && source.source_format == "codex_session_jsonl_tree"
        })
        .unwrap();
    std::fs::set_permissions(&sessions, original_permissions).unwrap();

    assert_eq!(source.status, ProviderSourceStatus::Unknown);
    assert!(source
        .unsupported_reason
        .unwrap()
        .contains("could not be read"));
}

#[cfg(unix)]
#[test]
pub(crate) fn default_source_probe_skips_unreadable_child_directory() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join(".codex/sessions");
    let readable = sessions.join("readable");
    let unreadable = sessions.join("unreadable");
    std::fs::create_dir_all(&readable).unwrap();
    std::fs::create_dir_all(&unreadable).unwrap();
    std::fs::write(readable.join("session.jsonl"), "{}\n").unwrap();

    let original_permissions = std::fs::metadata(&unreadable).unwrap().permissions();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

    if std::fs::read_dir(&unreadable).is_ok() {
        std::fs::set_permissions(&unreadable, original_permissions).unwrap();
        return;
    }

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| {
            source.provider == CaptureProvider::Codex
                && source.source_format == "codex_session_jsonl_tree"
        });
    std::fs::set_permissions(&unreadable, original_permissions).unwrap();

    let source = source.unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.unsupported_reason, None);
}

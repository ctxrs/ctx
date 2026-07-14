use ctx_history_core::CaptureProvider;

use super::super::probes::{default_location_import_probe, BoundedProbe};
use super::super::{
    discover_provider_sources, ProviderDefaultLocation, ProviderSourceKind, ProviderSourceStatus,
};
use super::support::{assert_source_status, tempdir};

#[test]
fn bounded_probe_reports_budget_exhausted_source_as_unknown() {
    let temp = tempdir();
    let claude = temp.path().join(".claude/projects");
    std::fs::create_dir_all(&claude).unwrap();
    for index in 0..10_001 {
        std::fs::create_dir(claude.join(format!("project-{index:05}"))).unwrap();
    }

    assert_source_status(
        temp.path(),
        CaptureProvider::Claude,
        ProviderSourceStatus::Unknown,
    );
}

#[test]
fn default_location_probe_does_not_fallback_to_path_existence_for_unhandled_providers() {
    let temp = tempdir();
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

#[cfg(unix)]
#[test]
fn default_source_probe_reports_unreadable_directory_as_unknown() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir();
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
fn default_source_probe_skips_unreadable_child_directory() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir();
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

use ctx_history_capture_model::ProviderRootDefinition;
use ctx_history_core::CaptureProvider;

use super::super::probes::BoundedProbe;
#[cfg(unix)]
use super::super::ProviderSource;
use super::super::{
    DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs, ProviderDefaultLocation,
    ProviderSourceKind, ProviderSourceStatus,
};
use super::support::{tempdir, EnvGuard, ENV_LOCK};

fn default_location_import_probe(
    data_root: Option<&std::path::Path>,
    provider: CaptureProvider,
    location: &ProviderDefaultLocation,
    path: &std::path::Path,
) -> BoundedProbe {
    super::super::probes::default_location_import_probe(
        &super::super::TEST_PROVIDER_PROBES,
        data_root,
        provider,
        location,
        path,
    )
}

#[cfg(unix)]
fn discover_provider_sources(home: &std::path::Path) -> Vec<ProviderSource> {
    super::super::discover_provider_sources(&super::super::TEST_PROVIDER_PROBES, home)
}

#[cfg(unix)]
fn discover_provider_sources_for_provider_report(
    home: &std::path::Path,
    provider: CaptureProvider,
) -> super::super::DiscoveryReport {
    super::super::discover_provider_sources_for_provider_report(
        &super::super::TEST_PROVIDER_PROBES,
        home,
        provider,
    )
}

#[test]
fn configured_codex_dense_date_directory_remains_available() {
    const FILES: usize = 10_000;
    const SEEDS: usize = 16;

    let temp = tempdir();
    let home = temp.path().join("codex-home");
    let sessions = home.join("sessions");
    let day = sessions.join("2025/01/02");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::create_dir(home.join("archived_sessions")).unwrap();
    std::fs::write(home.join("history.jsonl"), b"").unwrap();
    // Authored path-only fixtures: discovery does not parse session contents.
    let seeds = (0..SEEDS)
        .map(|index| {
            let path = temp.path().join(format!("seed-{index:02}"));
            std::fs::write(&path, b"{}\n").unwrap();
            path
        })
        .collect::<Vec<_>>();
    for index in 0..FILES {
        std::fs::hard_link(
            &seeds[index % SEEDS],
            day.join(format!("00000000-0000-4000-8000-{index:012x}.jsonl")),
        )
        .unwrap();
    }
    let context = DiscoveryContext::new(
        temp.path().to_path_buf(),
        temp.path().to_path_buf(),
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(vec![ProviderRootDefinition {
        id: "codex".to_owned(),
        provider: CaptureProvider::Codex,
        path: home.clone(),
        group: None,
        kind: None,
    }]);
    let report = super::super::discover_provider_sources_for_provider_with_context(
        &super::super::TEST_PROVIDER_PROBES,
        &context,
        CaptureProvider::Codex,
    );
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    assert_eq!(report.sources.len(), 3);
    for (path, status) in [
        (sessions, ProviderSourceStatus::Available),
        (home.join("archived_sessions"), ProviderSourceStatus::Empty),
        (home.join("history.jsonl"), ProviderSourceStatus::Available),
    ] {
        let source = report
            .sources
            .iter()
            .find(|source| source.path == path)
            .unwrap();
        assert_eq!(source.status, status, "{}", path.display());
    }
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
        default_location_import_probe(None, CaptureProvider::Shell, &location, &existing),
        BoundedProbe::NotFound
    );
}

#[cfg(unix)]
#[test]
fn default_source_probe_reports_unreadable_directory_as_unknown() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _codex_home = EnvGuard::remove("CODEX_HOME");
    let sessions = temp.path().join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let original_permissions = std::fs::metadata(&sessions).unwrap().permissions();
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o000)).unwrap();

    if std::fs::read_dir(&sessions).is_ok() {
        std::fs::set_permissions(&sessions, original_permissions).unwrap();
        return;
    }

    let report = discover_provider_sources_for_provider_report(temp.path(), CaptureProvider::Codex);
    std::fs::set_permissions(&sessions, original_permissions).unwrap();

    let source = report
        .sources
        .iter()
        .find(|source| source.path == sessions)
        .unwrap();
    assert!(source.exists);
    assert_eq!(source.status, ProviderSourceStatus::Unknown);
    assert!(report.issues.iter().any(|issue| {
        issue.provider == CaptureProvider::Codex
            && issue.path.as_deref() == Some(sessions.as_path())
            && issue.reason.contains("access was denied")
    }));
}

#[cfg(unix)]
#[test]
fn default_source_probe_skips_unreadable_child_directory() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _codex_home = EnvGuard::remove("CODEX_HOME");
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

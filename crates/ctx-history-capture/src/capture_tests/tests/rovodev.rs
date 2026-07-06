#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_rovodev_fixture_imports_searches_reimports_and_file_touches() {
    let temp = tempdir();
    let fixture = provider_history_fixture("rovodev/v1/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::RovoDev, fixture.clone());
    assert_eq!(source.source_format, "rovodev_session_json_tree");
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_rovodev_history(
        &fixture,
        &mut store,
        RovoDevImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T15:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..RovoDevImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    assert!(store
        .search_event_hits("rovodev fixture oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::RovoDev)));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "src/rovodev_oracle.rs"));

    let second = import_rovodev_history(
        &fixture,
        &mut store,
        RovoDevImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..RovoDevImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
}

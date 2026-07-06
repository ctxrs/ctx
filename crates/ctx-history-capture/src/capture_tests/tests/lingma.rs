#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_lingma_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("lingma/v1/local.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Lingma, fixture.clone());
    assert_eq!(source.source_format, LINGMA_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_lingma_sqlite(
        &fixture,
        &mut store,
        LingmaSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T16:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..LingmaSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 6);

    let alpha = provider_session_uuid(CaptureProvider::Lingma, "lingma-session-1");
    let events = store.events_for_session(alpha).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    assert_eq!(events[1].sync.fidelity, Fidelity::SummaryOnly);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("lingma oracle prompt update"));
    assert!(rendered.contains("src/lingma_fixture.rs"));
    assert!(rendered.contains("Lingma summary oracle answer"));
    assert!(rendered.contains("summary_only"));
    assert!(rendered.contains("assistant_content_caveat"));
    assert!(store
        .search_event_hits("Lingma summary oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Lingma)));
    assert!(store
        .search_event_hits("lingma oracle prompt update", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Lingma)));

    let error_session = provider_session_uuid(CaptureProvider::Lingma, "lingma-session-2");
    let error_events = store.events_for_session(error_session).unwrap();
    assert_eq!(error_events.len(), 2);
    assert_eq!(error_events[1].event_type, EventType::Notice);
    assert!(serde_json::to_string(&error_events)
        .unwrap()
        .contains("sanitized Lingma error"));

    let second = import_lingma_sqlite(
        &fixture,
        &mut store,
        LingmaSqliteImportOptions {
            allow_partial_failures: true,
            ..LingmaSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 6);
}

#[test]
pub(crate) fn native_lingma_import_reports_corrupt_sqlite() {
    let temp = tempdir();
    let db = temp.path().join("corrupt-lingma.db");
    fs::write(&db, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_lingma_sqlite(&db, &mut store, LingmaSqliteImportOptions::default())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("not a database") || err.contains("sqlite"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
pub(crate) fn native_lingma_normalizer_rejects_symlinked_sqlite() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = provider_history_fixture("lingma/v1/local.db");
    let link = temp.path().join("linked-lingma.db");
    symlink(&fixture, &link).unwrap();

    let err = normalize_lingma_sqlite(&link, &ProviderAdapterContext::default()).unwrap_err();
    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-lingma.db")
                && reason == "symlinked provider transcript files are rejected"
    ));
}

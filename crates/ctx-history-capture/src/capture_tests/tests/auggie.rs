#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_auggie_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("auggie/v0.32.0/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Auggie, fixture.clone());
    assert_eq!(source.source_format, AUGGIE_SESSION_JSON_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_auggie_history(
        &fixture,
        &mut store,
        AuggieImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T20:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);

    let session_id = provider_session_uuid(CaptureProvider::Auggie, "01K0AUGGIESESSION0000000000");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("auggie session json oracle prompt"));
    assert!(rendered.contains("Auggie session import finished"));
    assert!(rendered.contains("auggie node text oracle prompt"));
    assert!(store
        .search_event_hits("Auggie node response imported", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Auggie)));

    let source = store
        .capture_source_by_external_session(CaptureProvider::Auggie, "01K0AUGGIESESSION0000000000")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_metadata"]["upstream_schema_anchor"]["package"].as_str(),
        Some("@augmentcode/auggie@0.32.0")
    );

    let second = import_auggie_history(
        &fixture,
        &mut store,
        AuggieImportOptions {
            allow_partial_failures: true,
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 4);
}

#[test]
pub(crate) fn provider_sources_discovers_auggie_default_sessions() {
    let temp = tempdir();
    let fixture = provider_history_fixture("auggie/v0.32.0/sessions");
    let sessions = temp.path().join(".augment").join("sessions");
    copy_dir_all(&fixture, &sessions);

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Auggie);
    let source = sources
        .iter()
        .find(|source| source.source_format == AUGGIE_SESSION_JSON_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Auggie source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Auggie);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, sessions);
}

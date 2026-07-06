#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn archive_import_allows_multiple_capture_sources_for_same_provider_session() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let external_session_id = "provider-session-1";
    let first_source = provider_archive_source(
        "018f45d0-0000-7000-8000-000000080001",
        external_session_id,
        "/tmp/provider/first.jsonl",
    );
    let second_source = provider_archive_source(
        "018f45d0-0000-7000-8000-000000080002",
        external_session_id,
        "/tmp/provider/second.jsonl",
    );

    store
        .import_archive(&archive_with_source(first_source.clone()), false)
        .unwrap();
    store
        .import_archive(&archive_with_source(second_source.clone()), false)
        .unwrap();

    let sources = store.list_capture_sources().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(
        sources
            .iter()
            .map(|source| source.id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([first_source.id, second_source.id])
    );
    assert!(sources.iter().all(
        |source| source.descriptor.external_session_id.as_deref() == Some(external_session_id)
    ));
}

pub(crate) fn archive_with_source(source: CaptureSource) -> SessionHistoryArchive {
    SessionHistoryArchive {
        capture_sources: vec![source],
        ..SessionHistoryArchive::default()
    }
}

pub(crate) fn provider_archive_source(
    id: &str,
    external_session_id: &str,
    raw_source_path: &str,
) -> CaptureSource {
    CaptureSource {
        id: Uuid::parse_str(id).unwrap(),
        descriptor: CaptureSourceDescriptor {
            kind: ctx_history_core::CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Claude,
            machine_id: "test-machine".to_owned(),
            process_id: None,
            cwd: Some("/repo".to_owned()),
            raw_source_path: Some(raw_source_path.to_owned()),
            external_session_id: Some(external_session_id.to_owned()),
        },
        started_at: fixed_time(),
        ended_at: None,
        sync: sync_metadata(),
    }
}

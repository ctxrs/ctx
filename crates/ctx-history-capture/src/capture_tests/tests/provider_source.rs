#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn provider_import_scopes_provenance_by_source_format_and_path() {
    let temp = tempdir();
    let shared_path = temp
        .path()
        .join("shared-source.jsonl")
        .display()
        .to_string();
    assert_provider_source_collision_is_distinct(
        "provider_format_a",
        &shared_path,
        "provider_format_b",
        &shared_path,
    );

    let first_path = temp.path().join("first-source.jsonl").display().to_string();
    let second_path = temp
        .path()
        .join("second-source.jsonl")
        .display()
        .to_string();
    assert_provider_source_collision_is_distinct(
        "provider_format",
        &first_path,
        "provider_format",
        &second_path,
    );
}

#[test]
pub(crate) fn provider_source_event_seq_keeps_large_provider_indices_distinct() {
    let source_id = Uuid::parse_str("018fe2e4-2266-7000-8000-000000000001").unwrap();

    assert_ne!(
        provider_source_event_seq(source_id, 0),
        provider_source_event_seq(source_id, 1_048_576)
    );
    assert_eq!(
        provider_source_event_seq(source_id, 1_048_576) & 0xffff_ffff,
        1_048_576
    );
}

pub(crate) fn assert_provider_source_collision_is_distinct(
    first_source_format: &str,
    first_source_path: &str,
    second_source_format: &str,
    second_source_path: &str,
) {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let provider_session_id = "shared-provider-session";
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let first_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        first_source_format,
        Some(first_source_path),
    );
    let second_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        second_source_format,
        Some(second_source_path),
    );
    assert_ne!(first_source_id, second_source_id);

    let normalization = ProviderNormalizationResult {
        summary: ProviderImportSummary::default(),
        captures: vec![
            (
                1,
                provider_collision_capture(
                    provider,
                    provider_session_id,
                    first_source_format,
                    first_source_path,
                    occurred_at,
                ),
            ),
            (
                2,
                provider_collision_capture(
                    provider,
                    provider_session_id,
                    second_source_format,
                    second_source_path,
                    occurred_at,
                ),
            ),
        ],
        files_touched: vec![
            (
                1,
                provider_collision_file_touch(
                    provider,
                    provider_session_id,
                    first_source_format,
                    first_source_path,
                    occurred_at,
                ),
            ),
            (
                2,
                provider_collision_file_touch(
                    provider,
                    provider_session_id,
                    second_source_format,
                    second_source_path,
                    occurred_at,
                ),
            ),
        ],
    };

    let summary = import_normalized_provider_captures(
        &mut store,
        normalization,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(store.capture_source_count().unwrap(), 2);

    let first_source = store.get_capture_source(first_source_id).unwrap();
    let second_source = store.get_capture_source(second_source_id).unwrap();
    assert_eq!(
        first_source.descriptor.raw_source_path.as_deref(),
        Some(first_source_path)
    );
    assert_eq!(
        first_source.sync.metadata["source_format"].as_str(),
        Some(first_source_format)
    );
    assert_eq!(
        second_source.descriptor.raw_source_path.as_deref(),
        Some(second_source_path)
    );
    assert_eq!(
        second_source.sync.metadata["source_format"].as_str(),
        Some(second_source_format)
    );

    let session_id = provider_session_uuid(provider, provider_session_id);
    let event_source_ids = store
        .events_for_session(session_id)
        .unwrap()
        .into_iter()
        .map(|event| event.capture_source_id.unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        event_source_ids,
        BTreeSet::from([first_source_id, second_source_id])
    );

    let archive = store.export_archive().unwrap();
    assert_eq!(archive.files_touched.len(), 2);
    let touched_source_ids = archive
        .files_touched
        .iter()
        .map(|file| file.source_id.unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        touched_source_ids,
        BTreeSet::from([first_source_id, second_source_id])
    );
    for file in archive.files_touched {
        let source_id = file.source_id.unwrap();
        assert_eq!(
            file.event_id,
            Some(provider_source_event_uuid(source_id, 0))
        );
    }
}

use super::*;

#[test]
fn provider_native_batch_hydration_preserves_requested_order_and_full_bodies() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("opencode-batch.sqlite");
    let first = long_body("first source-backed batch row");
    let second = long_body("second source-backed batch row");
    create_opencode_session_message_database(&path, &[&first, &second]);

    let registration = opencode::opencode_source_backed_registration();
    let documents = collect_opencode_documents(registration, &path);
    assert_eq!(documents.len(), 2);
    let requests = vec![event_request(&documents[1]), event_request(&documents[0])];
    let batch = BatchHydrationRequest::new(requests.clone()).unwrap();
    let hydrated = registration
        .exact_resolver(crate::test_provider_sqlite_data_root(), &path)
        .hydrate_batch(&batch)
        .unwrap();

    assert_eq!(
        hydrated
            .records()
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        requests
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(hydrated.records()[0].provider_bytes, second.as_bytes());
    assert_eq!(hydrated.records()[1].provider_bytes, first.as_bytes());
}

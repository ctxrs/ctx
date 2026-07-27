use super::*;

#[test]
fn source_generation_retires_omissions_replays_and_allows_exact_restoration() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let source_id = Uuid::from_u128(800);
    let session_id = Uuid::from_u128(801);
    let retained_event_id = Uuid::from_u128(802);
    let retired_event_id = Uuid::from_u128(803);
    let mut capture_source = source(source_id);
    let observation = ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: "codex-jsonl".to_owned(),
        machine_id: "machine".to_owned(),
        locator_identity: "locator-800".to_owned(),
        cursor_stream: "native-path:source-800".to_owned(),
        proposed_source_identity: format!("source-{source_id}"),
        raw_source_path: Some("/repo/session.jsonl".to_owned()),
        source_revision: "revision-1".to_owned(),
        observed_at_ms: 1,
    };
    let mut retained_event = event(retained_event_id, 1, Some(source_id), Some(session_id));
    let retained_hash = compute_payload_hash(&retained_event.payload).unwrap();
    retained_event.dedupe_key = Some(Store::provider_source_event_dedupe_key(
        source_id,
        1,
        &retained_hash,
    ));
    retained_event.sync.metadata["provider_event_hash_authority"] =
        json!(ProviderEventHashAuthority::NormalizedPayloadFallback.as_str());
    let mut retired_event = event(retired_event_id, 2, Some(source_id), Some(session_id));
    let retired_hash = compute_payload_hash(&retired_event.payload).unwrap();
    retired_event.dedupe_key = Some(Store::provider_source_event_dedupe_key(
        source_id,
        2,
        &retired_hash,
    ));
    retired_event.sync.metadata["provider_event_hash_authority"] =
        json!(ProviderEventHashAuthority::NormalizedPayloadFallback.as_str());
    let key = NativePathSourceGenerationKey {
        provider: CaptureProvider::Codex,
        source_format: observation.source_format.clone(),
        machine_id: observation.machine_id.clone(),
        canonical_source_identity: observation.proposed_source_identity.clone(),
        locator_identity: observation.locator_identity.clone(),
        cursor_stream: observation.cursor_stream.clone(),
        source_revision: observation.source_revision.clone(),
        generation_id: "generation-1".to_owned(),
    };

    let mut group = begin_group(&store, &guard);
    let resolution = group
        .reconcile_provider_source_locator(&observation)
        .unwrap();
    capture_source.descriptor.source_identity = Some(resolution.canonical_source_identity.clone());
    group.upsert_capture_source(&capture_source).unwrap();
    group
        .bind_capture_source_provider_route(source_id, &resolution.route_binding())
        .unwrap();
    group
        .upsert_session(&session(session_id, Some(source_id)))
        .unwrap();
    group
        .reconcile_provider_event(
            &retained_event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap();
    group
        .reconcile_provider_event(
            &retired_event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap();
    group
        .stage_source_generation_page(
            &key,
            &NativePathRetainedSourceEntities {
                capture_source_ids: vec![source_id],
                session_ids: vec![session_id],
                event_ids: vec![retained_event_id],
                ..NativePathRetainedSourceEntities::default()
            },
        )
        .unwrap();
    publish_and_commit(group).unwrap();

    let retirement_cursor_key =
        NativePathCursorKey::new(None, "test-machine", "native-path:retirement-preview");
    let retirement_transition =
        NativePathCursorTransition::new(None, cursor(&retirement_cursor_key, "done", 2));
    let mut retirement = begin_unclassified_group(&store, &guard, accounting());
    let preview = retirement
        .preview_source_generation_retirement_page(&key, None, 16)
        .unwrap();
    assert_eq!(
        retirement
            .classify_cursor_set(
                "source-retirement-preview",
                std::slice::from_ref(&retirement_transition),
            )
            .unwrap(),
        NativePathCursorSetClassification::AllExpected
    );
    let page = retirement
        .retire_source_generation_page(&key, None, 16, 2)
        .unwrap();
    assert_eq!(page, preview);
    assert!(page.done);
    assert_eq!(page.retired, 1);
    publish_and_commit(retirement).unwrap();

    assert!(store
        .get_event(retained_event_id)
        .unwrap()
        .sync
        .deleted_at
        .is_none());
    assert!(store
        .get_event(retired_event_id)
        .unwrap()
        .sync
        .deleted_at
        .is_some());

    let mut replay = begin_group(&store, &guard);
    assert_eq!(
        replay
            .retire_source_generation_page(&key, None, 16, 2)
            .unwrap(),
        page
    );
    publish_and_commit(replay).unwrap();

    let mut restore = begin_group(&store, &guard);
    assert!(!restore
        .reconcile_provider_event(
            &retired_event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap());
    publish_and_commit(restore).unwrap();
    assert!(store
        .get_event(retired_event_id)
        .unwrap()
        .sync
        .deleted_at
        .is_none());
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

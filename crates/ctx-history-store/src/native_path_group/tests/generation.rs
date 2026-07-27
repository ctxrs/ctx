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

#[test]
fn source_generation_retirement_uses_owner_keyset_without_temp_sort() {
    let (_temp, store) = open_store();
    let mut statement = store
        .conn
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT entity.id,
                    EXISTS(
                        SELECT 1
                        FROM native_path_source_generation_entities kept
                        WHERE kept.provider = ?1
                          AND kept.source_format = ?2
                          AND kept.machine_id = ?3
                          AND kept.locator_identity = ?4
                          AND kept.generation_id = ?5
                          AND kept.entity_kind = ?6
                          AND kept.entity_id = entity.id
                    )
             FROM events entity INDEXED BY idx_events_source_generation_retirement
             WHERE entity.capture_source_id = ?7
               AND entity.deleted_at_ms IS NULL
               AND entity.id > ?8
             ORDER BY entity.id
             LIMIT ?9",
        )
        .unwrap();
    let plan = statement
        .query_map(
            params![
                "codex",
                "codex-jsonl",
                "machine",
                "locator",
                "generation",
                "event",
                Uuid::nil().to_string(),
                "",
                65,
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");

    assert!(
        plan.contains("idx_events_source_generation_retirement"),
        "{plan}"
    );
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");

    let mut statement = store
        .conn
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT staged.entity_id
             FROM native_path_source_generation_entities staged
             WHERE staged.provider = ?1
               AND staged.source_format = ?2
               AND staged.machine_id = ?3
               AND staged.locator_identity = ?4
               AND staged.generation_id = ?5
               AND staged.entity_kind = 'capture_source'
               AND staged.entity_id > ?6
               AND EXISTS(
                   SELECT 1
                   FROM events entity
                   WHERE entity.capture_source_id = staged.entity_id
                     AND entity.deleted_at_ms IS NULL
               )
             ORDER BY staged.entity_id
             LIMIT 1",
        )
        .unwrap();
    let owner_plan = statement
        .query_map(
            params![
                "codex",
                "codex-jsonl",
                "machine",
                "locator",
                "generation",
                "",
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert!(
        owner_plan.contains("sqlite_autoindex_native_path_source_generation_entities_1"),
        "{owner_plan}"
    );
    assert!(
        owner_plan.contains("idx_events_source_generation_retirement"),
        "{owner_plan}"
    );
    assert!(!owner_plan.contains("TEMP B-TREE"), "{owner_plan}");
}

#[test]
fn source_generation_retirement_pages_across_multiple_owners_without_gaps() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let observation = ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: "codex-jsonl".to_owned(),
        machine_id: "machine".to_owned(),
        locator_identity: "locator-multi-owner".to_owned(),
        cursor_stream: "native-path:multi-owner".to_owned(),
        proposed_source_identity: "source-multi-owner".to_owned(),
        raw_source_path: Some("/repo/sessions".to_owned()),
        source_revision: "revision-1".to_owned(),
        observed_at_ms: 1,
    };
    let key = NativePathSourceGenerationKey {
        provider: CaptureProvider::Codex,
        source_format: observation.source_format.clone(),
        machine_id: observation.machine_id.clone(),
        canonical_source_identity: observation.proposed_source_identity.clone(),
        locator_identity: observation.locator_identity.clone(),
        cursor_stream: observation.cursor_stream.clone(),
        source_revision: observation.source_revision.clone(),
        generation_id: "generation-multi-owner".to_owned(),
    };
    let source_ids = [
        Uuid::from_u128(900),
        Uuid::from_u128(901),
        Uuid::from_u128(902),
    ];
    let event_ids = [
        Uuid::from_u128(1_000),
        Uuid::from_u128(1_006),
        Uuid::from_u128(1_001),
        Uuid::from_u128(1_007),
        Uuid::from_u128(1_002),
        Uuid::from_u128(1_008),
    ];
    let retained_event_ids = vec![event_ids[0], event_ids[3], event_ids[4]];

    let mut publication = begin_group(&store, &guard);
    let resolution = publication
        .reconcile_provider_source_locator(&observation)
        .unwrap();
    for source_id in source_ids {
        let mut capture_source = source(source_id);
        capture_source.descriptor.source_identity =
            Some(resolution.canonical_source_identity.clone());
        publication.upsert_capture_source(&capture_source).unwrap();
        publication
            .bind_capture_source_provider_route(source_id, &resolution.route_binding())
            .unwrap();
    }
    for (index, event_id) in event_ids.iter().copied().enumerate() {
        let source_id = source_ids[index / 2];
        let mut value = event(
            event_id,
            u64::try_from(index + 1).unwrap(),
            Some(source_id),
            None,
        );
        let payload_hash = compute_payload_hash(&value.payload).unwrap();
        value.dedupe_key = Some(Store::provider_source_event_dedupe_key(
            source_id,
            value.seq,
            &payload_hash,
        ));
        value.sync.metadata["provider_event_hash_authority"] =
            json!(ProviderEventHashAuthority::NormalizedPayloadFallback.as_str());
        publication
            .reconcile_provider_event(
                &value,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
            )
            .unwrap();
    }
    publication
        .stage_source_generation_page(
            &key,
            &NativePathRetainedSourceEntities {
                capture_source_ids: source_ids.to_vec(),
                event_ids: retained_event_ids.clone(),
                ..NativePathRetainedSourceEntities::default()
            },
        )
        .unwrap();
    publish_and_commit(publication).unwrap();

    let mut after = None;
    let mut inspected = 0_usize;
    let mut retired = 0_usize;
    loop {
        let mut retirement = begin_group(&store, &guard);
        let page = retirement
            .retire_source_generation_page(&key, after.as_ref(), 2, 2)
            .unwrap();
        inspected = inspected.saturating_add(page.inspected);
        retired = retired.saturating_add(page.retired);
        after = page.next_after.clone();
        let done = page.done;
        publish_and_commit(retirement).unwrap();
        if done {
            break;
        }
    }

    assert_eq!(inspected, event_ids.len());
    assert_eq!(retired, event_ids.len() - retained_event_ids.len());
    for event_id in event_ids {
        let deleted = store.get_event(event_id).unwrap().sync.deleted_at.is_some();
        assert_eq!(deleted, !retained_event_ids.contains(&event_id));
    }
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

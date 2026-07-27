use super::*;

#[test]
fn store_owned_cursor_classification_publishes_and_recovers_exact_checkpoint() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let key = NativePathCursorKey::new(None, "machine", "native-path:checkpoint");
    let transition = NativePathCursorTransition::new(None, cursor(&key, "provider-next", 1));

    let mut group = begin_unclassified_group(&store, &guard, accounting());
    assert_eq!(
        group
            .classify_cursor_set("publication-1", std::slice::from_ref(&transition))
            .unwrap(),
        NativePathCursorSetClassification::AllExpected
    );
    group
        .reconcile_provider_event(
            &event(Uuid::from_u128(500), 1, None, None),
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap();
    let checkpoint = group
        .prepare_journal_checkpoint()
        .unwrap()
        .expect("active journal checkpoint");
    group.publish_cursor_set().unwrap();
    let receipt = group.commit().unwrap();
    assert_eq!(receipt.checkpoint(), Some(&checkpoint));

    let stored = store
        .get_sync_cursor(key.team_id(), key.device_id(), key.stream())
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.published_cursors(),
        std::slice::from_ref(&stored),
        "the receipt must return the exact atomically committed Store envelope"
    );
    let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
    assert_eq!(committed.publication_id(), "publication-1");
    assert_eq!(committed.provider_cursor(), "provider-next");
    assert_eq!(committed.journal_checkpoint(), Some(&checkpoint));

    let regenerated = NativePathCursorTransition::new(None, cursor(&key, "provider-next", 99));
    assert_ne!(
        regenerated.next().timestamps.updated_at,
        stored.timestamps.updated_at
    );
    let mut retry = begin_unclassified_group(&store, &guard, accounting());
    assert_eq!(
        retry
            .classify_cursor_set("publication-1", &[regenerated])
            .unwrap(),
        NativePathCursorSetClassification::AllNextSameGroup {
            checkpoint: Some(checkpoint.clone())
        }
    );
    let retry_receipt = retry.commit().unwrap();
    assert_eq!(retry_receipt.checkpoint(), Some(&checkpoint));
    assert_eq!(retry_receipt.attempted_mutation_units(), 0);
    assert_eq!(
        retry_receipt.published_cursors(),
        std::slice::from_ref(&stored),
        "all-next recovery must return the same exact committed envelope"
    );
    assert_eq!(
        store
            .get_sync_cursor(key.team_id(), key.device_id(), key.stream())
            .unwrap(),
        Some(stored),
        "all-next recovery must not rewrite cursor timestamps or identity"
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn stale_exact_envelope_conflicts_after_same_provider_cursor_republication() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let key = NativePathCursorKey::new(None, "machine", "native-path:split-race");
    let provider_split = cursor(&key, "provider-split", 1);

    let mut first = begin_unclassified_group(&store, &guard, accounting());
    let first_transition = NativePathCursorTransition::new(None, provider_split.clone());
    assert_eq!(
        first
            .classify_cursor_set("split-left", std::slice::from_ref(&first_transition))
            .unwrap(),
        NativePathCursorSetClassification::AllExpected
    );
    first.prepare_journal_checkpoint().unwrap();
    first.publish_cursor_set().unwrap();
    let first_receipt = first.commit().unwrap();
    let first_exact = first_receipt.published_cursors()[0].clone();

    let mut competitor = begin_unclassified_group(&store, &guard, accounting());
    let competitor_transition =
        NativePathCursorTransition::new(Some(first_exact.cursor.clone()), provider_split);
    assert_eq!(
        competitor
            .classify_cursor_set(
                "competing-publication",
                std::slice::from_ref(&competitor_transition),
            )
            .unwrap(),
        NativePathCursorSetClassification::AllExpected
    );
    competitor.prepare_journal_checkpoint().unwrap();
    competitor.publish_cursor_set().unwrap();
    competitor.commit().unwrap();

    let mut right = begin_unclassified_group(&store, &guard, accounting());
    let intended_right = NativePathCursorTransition::new(
        Some(first_exact.cursor),
        cursor(&key, "provider-right", 2),
    );
    assert!(matches!(
        right.classify_cursor_set("split-right", &[intended_right]),
        Err(StoreError::NativePathCursorConflict)
    ));
    assert!(matches!(
        right.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn published_receipt_rereads_the_exact_canonical_update_row() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let key = NativePathCursorKey::new(None, "machine", "native-path:canonical-receipt");

    let mut initial = begin_unclassified_group(&store, &guard, accounting());
    let initial_transition =
        NativePathCursorTransition::new(None, cursor(&key, "provider-initial", 1));
    assert_eq!(
        initial
            .classify_cursor_set(
                "canonical-initial",
                std::slice::from_ref(&initial_transition),
            )
            .unwrap(),
        NativePathCursorSetClassification::AllExpected
    );
    initial.prepare_journal_checkpoint().unwrap();
    initial.publish_cursor_set().unwrap();
    let initial_receipt = initial.commit().unwrap();
    let current = initial_receipt.published_cursors()[0].clone();

    let mut proposed = cursor(&key, "provider-updated", 2);
    proposed.timestamps.created_at = now() + TimeDelta::microseconds(123);
    proposed.timestamps.updated_at += TimeDelta::microseconds(456);
    let proposed_id = proposed.id;
    let proposed_created_at = proposed.timestamps.created_at;
    let mut update = begin_unclassified_group(&store, &guard, accounting());
    let update_transition = NativePathCursorTransition::new(Some(current.cursor.clone()), proposed);
    assert_eq!(
        update
            .classify_cursor_set("canonical-update", std::slice::from_ref(&update_transition),)
            .unwrap(),
        NativePathCursorSetClassification::AllExpected
    );
    update.prepare_journal_checkpoint().unwrap();
    update.publish_cursor_set().unwrap();
    let update_receipt = update.commit().unwrap();
    let committed = store
        .get_sync_cursor(key.team_id(), key.device_id(), key.stream())
        .unwrap()
        .unwrap();

    assert_eq!(
        update_receipt.published_cursors(),
        std::slice::from_ref(&committed)
    );
    assert_eq!(committed.id, current.id);
    assert_ne!(committed.id, proposed_id);
    assert_eq!(
        committed.timestamps.created_at,
        current.timestamps.created_at
    );
    assert_ne!(committed.timestamps.created_at, proposed_created_at);
    assert_eq!(
        committed.timestamps.updated_at.timestamp_subsec_millis(),
        0,
        "SQLite canonicalizes cursor timestamps to milliseconds"
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn mixed_cursor_states_and_checkpoint_mismatch_conflict_without_callbacks() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let first_key = NativePathCursorKey::new(None, "machine", "native-path:first");
    let second_key = NativePathCursorKey::new(None, "machine", "native-path:second");
    let transitions = vec![
        NativePathCursorTransition::new(None, cursor(&first_key, "first-next", 1)),
        NativePathCursorTransition::new(None, cursor(&second_key, "second-next", 1)),
    ];

    let two_sources = NativePathGroupAccounting::new(1, 2, 64).unwrap();
    let mut publish = begin_unclassified_group(&store, &guard, two_sources);
    publish
        .classify_cursor_set("publication-set", &transitions)
        .unwrap();
    publish.prepare_journal_checkpoint().unwrap();
    publish.publish_cursor_set().unwrap();
    publish.commit().unwrap();

    let mut subset_retry = begin_unclassified_group(&store, &guard, two_sources);
    assert!(matches!(
        subset_retry.classify_cursor_set("publication-set", std::slice::from_ref(&transitions[0])),
        Err(StoreError::InvalidNativePathCursorSet)
    ));
    assert!(matches!(
        subset_retry.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));

    let mut second = store
        .get_sync_cursor(
            second_key.team_id(),
            second_key.device_id(),
            second_key.stream(),
        )
        .unwrap()
        .unwrap();
    second.cursor = "expected-second".to_owned();
    second.timestamps.updated_at += TimeDelta::seconds(1);
    store.upsert_sync_cursor(&second).unwrap();

    let mixed_transitions = vec![
        NativePathCursorTransition::new(
            Some("expected-first".to_owned()),
            cursor(&first_key, "first-next", 10),
        ),
        NativePathCursorTransition::new(
            Some("expected-second".to_owned()),
            cursor(&second_key, "second-next", 10),
        ),
    ];
    let mut mixed = begin_unclassified_group(&store, &guard, two_sources);
    assert!(matches!(
        mixed.classify_cursor_set("publication-set", &mixed_transitions),
        Err(StoreError::NativePathCursorConflict)
    ));
    assert!(matches!(
        mixed.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));

    // Republish both, then give only one row a different, independently valid
    // checkpoint. Exact common-checkpoint verification must reject the set.
    store.conn.execute("DELETE FROM sync_cursors", []).unwrap();
    let mut republish = begin_unclassified_group(&store, &guard, two_sources);
    republish
        .classify_cursor_set("publication-set", &transitions)
        .unwrap();
    let old_checkpoint = republish.prepare_journal_checkpoint().unwrap().unwrap();
    republish.publish_cursor_set().unwrap();
    republish.commit().unwrap();

    let mut advance = begin_group(&store, &guard);
    advance
        .reconcile_provider_event(
            &event(Uuid::from_u128(501), 2, None, None),
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap();
    let new_checkpoint = publish_and_commit(advance)
        .unwrap()
        .checkpoint()
        .cloned()
        .unwrap();
    assert_ne!(old_checkpoint, new_checkpoint);

    let mut second = store
        .get_sync_cursor(
            second_key.team_id(),
            second_key.device_id(),
            second_key.stream(),
        )
        .unwrap()
        .unwrap();
    let mut envelope = decode_cursor_envelope(&second.cursor).unwrap();
    envelope.journal_checkpoint = Some(new_checkpoint);
    second.cursor = encode_cursor_envelope(&envelope).unwrap();
    second.timestamps.updated_at += TimeDelta::seconds(1);
    store.upsert_sync_cursor(&second).unwrap();

    let retry_transitions = vec![
        NativePathCursorTransition::new(None, cursor(&first_key, "first-next", 20)),
        NativePathCursorTransition::new(None, cursor(&second_key, "second-next", 20)),
    ];
    let mut mismatch = begin_unclassified_group(&store, &guard, two_sources);
    assert!(matches!(
        mismatch.classify_cursor_set("publication-set", &retry_transitions),
        Err(StoreError::NativePathCursorConflict)
    ));
    assert!(matches!(
        mismatch.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

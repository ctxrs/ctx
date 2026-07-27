use super::*;
use ctx_history_core::{
    CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event, EventRole, EventType,
    SyncMetadata,
};
use serde_json::json;

fn observation(
    path: &Path,
    locator: &str,
    cursor: &str,
    revision: &str,
) -> ProviderSourceLocatorObservation {
    ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: "codex_session_jsonl".to_owned(),
        machine_id: "machine-1".to_owned(),
        locator_identity: locator.to_owned(),
        cursor_stream: cursor.to_owned(),
        proposed_source_identity: format!("identity-{locator}"),
        raw_source_path: Some(path.to_string_lossy().into_owned()),
        source_revision: revision.to_owned(),
        observed_at_ms: 1,
    }
}

fn capture_source(id: Uuid, path: &Path, canonical_source_identity: &str) -> CaptureSource {
    CaptureSource {
        id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "machine-1".to_owned(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(path.to_string_lossy().into_owned()),
            source_format: Some("codex_session_jsonl".to_owned()),
            source_root: None,
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(format!("session-{id}")),
        },
        started_at: "2026-07-22T00:00:00Z".parse().unwrap(),
        ended_at: None,
        sync: SyncMetadata::default(),
    }
}

fn event(id: Uuid, source_id: Uuid, seq: u64) -> Event {
    Event {
        id,
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::ToolOutput,
        role: Some(EventRole::Tool),
        occurred_at: "2026-07-22T00:00:00Z".parse().unwrap(),
        capture_source_id: Some(source_id),
        payload: json!({"body": "bounded result"}),
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata::default(),
    }
}

fn insert_source_event(
    store: &Store,
    source_id: Uuid,
    event_id: Uuid,
    path: &Path,
    canonical_source_identity: &str,
    seq: u64,
) {
    store
        .upsert_capture_source(&capture_source(source_id, path, canonical_source_identity))
        .unwrap();
    store
        .upsert_event(&event(event_id, source_id, seq))
        .unwrap();
}

#[test]
fn changed_source_at_a_new_locator_never_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let old_path = temp.path().join("old.jsonl");
    let new_path = temp.path().join("new.jsonl");
    std::fs::write(&old_path, b"old source").unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let first = store
        .reconcile_provider_source_locator(&observation(
            &old_path,
            "old-locator",
            "old-cursor",
            "revision-1",
        ))
        .unwrap();
    std::fs::remove_file(&old_path).unwrap();
    std::fs::write(&new_path, b"rewritten source").unwrap();
    let rewritten = store
        .reconcile_provider_source_locator(&observation(
            &new_path,
            "new-locator",
            "new-cursor",
            "revision-2",
        ))
        .unwrap();

    assert!(!rewritten.relocated);
    assert_ne!(
        rewritten.canonical_source_identity,
        first.canonical_source_identity
    );
}

fn assert_shared_canonical_source_allows_multiple_current_physical_sources() {
    let temp = tempfile::tempdir().unwrap();
    let first_path = temp.path().join("first.jsonl");
    let second_path = temp.path().join("second.jsonl");
    let moved_path = temp.path().join("moved-first.jsonl");
    std::fs::write(&first_path, b"first source").unwrap();
    std::fs::write(&second_path, b"second source").unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let mut first = observation(&first_path, "first", "cursor-first", "revision-first");
    first.proposed_source_identity = "shared-root-identity".to_owned();
    let mut second = observation(&second_path, "second", "cursor-second", "revision-second");
    second.proposed_source_identity = "shared-root-identity".to_owned();

    assert!(
        !store
            .reconcile_provider_source_locator(&first)
            .unwrap()
            .relocated
    );
    assert!(
        !store
            .reconcile_provider_source_locator(&second)
            .unwrap()
            .relocated
    );

    std::fs::rename(&first_path, &moved_path).unwrap();
    let mut moved = observation(
        &moved_path,
        "moved-first",
        "cursor-moved-first",
        "revision-first",
    );
    moved.proposed_source_identity = "new-root-identity".to_owned();
    let resolution = store.reconcile_provider_source_locator(&moved).unwrap();
    assert!(resolution.relocated);
    assert_eq!(resolution.canonical_source_identity, "shared-root-identity");

    let second_replay = store.reconcile_provider_source_locator(&second).unwrap();
    assert!(!second_replay.relocated);
    assert_eq!(
        second_replay.canonical_source_identity,
        "shared-root-identity"
    );
}

#[test]
fn unique_missing_locator_reconciles_and_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("ctx.db");
    let old_path = temp.path().join("old.jsonl");
    let new_path = temp.path().join("new.jsonl");
    std::fs::write(&old_path, b"same provider source").unwrap();
    let store = Store::open(&database).unwrap();
    let first = store
        .reconcile_provider_source_locator(&observation(
            &old_path,
            "old-locator",
            "old-cursor",
            "revision-1",
        ))
        .unwrap();
    assert!(!first.relocated);
    std::fs::rename(&old_path, &new_path).unwrap();
    let moved = store
        .reconcile_provider_source_locator(&observation(
            &new_path,
            "new-locator",
            "new-cursor",
            "revision-1",
        ))
        .unwrap();
    assert!(moved.relocated);
    assert_eq!(moved.canonical_source_identity, "identity-old-locator");
    drop(store);

    let reopened = Store::open(&database).unwrap();
    let appended = reopened
        .reconcile_provider_source_locator(&observation(
            &new_path,
            "new-locator",
            "new-cursor",
            "revision-2",
        ))
        .unwrap();
    assert!(appended.relocated);
    assert_eq!(appended.canonical_source_identity, "identity-old-locator");

    std::fs::rename(&new_path, &old_path).unwrap();
    let moved_back = reopened
        .reconcile_provider_source_locator(&observation(
            &old_path,
            "old-locator",
            "old-cursor",
            "revision-2",
        ))
        .unwrap();
    assert!(moved_back.relocated);
    assert_eq!(moved_back.canonical_source_identity, "identity-old-locator");
}

#[test]
fn identical_live_sources_never_alias_and_known_alias_collision_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let first_path = temp.path().join("first.jsonl");
    let second_path = temp.path().join("second.jsonl");
    let third_path = temp.path().join("third.jsonl");
    std::fs::write(&first_path, b"same").unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    store
        .reconcile_provider_source_locator(&observation(
            &first_path,
            "first",
            "cursor-first",
            "revision-same",
        ))
        .unwrap();
    std::fs::rename(&first_path, &second_path).unwrap();
    let moved = store
        .reconcile_provider_source_locator(&observation(
            &second_path,
            "second",
            "cursor-second",
            "revision-same",
        ))
        .unwrap();
    assert!(moved.relocated);

    std::fs::write(&first_path, b"same").unwrap();
    let error = store
        .reconcile_provider_source_locator(&observation(
            &first_path,
            "first",
            "cursor-first",
            "revision-same",
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::ProviderSourceRelocationAmbiguous { .. }
    ));

    std::fs::write(&third_path, b"same").unwrap();
    let third = store
        .reconcile_provider_source_locator(&observation(
            &third_path,
            "third",
            "cursor-third",
            "revision-same",
        ))
        .unwrap();
    assert!(!third.relocated, "multiple live sources must stay distinct");
    assert_shared_canonical_source_allows_multiple_current_physical_sources();
}

#[test]
fn debug_output_never_contains_the_local_path() {
    let observation = observation(
        Path::new("/private/home/alice/provider/session.jsonl"),
        "private-locator",
        "private-cursor",
        "private-revision",
    );
    let debug = format!("{observation:?}");
    assert!(!debug.contains("/private/home/alice"));
    assert!(debug.contains("<local-path>"));
}

#[test]
fn exact_alias_binding_disambiguates_shared_identity_and_follows_relocation() {
    let temp = tempfile::tempdir().unwrap();
    let first_path = temp.path().join("first.jsonl");
    let second_path = temp.path().join("second.jsonl");
    let moved_path = temp.path().join("moved-first.jsonl");
    std::fs::write(&first_path, b"first source").unwrap();
    std::fs::write(&second_path, b"second source").unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let mut first = observation(&first_path, "first", "first-cursor", "first-revision");
    first.proposed_source_identity = "shared-canonical-identity".to_owned();
    let mut second = observation(&second_path, "second", "second-cursor", "second-revision");
    second.proposed_source_identity = "shared-canonical-identity".to_owned();
    let first_resolution = store.reconcile_provider_source_locator(&first).unwrap();
    let second_resolution = store.reconcile_provider_source_locator(&second).unwrap();

    let first_source_id = Uuid::new_v4();
    let first_event_id = Uuid::new_v4();
    let second_source_id = Uuid::new_v4();
    let second_event_id = Uuid::new_v4();
    insert_source_event(
        &store,
        first_source_id,
        first_event_id,
        &first_path,
        "shared-canonical-identity",
        1,
    );
    insert_source_event(
        &store,
        second_source_id,
        second_event_id,
        &second_path,
        "shared-canonical-identity",
        2,
    );
    store
        .bind_capture_source_provider_route(first_source_id, &first_resolution.route_binding())
        .unwrap();
    store
        .bind_capture_source_provider_route(second_source_id, &second_resolution.route_binding())
        .unwrap();

    assert_eq!(
        store
            .authorized_source_route_for_event(first_event_id)
            .unwrap()
            .path(),
        first_path
    );
    assert_eq!(
        store
            .authorized_source_route_for_event(second_event_id)
            .unwrap()
            .path(),
        second_path
    );

    std::fs::rename(&first_path, &moved_path).unwrap();
    let mut moved = observation(
        &moved_path,
        "moved-first",
        "moved-first-cursor",
        "first-revision",
    );
    moved.proposed_source_identity = "irrelevant-new-identity".to_owned();
    assert!(
        store
            .reconcile_provider_source_locator(&moved)
            .unwrap()
            .relocated
    );
    assert_eq!(
        store
            .authorized_source_route_for_event(first_event_id)
            .unwrap()
            .path(),
        moved_path
    );
    assert_eq!(
        store
            .authorized_source_route_for_event(second_event_id)
            .unwrap()
            .path(),
        second_path
    );
}

#[test]
fn missing_conflicting_and_ambiguous_routes_fail_closed_without_journaling() {
    let temp = tempfile::tempdir().unwrap();
    let first_path = temp.path().join("first.jsonl");
    let second_path = temp.path().join("second.jsonl");
    std::fs::write(&first_path, b"first source").unwrap();
    std::fs::write(&second_path, b"second source").unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let mut first = observation(&first_path, "first", "first-cursor", "first-revision");
    first.proposed_source_identity = "shared-canonical-identity".to_owned();
    let mut second = observation(&second_path, "second", "second-cursor", "second-revision");
    second.proposed_source_identity = "shared-canonical-identity".to_owned();
    let first_binding = store
        .reconcile_provider_source_locator(&first)
        .unwrap()
        .route_binding();
    let second_binding = store
        .reconcile_provider_source_locator(&second)
        .unwrap()
        .route_binding();
    let source_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    insert_source_event(
        &store,
        source_id,
        event_id,
        &first_path,
        "shared-canonical-identity",
        1,
    );
    assert!(matches!(
        store.authorized_source_route_for_event(event_id),
        Err(StoreError::AuthorizedSourceRouteUnavailable { .. })
    ));

    let journal_before = store
        .conn
        .query_row(
            "SELECT active, high_water_sequence, cumulative_digest,
                    acknowledged_sequence, acknowledged_cumulative_digest
             FROM projection_journal_state WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .unwrap();
    store
        .bind_capture_source_provider_route(source_id, &first_binding)
        .unwrap();
    store
        .bind_capture_source_provider_route(source_id, &first_binding)
        .unwrap();
    let mut renamed_machine = first.clone();
    renamed_machine.machine_id = "renamed-machine-1".to_owned();
    renamed_machine.locator_identity = "renamed-machine-locator".to_owned();
    renamed_machine.cursor_stream = "renamed-machine-cursor".to_owned();
    let renamed_binding = store
        .reconcile_provider_source_locator(&renamed_machine)
        .unwrap()
        .route_binding();
    store
        .bind_capture_source_provider_route(source_id, &renamed_binding)
        .expect("an exact path and revision survive a machine identity rename");
    assert_eq!(
        store
            .authorized_source_route_for_event(event_id)
            .unwrap()
            .path(),
        first_path
    );
    let journal_after = store
        .conn
        .query_row(
            "SELECT active, high_water_sequence, cumulative_digest,
                    acknowledged_sequence, acknowledged_cumulative_digest
             FROM projection_journal_state WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(journal_after, journal_before);
    assert!(matches!(
        store.bind_capture_source_provider_route(source_id, &second_binding),
        Err(StoreError::CaptureSourceProviderRouteConflict { .. })
    ));

    store
        .conn
        .execute(
            "UPDATE provider_source_locators SET raw_source_path = NULL
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND alias_group_identity = ?4 AND is_current = 1",
            params![
                first_binding.provider.as_str(),
                first_binding.source_format,
                first_binding.machine_id,
                first_binding.alias_group_identity,
            ],
        )
        .unwrap();
    assert!(matches!(
        store.authorized_source_route_for_event(event_id),
        Err(StoreError::AuthorizedSourceRouteUnavailable { .. })
    ));
    store
        .conn
        .execute(
            "UPDATE provider_source_locators SET raw_source_path = ?1
             WHERE provider = ?2 AND source_format = ?3 AND machine_id = ?4
               AND alias_group_identity = ?5 AND is_current = 1",
            params![
                first_path.to_string_lossy(),
                first_binding.provider.as_str(),
                first_binding.source_format,
                first_binding.machine_id,
                first_binding.alias_group_identity,
            ],
        )
        .unwrap();

    store
        .conn
        .execute("DROP INDEX idx_provider_source_locators_current", [])
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO provider_source_locators
             (provider, source_format, machine_id, locator_identity, cursor_stream,
              canonical_source_identity, alias_group_identity, raw_source_path,
              source_revision, is_current, is_relocation_alias, observed_at_ms)
             SELECT provider, source_format, machine_id, ?1, cursor_stream,
                    canonical_source_identity, alias_group_identity, ?2,
                    source_revision, 1, is_relocation_alias, observed_at_ms
             FROM provider_source_locators
             WHERE provider = ?3 AND source_format = ?4 AND machine_id = ?5
               AND alias_group_identity = ?6 AND is_current = 1",
            params![
                locator_storage_key("duplicate-current"),
                second_path.to_string_lossy(),
                first_binding.provider.as_str(),
                first_binding.source_format,
                first_binding.machine_id,
                first_binding.alias_group_identity,
            ],
        )
        .unwrap();
    assert!(matches!(
        store.authorized_source_route_for_event(event_id),
        Err(StoreError::AuthorizedSourceRouteAmbiguous { .. })
    ));
}

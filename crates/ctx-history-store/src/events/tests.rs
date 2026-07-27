use ctx_history_core::{new_id, utc_now, SyncMetadata};
use serde_json::{json, Value};
use tempfile::tempdir;

use super::*;

fn normalized_provider_event(
    id: Uuid,
    body: Value,
    hash: &str,
    authority: Option<ProviderEventHashAuthority>,
) -> Event {
    let mut sync = SyncMetadata::default();
    if let Some(authority) = authority {
        sync.metadata[PROVIDER_EVENT_HASH_AUTHORITY_KEY] = json!(authority.as_str());
    }
    Event {
        id,
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: utc_now(),
        capture_source_id: None,
        payload: json!({"body": body, "provider_event_hash": hash}),
        payload_blob_id: None,
        dedupe_key: Some(Store::provider_source_event_dedupe_key(
            Uuid::nil(),
            0,
            hash,
        )),
        sync,
    }
}

#[test]
fn reconcile_provider_event_inserts_then_replays_idempotently() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let body = json!({"text": "stable"});
    let hash = compute_payload_hash(&body).unwrap();
    let event = normalized_provider_event(
        new_id(),
        body,
        &hash,
        Some(ProviderEventHashAuthority::NormalizedPayloadFallback),
    );

    assert!(store
        .reconcile_provider_event(
            &event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap());
    assert!(!store
        .reconcile_provider_event(
            &event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap());
    assert_eq!(store.list_events().unwrap().len(), 1);
}

#[test]
fn reconcile_provider_event_migrates_legacy_fallback_hash_in_place() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let id = new_id();
    let legacy_body = json!({"text": "stable", "truncated": false});
    let legacy_hash = compute_payload_hash(&legacy_body).unwrap();
    let legacy = normalized_provider_event(id, legacy_body, &legacy_hash, None);
    assert!(store.insert_event_if_absent(&legacy).unwrap());

    let new_body = json!({"text": "stable", "text_retention": {"status": "full"}});
    let new_hash = compute_payload_hash(&new_body).unwrap();
    let replacement = normalized_provider_event(
        id,
        new_body.clone(),
        &new_hash,
        Some(ProviderEventHashAuthority::NormalizedPayloadFallback),
    );
    assert!(!store
        .reconcile_provider_event(
            &replacement,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap());

    let migrated = store.get_event(id).unwrap();
    assert_eq!(migrated.payload["body"], new_body);
    assert!(migrated.dedupe_key.as_deref().unwrap().ends_with(&new_hash));
    assert_eq!(store.list_events().unwrap().len(), 1);
}

#[test]
fn reconcile_provider_event_matching_fallback_hash_preserves_legacy_row() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let id = new_id();
    let body = json!({"text": "stable"});
    let hash = compute_payload_hash(&body).unwrap();
    let legacy = normalized_provider_event(id, body.clone(), &hash, None);
    assert!(store.insert_event_if_absent(&legacy).unwrap());
    let mut replay = normalized_provider_event(
        id,
        body,
        &hash,
        Some(ProviderEventHashAuthority::NormalizedPayloadFallback),
    );
    replay.capture_source_id = Some(new_id());
    replay.sync.metadata["new_adapter_metadata"] = json!(true);

    assert!(!store
        .reconcile_provider_event(
            &replay,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap());

    let retained = store.get_event(id).unwrap();
    assert_eq!(retained.capture_source_id, legacy.capture_source_id);
    assert_eq!(retained.sync.metadata, legacy.sync.metadata);
}

#[test]
fn reconcile_provider_event_matching_provider_hash_preserves_payload() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let id = new_id();
    let existing = normalized_provider_event(
        id,
        json!({"text": "immutable"}),
        "provider-native-a",
        Some(ProviderEventHashAuthority::ProviderSupplied),
    );
    assert!(store.insert_event_if_absent(&existing).unwrap());
    let replay = normalized_provider_event(
        id,
        json!({"text": "rewritten"}),
        "provider-native-a",
        Some(ProviderEventHashAuthority::ProviderSupplied),
    );

    assert!(!store
        .reconcile_provider_event(&replay, ProviderEventHashAuthority::ProviderSupplied)
        .unwrap());

    let retained = store.get_event(id).unwrap();
    assert_eq!(retained.payload, existing.payload);
    assert_eq!(retained.dedupe_key, existing.dedupe_key);
}

#[test]
fn reconcile_provider_event_rejects_provider_supplied_hash_conflict() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let id = new_id();
    let existing = normalized_provider_event(
        id,
        json!({"text": "existing"}),
        "provider-native-a",
        Some(ProviderEventHashAuthority::ProviderSupplied),
    );
    assert!(store.insert_event_if_absent(&existing).unwrap());
    let conflicting = normalized_provider_event(
        id,
        json!({"text": "conflicting"}),
        "provider-native-b",
        Some(ProviderEventHashAuthority::ProviderSupplied),
    );

    let error = store
        .reconcile_provider_event(&conflicting, ProviderEventHashAuthority::ProviderSupplied)
        .unwrap_err();
    assert!(matches!(error, StoreError::ProviderEventConflict { .. }));
    let retained = store.get_event(id).unwrap();
    assert_eq!(retained.id, existing.id);
    assert_eq!(retained.payload, existing.payload);
    assert_eq!(retained.dedupe_key, existing.dedupe_key);
}

#[test]
fn native_path_exactly_migrates_released_provider_hash_in_place() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let id = new_id();
    let legacy_hash = "released-positional-hash";
    let existing = normalized_provider_event(
        id,
        json!({"text": "released"}),
        legacy_hash,
        Some(ProviderEventHashAuthority::ProviderSupplied),
    );
    assert!(store.insert_event_if_absent(&existing).unwrap());

    let replacement_body = json!({"text": "rewritten"});
    let replacement_hash = compute_payload_hash(&replacement_body).unwrap();
    let replacement =
        normalized_provider_event(new_id(), replacement_body.clone(), &replacement_hash, None);
    let (inserted, _) = store
        .reconcile_provider_event_migrating_exact_legacy_provider_hash_with_native_path_accounting(
            &replacement,
            legacy_hash,
        )
        .unwrap();
    assert!(!inserted);

    let migrated = store.get_event(id).unwrap();
    assert_eq!(migrated.id, id);
    assert_eq!(migrated.seq, existing.seq);
    assert_eq!(migrated.payload["body"], replacement_body);
    assert!(migrated
        .dedupe_key
        .as_deref()
        .unwrap()
        .ends_with(&replacement_hash));
    assert_eq!(
        migrated.sync.metadata[PROVIDER_EVENT_HASH_AUTHORITY_KEY],
        json!(ProviderEventHashAuthority::NormalizedPayloadFallback.as_str())
    );
}

#[test]
fn provider_reconciliation_elides_success_and_unknown_outputs_but_retains_failure() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let mut success = normalized_provider_event(
        new_id(),
        json!({
            "result_outcome": "success",
            "exit_code": 0,
            "output_preview": "successful output"
        }),
        "success-output",
        Some(ProviderEventHashAuthority::ProviderSupplied),
    );
    success.event_type = EventType::CommandOutput;
    assert!(!store
        .reconcile_provider_event(&success, ProviderEventHashAuthority::ProviderSupplied)
        .unwrap());

    let mut unknown = normalized_provider_event(
        new_id(),
        json!({"output_preview": "unknown output"}),
        "unknown-output",
        Some(ProviderEventHashAuthority::ProviderSupplied),
    );
    unknown.event_type = EventType::ToolOutput;
    assert!(!store
        .reconcile_provider_event(&unknown, ProviderEventHashAuthority::ProviderSupplied)
        .unwrap());

    let mut failure = normalized_provider_event(
        new_id(),
        json!({
            "result_outcome": "failure",
            "exit_code": 1,
            "output_preview": "sparse failure oracle"
        }),
        "failure-output",
        Some(ProviderEventHashAuthority::ProviderSupplied),
    );
    failure.event_type = EventType::CommandOutput;
    assert!(store
        .reconcile_provider_event(&failure, ProviderEventHashAuthority::ProviderSupplied)
        .unwrap());

    let events = store.list_events().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, failure.id);
    assert_eq!(events[0].payload["body"]["result_outcome"], "failure");
    assert_eq!(events[0].payload["body"]["exit_code"], 1);
    assert!(events[0].payload["body"].get("output_preview").is_none());
    assert!(store
        .search_event_hits("sparse failure oracle", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn cached_event_upsert_reprepares_after_schema_change() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let id = new_id();
    let mut event = normalized_provider_event(
        id,
        json!({"text": "before"}),
        "cached-upsert",
        Some(ProviderEventHashAuthority::NormalizedPayloadFallback),
    );
    store
        .write_event(&event, &mut NativePathEventBindAccounting::default())
        .unwrap();

    store
        .conn
        .execute_batch(
            "CREATE TABLE event_upsert_audit (event_id TEXT NOT NULL);
                 CREATE TRIGGER event_upsert_audit_trigger AFTER UPDATE ON events BEGIN
                     INSERT INTO event_upsert_audit VALUES (NEW.id);
                 END;",
        )
        .unwrap();
    event.payload["body"] = json!({"text": "after"});
    store
        .write_event(&event, &mut NativePathEventBindAccounting::default())
        .unwrap();

    let audit_rows: u64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM event_upsert_audit", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        audit_rows, 1,
        "schema change did not reprepare cached UPSERT"
    );
    assert_eq!(
        store.get_event(id).unwrap().payload["body"]["text"],
        "after"
    );
}

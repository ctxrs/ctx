use ctx_history_core::SessionHistoryArchive;

use super::tests::{local_preview_event, tempdir};
use crate::Store;

#[test]
fn failed_full_rebuild_atomically_preserves_the_previous_search_projection() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let event = local_preview_event(1, "atomic-rebuild-old-needle");
    store.upsert_event(&event).unwrap();
    assert_eq!(
        store
            .search_event_hits("atomic-rebuild-old-needle", 10)
            .unwrap()
            .len(),
        1
    );

    // Fail after the rebuild has deleted the prior lookup rows and begun
    // repopulating projections. The transaction must restore every old table.
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER ctx_test_fail_search_rebuild
             BEFORE INSERT ON event_search_lookup
             BEGIN SELECT RAISE(ABORT, 'injected search rebuild failure'); END;",
        )
        .unwrap();
    let error = store.refresh_search_index().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected search rebuild failure"),
        "unexpected rebuild error: {error}"
    );
    assert_eq!(
        store
            .search_event_hits("atomic-rebuild-old-needle", 10)
            .unwrap()
            .len(),
        1
    );
    let lookup_rows: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM event_search_lookup", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(lookup_rows, 1);

    drop(store);
    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(
        reopened
            .search_event_hits("atomic-rebuild-old-needle", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn rebuild_refreshes_cached_capabilities_after_dropping_malformed_event_lookup() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    store
        .conn
        .execute_batch(
            "DROP TABLE event_search_lookup;
             CREATE TABLE event_search_lookup (event_id TEXT PRIMARY KEY NOT NULL);",
        )
        .unwrap();

    store.refresh_search_index().unwrap();
    let event = local_preview_event(1, "post-rebuild-upsert-needle");
    store.upsert_event(&event).unwrap();

    assert_eq!(store.get_event(event.id).unwrap(), event);
}

#[test]
fn archive_import_and_its_full_search_rebuild_commit_or_rollback_together() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let existing = local_preview_event(1, "archive-atomic-existing-needle");
    let incoming = local_preview_event(2, "archive-atomic-incoming-needle");
    store.upsert_event(&existing).unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER ctx_test_fail_archive_search_rebuild
             BEFORE INSERT ON event_search_lookup
             BEGIN SELECT RAISE(ABORT, 'injected archive rebuild failure'); END;",
        )
        .unwrap();
    let archive = SessionHistoryArchive {
        schema_version: 2,
        version: 2,
        events: vec![incoming.clone()],
        ..SessionHistoryArchive::default()
    };

    let error = store.import_archive(&archive, false).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected archive rebuild failure"),
        "unexpected import error: {error}"
    );
    assert!(store.get_event(incoming.id).is_err());
    assert_eq!(
        store
            .search_event_hits("archive-atomic-existing-needle", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_event_hits("archive-atomic-incoming-needle", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn schema_46_upgrade_forces_one_clean_rebuild_of_a_partial_projection() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let retained = local_preview_event(1, "upgrade-retained-projection-needle");
    let missing = local_preview_event(2, "upgrade-missing-projection-needle");
    store.upsert_event(&retained).unwrap();
    store.upsert_event(&missing).unwrap();
    assert_eq!(
        store
            .search_event_hits("upgrade-missing-projection-needle", 10)
            .unwrap()
            .len(),
        1
    );

    for table in [
        "event_search",
        "event_search_scriptgram",
        "event_search_lookup",
    ] {
        store
            .conn
            .execute(
                &format!("DELETE FROM {table} WHERE event_id = ?1"),
                [missing.id.to_string()],
            )
            .unwrap();
    }
    // The retained event makes this look initialized to the legacy aggregate
    // nonzero check even though one canonical event is now unsearchable.
    assert_eq!(
        store
            .search_event_hits("upgrade-retained-projection-needle", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_event_hits("upgrade-missing-projection-needle", 10)
        .unwrap()
        .is_empty());
    store
        .conn
        .execute_batch(
            "DROP TABLE projection_journal_entities;
             DROP TABLE projection_journal_chunks;
             DROP TABLE projection_journal_state;
             DROP TABLE ctx_store_schema_identity;
             PRAGMA user_version = 46;",
        )
        .unwrap();
    drop(store);

    let upgraded = Store::open(&db_path).unwrap();
    assert_eq!(
        upgraded
            .search_event_hits("upgrade-retained-projection-needle", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        upgraded
            .search_event_hits("upgrade-missing-projection-needle", 10)
            .unwrap()
            .len(),
        1
    );
}

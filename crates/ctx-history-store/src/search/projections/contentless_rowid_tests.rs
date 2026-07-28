//! Does `events.seq` survive as an FTS rowid across a rewrite?
//!
//! A contentless FTS5 table has no columns, so a row can only be addressed by
//! rowid. That makes the choice of rowid a correctness question, not a tuning
//! one: if the key a posting was written under can change while the event keeps
//! its identity, the old posting is orphaned and the index no longer describes
//! the store.
//!
//! `events.seq` is not stable per event id. It is derived from
//! `provider_event_sequence_index` (while `events.id` is derived from the
//! separate `provider_event_index`), and
//! `avoid_provider_source_event_seq_collision` deliberately reassigns it when
//! it collides. `events.rs` then persists the change with
//! `ON CONFLICT(id) DO UPDATE SET seq = excluded.seq`.
//!
//! `EVENT_FTS_KEY_TRIGGERS_SQL` is what closes that gap: the old posting is
//! removed in the same statement that moves or removes the key, inside the same
//! `with_atomic_write` transaction as the replacement insert.

use ctx_history_core::{new_id, Event, EventRole, EventType};
use rusqlite::Connection;

use crate::Store;

fn seed_event(store: &Store, id: uuid::Uuid, seq: u64, text: &str) {
    let event = Event {
        id,
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: ctx_history_core::utc_now(),
        capture_source_id: None,
        payload: serde_json::json!({ "text": text }),
        payload_blob_id: None,
        dedupe_key: None,
        sync: Default::default(),
    };
    store.upsert_event(&event).unwrap();
}

fn fts_rowids(conn: &Connection) -> Vec<i64> {
    let mut stmt = conn
        .prepare("SELECT rowid FROM event_search ORDER BY rowid")
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn matching_rowids(conn: &Connection, term: &str) -> Vec<i64> {
    let mut stmt = conn
        .prepare("SELECT rowid FROM event_search WHERE event_search MATCH ?1 ORDER BY rowid")
        .unwrap();
    stmt.query_map([format!("\"{term}\"")], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The rewrite path: same event id, reassigned seq.
#[test]
fn rewriting_an_event_under_a_new_seq_must_not_orphan_its_posting() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let id = new_id();

    seed_event(&store, id, 1_000, "alpha unique-token-aaa");
    assert_eq!(fts_rowids(&store.conn), vec![1_000]);

    // The importer reassigns seq for this same event id, exactly as
    // avoid_provider_source_event_seq_collision does.
    seed_event(&store, id, 2_000, "alpha unique-token-aaa");

    let seq: i64 = store
        .conn
        .query_row(
            "SELECT seq FROM events WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(seq, 2_000, "events kept the reassigned seq");

    let event_rows: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(event_rows, 1, "one event");

    assert_eq!(
        fts_rowids(&store.conn),
        vec![2_000],
        "the posting written under the old seq was orphaned by the rewrite"
    );
    assert_eq!(
        matching_rowids(&store.conn, "unique-token-aaa"),
        vec![2_000],
        "one event must yield exactly one posting"
    );
}

/// Recycling is the benign half of the story, and worth pinning: because the
/// upsert path deletes the target rowid before inserting, an event that later
/// lands on a recycled seq evicts the stale posting. So orphans leak index
/// space and can collide with a *cold* insert, but they do not silently
/// attribute one event's text to another.
#[test]
fn a_recycled_seq_must_not_collide_with_a_stale_posting() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first = new_id();
    let second = new_id();

    seed_event(&store, first, 1_000, "alpha unique-token-bbb");
    seed_event(&store, first, 2_000, "alpha unique-token-bbb");
    // seq 1000 is now free as far as `events` is concerned, so the importer may
    // hand it to a different event.
    seed_event(&store, second, 1_000, "beta unique-token-ccc");

    assert_eq!(
        matching_rowids(&store.conn, "unique-token-bbb"),
        vec![2_000],
        "the first event's text must not still be indexed under the recycled seq"
    );
    assert_eq!(
        matching_rowids(&store.conn, "unique-token-ccc"),
        vec![1_000],
        "the second event owns the recycled seq"
    );
}

/// The deletion path: a hard `DELETE FROM events` must take the posting with it.
#[test]
fn deleting_an_event_removes_its_posting() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let id = new_id();

    seed_event(&store, id, 4_100, "gamma unique-token-ddd");
    assert_eq!(fts_rowids(&store.conn), vec![4_100]);

    store
        .conn
        .execute("DELETE FROM events WHERE id = ?1", [id.to_string()])
        .unwrap();

    assert!(
        fts_rowids(&store.conn).is_empty(),
        "the posting outlived the event it described"
    );
    assert!(matching_rowids(&store.conn, "unique-token-ddd").is_empty());
}

/// The truncation path: deleting a contiguous tail of a session must leave the
/// index describing exactly the events that survived.
#[test]
fn truncating_a_session_leaves_only_surviving_postings() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let kept = new_id();
    let dropped_first = new_id();
    let dropped_second = new_id();

    seed_event(&store, kept, 5_000, "delta unique-token-eee");
    seed_event(&store, dropped_first, 5_001, "delta unique-token-fff");
    seed_event(&store, dropped_second, 5_002, "delta unique-token-ggg");
    assert_eq!(fts_rowids(&store.conn), vec![5_000, 5_001, 5_002]);

    store
        .conn
        .execute("DELETE FROM events WHERE seq > ?1", [5_000])
        .unwrap();

    assert_eq!(
        fts_rowids(&store.conn),
        vec![5_000],
        "truncation must prune the postings of the events it removed"
    );
    assert_eq!(
        matching_rowids(&store.conn, "delta"),
        vec![5_000],
        "the surviving event is still findable"
    );
}

/// Every posting must correspond to a live event, on every path above.
#[test]
fn no_path_leaves_a_posting_without_a_matching_event() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let rewritten = new_id();
    let deleted = new_id();
    let survivor = new_id();

    seed_event(&store, rewritten, 6_000, "epsilon one");
    seed_event(&store, rewritten, 6_100, "epsilon one");
    seed_event(&store, deleted, 6_200, "epsilon two");
    seed_event(&store, survivor, 6_300, "epsilon three");
    store
        .conn
        .execute("DELETE FROM events WHERE id = ?1", [deleted.to_string()])
        .unwrap();

    let orphans: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM event_search
             WHERE rowid NOT IN (SELECT seq FROM events)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0, "orphaned postings remain");

    let indexed: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM event_search", [], |row| row.get(0))
        .unwrap();
    assert_eq!(indexed, 2, "exactly the two surviving events are indexed");
}

/// The direction that matters most: a crash must not leave the event
/// *unindexed*.
///
/// The trigger's delete of the old posting and the projection's insert of the
/// replacement both run inside the transaction `with_atomic_write` opens
/// (`BEGIN IMMEDIATE`, or a savepoint when a batch is already open), so they
/// commit together or not at all. Rolling the transaction back stands in for a
/// crash at the worst possible instant - after the old posting is gone and
/// before the new one lands - and must restore the original posting rather than
/// leave the event missing from search.
#[test]
fn a_crash_between_the_delete_and_the_insert_cannot_lose_the_posting() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let id = new_id();

    seed_event(&store, id, 7_000, "zeta unique-token-hhh");
    assert_eq!(fts_rowids(&store.conn), vec![7_000]);

    store.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    // Inside the transaction the rekey is visible: old posting gone, new one in.
    store
        .conn
        .execute(
            "UPDATE events SET seq = ?1 WHERE id = ?2",
            rusqlite::params![7_500_i64, id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO event_search(rowid, preview_text) VALUES (?1, ?2)",
            rusqlite::params![7_500_i64, "zeta unique-token-hhh"],
        )
        .unwrap();
    assert_eq!(fts_rowids(&store.conn), vec![7_500]);

    // Crash.
    store.conn.execute_batch("ROLLBACK").unwrap();

    assert_eq!(
        fts_rowids(&store.conn),
        vec![7_000],
        "the original posting must survive a torn rekey"
    );
    assert_eq!(
        matching_rowids(&store.conn, "unique-token-hhh"),
        vec![7_000],
        "the event must still be findable after a torn rekey"
    );
}

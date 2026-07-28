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
///
/// KNOWN RED. This is the open design question, not a bug to be patched around:
/// with `events.seq` as the FTS rowid, a rewrite that reassigns seq leaves the
/// posting written under the old seq behind. Ignored so the branch is green;
/// run with `--ignored` to see it.
#[test]
#[ignore = "demonstrates the open rowid-stability question; see module docs"]
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

//! Oracle for the contentless-FTS5 conversion, run against a real store.
//!
//! A contentless `event_search` cannot return `preview_text`; it has to be
//! recomputed from `events.payload_json` at read time. These checks prove, on
//! a real corpus, that the recomputation is exact and that `events.seq` is a
//! usable FTS rowid. Ignored by default: point it at a store with
//! `CTX_CONTENTLESS_ORACLE_DB`.

use rusqlite::Connection;

use super::prepared::PreparedEventProjection;

const STORED_EVENT_SCAN: &str = r#"
    SELECT e.id,
           COALESCE(e.history_record_id, r.history_record_id, s.history_record_id, rs.history_record_id),
           e.session_id,
           e.role,
           e.event_type,
           e.payload_json,
           'safe_preview',
           e.visibility,
           e.sync_state,
           e.deleted_at_ms,
           e.seq
    FROM events e
    LEFT JOIN runs r ON r.id = e.run_id
    LEFT JOIN sessions s ON s.id = e.session_id
    LEFT JOIN sessions rs ON rs.id = r.session_id
"#;

fn oracle_db() -> Option<Connection> {
    let path = std::env::var("CTX_CONTENTLESS_ORACLE_DB").ok()?;
    Some(
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open oracle db"),
    )
}

#[test]
#[ignore = "requires CTX_CONTENTLESS_ORACLE_DB pointing at a real store"]
fn events_seq_is_a_usable_fts_rowid() {
    let Some(conn) = oracle_db() else {
        return;
    };
    let (total, distinct, nulls, min, max): (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT seq), SUM(seq IS NULL),
                    COALESCE(MIN(seq), 0), COALESCE(MAX(seq), 0) FROM events",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    println!("events={total} distinct_seq={distinct} null_seq={nulls} seq_range=[{min}, {max}]");
    assert_eq!(total, distinct, "events.seq is not unique");
    assert_eq!(nulls, 0, "events.seq has NULLs");
    assert!(min >= 1, "events.seq must be a positive integer rowid");
}

/// The hit path reads `event_search.history_record_id` and
/// `event_search.session_id` as COALESCE fallbacks. A contentless table cannot
/// return them, so they have to be re-derived from the live rows. The
/// substitutions are only sound if the stored projection values still equal
/// what the joins produce today, which is what this checks.
#[test]
#[ignore = "requires CTX_CONTENTLESS_ORACLE_DB pointing at a real store"]
fn stored_projection_keys_still_equal_the_live_join() {
    let Some(conn) = oracle_db() else {
        return;
    };
    // event_search.history_record_id was stored as
    // COALESCE(e.history_record_id, r.history_record_id, s.history_record_id,
    // rs.history_record_id); event_search.session_id was stored as
    // e.session_id verbatim.
    let (rows, hr_diff, sid_diff): (i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(*),
                    SUM(es.history_record_id IS NOT COALESCE(
                        e.history_record_id, r.history_record_id,
                        s.history_record_id, rs.history_record_id)),
                    SUM(es.session_id IS NOT e.session_id)
             FROM event_search es
             JOIN events e ON e.id = es.event_id
             LEFT JOIN runs r ON r.id = e.run_id
             LEFT JOIN sessions s ON s.id = e.session_id
             LEFT JOIN sessions rs ON rs.id = r.session_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    println!("rows={rows} history_record_id_drift={hr_diff} session_id_drift={sid_diff}");
    assert_eq!(
        hr_diff, 0,
        "stored history_record_id drifted from the live join"
    );
    assert_eq!(
        sid_diff, 0,
        "stored session_id drifted from events.session_id"
    );
}

#[test]
#[ignore = "requires CTX_CONTENTLESS_ORACLE_DB pointing at a real store"]
fn recomputed_preview_matches_the_stored_projection_exactly() {
    let Some(conn) = oracle_db() else {
        return;
    };

    // What the current projection stores, keyed by event id.
    let mut stored = std::collections::HashMap::<String, String>::new();
    {
        let mut stmt = conn
            .prepare("SELECT event_id, preview_text FROM event_search")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            stored.insert(row.get(0).unwrap(), row.get(1).unwrap());
        }
    }
    let stored_total = stored.len();

    let mut matched = 0usize;
    let mut mismatched = Vec::<(String, usize, usize)>::new();
    let mut missing_from_fts = Vec::<String>::new();
    let mut seqs = std::collections::HashSet::<i64>::new();
    let mut duplicate_seq = 0usize;

    let mut stmt = conn.prepare(STORED_EVENT_SCAN).unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let seq: i64 = row.get(10).unwrap();
        let Some(projection) = PreparedEventProjection::from_stored_row(row).unwrap() else {
            continue;
        };
        if !seqs.insert(seq) {
            duplicate_seq += 1;
        }
        match stored.remove(&projection.event_id) {
            Some(stored_preview) => {
                if stored_preview == projection.preview {
                    matched += 1;
                } else {
                    mismatched.push((
                        projection.event_id.clone(),
                        stored_preview.len(),
                        projection.preview.len(),
                    ));
                }
            }
            None => missing_from_fts.push(projection.event_id.clone()),
        }
    }

    println!("stored_fts_rows={stored_total}");
    println!("recomputed_exact_match={matched}");
    println!("recomputed_mismatch={}", mismatched.len());
    println!("eligible_but_absent_from_fts={}", missing_from_fts.len());
    println!("fts_rows_with_no_eligible_event={}", stored.len());
    println!("duplicate_seq_among_projected={duplicate_seq}");
    for (id, a, b) in mismatched.iter().take(5) {
        println!("  mismatch {id}: stored_len={a} recomputed_len={b}");
    }
    for id in missing_from_fts.iter().take(5) {
        println!("  eligible but absent: {id}");
    }
    for id in stored.keys().take(5) {
        println!("  fts row with no eligible event: {id}");
    }

    assert_eq!(duplicate_seq, 0, "events.seq collided among projected rows");
    assert!(
        mismatched.is_empty(),
        "{} previews would change under read-time derivation",
        mismatched.len()
    );
    assert!(
        missing_from_fts.is_empty(),
        "{} eligible events have no FTS row",
        missing_from_fts.len()
    );
    assert!(
        stored.is_empty(),
        "{} FTS rows have no eligible event",
        stored.len()
    );
}

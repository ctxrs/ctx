//! Read-time cost of deriving previews instead of storing them.
//!
//! A contentless `event_search` cannot return `preview_text`, so the hit path
//! has to recompute it. The question is what that costs per result. The hit
//! path already reads the whole `events.payload_json` and already parses it as
//! JSON for cursor extraction (`event_search_cursor`), so the marginal cost
//! should be the preview function on an already-parsed value, not a new read
//! and not a new parse. These benchmarks measure that on real payloads.
//!
//! Ignored by default: point at a store with `CTX_CONTENTLESS_ORACLE_DB`.

use std::time::Instant;

use ctx_history_core::{EventRole, EventType, RedactionState};
use rusqlite::Connection;

use super::encoding::event_search_preview_from_payload;

fn oracle_db() -> Option<Connection> {
    let path = std::env::var("CTX_CONTENTLESS_ORACLE_DB").ok()?;
    Some(
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open oracle db"),
    )
}

/// One realistic hit's worth of inputs.
struct Hit {
    payload_json: String,
    event_type: EventType,
    role: Option<EventRole>,
    stored_preview: String,
}

fn load_hits(conn: &Connection, limit: usize) -> Vec<Hit> {
    let mut stmt = conn
        .prepare(
            "SELECT e.payload_json, e.event_type, e.role, es.preview_text
             FROM event_search es
             JOIN events e ON e.id = es.event_id
             LIMIT ?1",
        )
        .unwrap();
    let rows = stmt
        .query_map([limit as i64], |row| {
            Ok(Hit {
                payload_json: row.get(0)?,
                event_type: crate::connection::parse_text_enum::<EventType>(
                    row.get::<_, String>(1)?,
                )
                .unwrap(),
                role: crate::connection::parse_optional_text_enum::<EventRole>(row.get(2)?)
                    .unwrap(),
                stored_preview: row.get(3)?,
            })
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// What the hit path does today: parse payload_json for the cursor, and read
/// the preview straight out of the FTS content table.
fn current_per_hit(hit: &Hit) -> usize {
    let parsed = serde_json::from_str::<serde_json::Value>(&hit.payload_json).ok();
    let cursor = parsed
        .as_ref()
        .and_then(|p| p.get("cursor"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    hit.stored_preview.len() + cursor.map(|c| c.len()).unwrap_or(0)
}

/// What a contentless hit path does: one parse serving both the cursor and the
/// derived preview.
fn contentless_per_hit(hit: &Hit) -> usize {
    let parsed = serde_json::from_str::<serde_json::Value>(&hit.payload_json).ok();
    let cursor = parsed
        .as_ref()
        .and_then(|p| p.get("cursor"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let preview = match parsed.as_ref() {
        Some(payload) => event_search_preview_from_payload(
            hit.event_type,
            hit.role,
            payload,
            RedactionState::SafePreview,
        ),
        None => String::new(),
    };
    preview.len() + cursor.map(|c| c.len()).unwrap_or(0)
}

fn bench(label: &str, hits: &[Hit], f: impl Fn(&Hit) -> usize) -> f64 {
    // Warm, then take the best of five passes to suppress host noise.
    let mut best = f64::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        let mut sink = 0usize;
        for hit in hits {
            sink = sink.wrapping_add(f(hit));
        }
        std::hint::black_box(sink);
        let elapsed = start.elapsed().as_secs_f64();
        best = best.min(elapsed);
    }
    let per_hit_us = best / hits.len() as f64 * 1e6;
    let total_ms = best * 1e3;
    println!("  {label:<28} total={total_ms:8.3} ms   per_hit={per_hit_us:7.3} us");
    per_hit_us
}

#[test]
#[ignore = "requires CTX_CONTENTLESS_ORACLE_DB pointing at a real store"]
fn read_time_preview_derivation_cost_per_result() {
    let Some(conn) = oracle_db() else {
        return;
    };
    // Realistic result counts: the CLI default is 20 and the cap is 200.
    for count in [20usize, 50, 200, 2000] {
        let hits = load_hits(&conn, count);
        if hits.is_empty() {
            continue;
        }
        let payload_bytes: usize = hits.iter().map(|h| h.payload_json.len()).sum();
        println!(
            "results={count} loaded={} payload_bytes={payload_bytes} \
             avg_payload={:.0}B",
            hits.len(),
            payload_bytes as f64 / hits.len() as f64
        );
        let current = bench("current (stored preview)", &hits, current_per_hit);
        let contentless = bench("contentless (derived)", &hits, contentless_per_hit);
        let delta = contentless - current;
        println!(
            "  delta={delta:+.3} us/hit   for {count} results = {:+.3} ms\n",
            delta * hits.len() as f64 / 1000.0
        );
    }
}

#[test]
#[ignore = "requires CTX_CONTENTLESS_ORACLE_DB pointing at a real store"]
fn derived_preview_equals_stored_preview_on_sampled_hits() {
    let Some(conn) = oracle_db() else {
        return;
    };
    let hits = load_hits(&conn, 20_000);
    let mut mismatched = 0usize;
    for hit in &hits {
        let parsed = serde_json::from_str::<serde_json::Value>(&hit.payload_json).unwrap();
        let derived = event_search_preview_from_payload(
            hit.event_type,
            hit.role,
            &parsed,
            RedactionState::SafePreview,
        );
        if derived != hit.stored_preview {
            mismatched += 1;
        }
    }
    println!("sampled={} mismatched={mismatched}", hits.len());
    assert_eq!(mismatched, 0);
}

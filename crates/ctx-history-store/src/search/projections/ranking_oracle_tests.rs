//! Differential ranking oracle.
//!
//! Any change to how the event index is built or maintained - batching
//! projection maintenance, going contentless, changing the rowid - has to leave
//! ranking bit-identical, not merely similar. This compares two stores built by
//! different paths and asserts the same documents come back with the same
//! `bm25()` scores in the same order.
//!
//! Two properties make it worth running rather than reasoning about:
//!
//! * It compares scores, not just result sets. Two indexes can agree on which
//!   documents match and still rank them differently.
//! * It does not apply `LIMIT` inside the FTS scan. Doing so cuts a score-tie
//!   group at a different member in each arm whenever the arms use different
//!   rowids, which looks exactly like a ranking regression and is not one. The
//!   real query materialises all matches and orders by
//!   `matched_terms, score, occurred_at_ms, seq, id`, so the cut is applied
//!   after a shared deterministic sort here too.
//!
//! Ignored by default. Point it at two stores:
//!
//! ```text
//! CTX_RANKING_ORACLE_BASELINE=/path/a/work.sqlite \
//! CTX_RANKING_ORACLE_CANDIDATE=/path/b/work.sqlite \
//!   cargo test -p ctx-history-store --lib ranking_oracle -- --ignored --nocapture
//! ```

use std::collections::HashMap;

use rusqlite::{Connection, OpenFlags};

/// Vocabulary sampled from real user-role preview text, plus the phrase,
/// boolean and prefix forms the CLI can produce.
const TERMS: &[&str] = &[
    "home",
    "daddy",
    "code",
    "ctx",
    "work",
    "repo",
    "task",
    "workspace",
    "instructions",
    "branch",
    "agents",
    "public",
    "objective",
    "worktrees",
    "codex",
    "worktree",
    "launch",
    "changes",
    "goal",
    "repos",
    "manual",
    "plan",
    "requires",
    "local",
    "source",
    "status",
    "canonical",
    "monorepo",
    "file",
    "routing",
    "ctxrs",
    "progress",
    "handoffs",
    "subagents",
    "relevant",
    "private",
    "origin",
    "main",
    "optimized",
    "final",
    "directory",
    "latest",
    "perf",
    "complete",
    "review",
    "tokens",
    "session",
    "rust",
    "continuation",
    "cwd",
    "shell",
    "environment_context",
    "hostkey",
    "budget",
    "context",
    "error",
    "timeout",
    "cargo",
    "clippy",
    "sqlite",
    "index",
    "migration",
    "schema",
    "rustfmt",
    "panic",
    "unwrap",
    "async",
    "tokio",
    "provider",
    "claude",
    "fts5",
    "bm25",
    "vacuum",
    "event",
    "rebuild",
    "python",
    "bazel",
    "commit",
    "rebase",
    "failed",
    "assert",
    "null",
    "json",
    "parse",
    "decode",
    "encode",
    "benchmark",
    "latency",
    "throughput",
];
const PHRASES: &[&str] = &[
    "\"cargo build\"",
    "\"cargo clippy\"",
    "\"cargo fmt\"",
    "\"git worktree\"",
    "\"origin main\"",
    "\"schema version\"",
    "\"query plan\"",
    "\"search index\"",
    "\"no such module\"",
    "\"history record\"",
    "\"capture source\"",
    "\"bazel test\"",
    "\"index watch\"",
    "\"projection journal\"",
];
const BOOLEAN: &[&str] = &[
    "test OR tests",
    "search AND index",
    "schema OR migration OR index",
    "worktree AND branch",
    "error AND (timeout OR panic)",
];
const PREFIX: &[&str] = &["work*", "migrat*", "index*", "sess*"];
/// The cut is applied after a shared sort, so these exercise tie handling at
/// the boundary rather than inside the FTS scan.
const LIMITS: &[usize] = &[1, 5, 20, 50, 120, 200, 1000];

fn open(path: &str) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open store")
}

/// `(event_id, bm25)` for every match, however the store keys its index.
///
/// A pre-v48 index exposes `event_id` directly; a contentless one only has a
/// rowid, which is mapped back through `events.seq`.
fn hits(conn: &Connection, query: &str) -> Vec<(String, f64)> {
    let contentless = conn
        .prepare("SELECT event_id FROM event_search LIMIT 0")
        .is_err();
    let sql = if contentless {
        "SELECT e.id, bm25(event_search) FROM event_search
         JOIN events e ON e.seq = event_search.rowid
         WHERE event_search MATCH ?1"
    } else {
        "SELECT event_search.event_id, bm25(event_search) FROM event_search
         WHERE event_search MATCH ?1"
    };
    let mut stmt = conn.prepare(sql).expect("prepare hit query");
    let mut rows = stmt
        .query_map([query], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .expect("run hit query")
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    rows
}

#[test]
#[ignore = "requires CTX_RANKING_ORACLE_BASELINE and CTX_RANKING_ORACLE_CANDIDATE"]
fn two_index_builds_rank_identically() {
    let (Ok(baseline_path), Ok(candidate_path)) = (
        std::env::var("CTX_RANKING_ORACLE_BASELINE"),
        std::env::var("CTX_RANKING_ORACLE_CANDIDATE"),
    ) else {
        return;
    };
    let baseline = open(&baseline_path);
    let candidate = open(&candidate_path);

    let mut compared = 0usize;
    let mut set_mismatch = Vec::new();
    let mut score_mismatch = Vec::new();
    let mut order_mismatch = Vec::new();

    let queries = TERMS
        .iter()
        .chain(PHRASES)
        .chain(BOOLEAN)
        .chain(PREFIX)
        .copied()
        .collect::<Vec<_>>();

    for query in &queries {
        let a = hits(&baseline, query);
        let b = hits(&candidate, query);
        for &limit in LIMITS {
            compared += 1;
            let a = &a[..a.len().min(limit)];
            let b = &b[..b.len().min(limit)];
            let sa: HashMap<&str, f64> = a.iter().map(|(id, s)| (id.as_str(), *s)).collect();
            let sb: HashMap<&str, f64> = b.iter().map(|(id, s)| (id.as_str(), *s)).collect();
            if sa.keys().collect::<Vec<_>>().len() != sb.keys().collect::<Vec<_>>().len()
                || !sa.keys().all(|k| sb.contains_key(k))
            {
                set_mismatch.push((*query, limit));
                continue;
            }
            if let Some(id) = sa
                .keys()
                .find(|id| sa[**id].to_bits() != sb[**id].to_bits())
            {
                score_mismatch.push((*query, limit, sa[*id], sb[*id]));
                continue;
            }
            if a.iter().map(|(id, _)| id).ne(b.iter().map(|(id, _)| id)) {
                order_mismatch.push((*query, limit));
            }
        }
    }

    println!("queries compared       : {compared}");
    println!("result-set mismatches  : {}", set_mismatch.len());
    println!("bm25 score mismatches  : {}", score_mismatch.len());
    println!("order mismatches       : {}", order_mismatch.len());
    for entry in set_mismatch.iter().take(10) {
        println!("  SET   {entry:?}");
    }
    for entry in score_mismatch.iter().take(10) {
        println!("  SCORE {entry:?}");
    }
    for entry in order_mismatch.iter().take(10) {
        println!("  ORDER {entry:?}");
    }

    assert!(compared > 0, "no queries compared");
    assert!(set_mismatch.is_empty(), "result sets diverged");
    assert!(score_mismatch.is_empty(), "bm25 scores diverged");
    assert!(order_mismatch.is_empty(), "ranking order diverged");
}

use std::{fs, time::Duration};

use rusqlite::Connection;

use super::*;

fn tempdir() -> tempfile::TempDir {
    crate::test_support_paths::tempdir()
        .expect("system temporary directory should support probe fixtures")
}

fn sqlite_component_bytes(path: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut paths = vec![path.to_path_buf()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        paths.push(PathBuf::from(sidecar));
    }
    paths
        .into_iter()
        .map(|component| {
            let bytes = fs::read(&component).ok();
            (component, bytes)
        })
        .collect()
}

#[test]
fn sqlite_probe_reads_committed_live_wal_without_mutating_provider_files() {
    let temp = tempdir();
    let path = temp.path().join("forge.db");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch("create table conversations (id text primary key);")
        .unwrap();
    let before = sqlite_component_bytes(&path);
    assert!(before.iter().any(|(path, bytes)| {
        path.to_string_lossy().ends_with("-wal")
            && bytes.as_ref().is_some_and(|bytes| !bytes.is_empty())
    }));

    assert_eq!(
        has_forgecode_conversations_table(&path),
        BoundedProbe::Found
    );
    assert_eq!(sqlite_component_bytes(&path), before);
    drop(writer);
}

#[test]
fn sqlite_probe_fails_closed_for_corruption_and_oversized_sources() {
    let temp = tempdir();
    let corrupt = temp.path().join("corrupt.db");
    fs::write(&corrupt, b"not a sqlite database").unwrap();
    assert_eq!(
        has_forgecode_conversations_table(&corrupt),
        BoundedProbe::IoError
    );

    let oversized = temp.path().join("oversized.db");
    fs::File::create(&oversized)
        .unwrap()
        .set_len(SQLITE_PROBE_MAX_TOTAL_BYTES + 1)
        .unwrap();
    assert_eq!(
        has_forgecode_conversations_table(&oversized),
        BoundedProbe::BudgetExhausted
    );
}

#[test]
fn sqlite_probe_deadline_interrupts_expensive_queries() {
    let temp = tempdir();
    let path = temp.path().join("deadline.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch("create table conversations (id text);")
        .unwrap();
    let limits = SqliteProbeLimits {
        deadline: Duration::ZERO,
        max_progress_calls: usize::MAX,
        ..SqliteProbeLimits::default()
    };

    let outcome = sqlite_structural_probe(&path, limits, |connection| {
        connection.query_row(
            "with recursive counter(value) as (\
                 values(0) union all select value + 1 from counter where value < 10000000\
             ) select max(value) = 10000000 from counter",
            [],
            |row| row.get::<_, bool>(0),
        )
    });
    assert_eq!(outcome, BoundedProbe::BudgetExhausted);
}

#[test]
fn sqlite_probe_connections_are_query_only() {
    let temp = tempdir();
    let path = temp.path().join("query-only.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch("create table conversations (id text);")
        .unwrap();

    let outcome = sqlite_structural_probe(&path, SqliteProbeLimits::default(), |connection| {
        let query_only =
            connection.pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))?;
        Ok(query_only
            && connection
                .execute("create table denied (id integer)", [])
                .is_err())
    });
    assert_eq!(outcome, BoundedProbe::Found);
}

#[test]
fn recursive_probe_visits_sorted_entries_before_spending_the_budget() {
    let temp = tempdir();
    let root = temp.path();
    fs::create_dir(root.join("z-decoy")).unwrap();
    fs::write(root.join("z-decoy/other.txt"), b"decoy").unwrap();
    fs::create_dir(root.join("a-match")).unwrap();
    fs::write(root.join("a-match/session.jsonl"), b"{}\n").unwrap();

    assert_eq!(
        has_jsonl_file_under_matching(root, 3, |_| true),
        BoundedProbe::Found
    );
    let sorted = sorted_probe_entries(root, 2).unwrap();
    assert!(sorted[0].ends_with("a-match"));
    assert!(sorted[1].ends_with("z-decoy"));
}

#[test]
fn oversized_directories_exhaust_before_order_can_change_the_result() {
    let temp = tempdir();
    fs::write(temp.path().join("a-match.jsonl"), b"{}\n").unwrap();
    fs::write(temp.path().join("z-decoy.txt"), b"decoy").unwrap();

    assert_eq!(
        has_jsonl_file_under_matching(temp.path(), 1, |_| true),
        BoundedProbe::BudgetExhausted
    );
}

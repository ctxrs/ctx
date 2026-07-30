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
    let data = tempdir();
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
        has_forgecode_conversations_table(Some(data.path()), &path),
        BoundedProbe::Found
    );
    assert_eq!(sqlite_component_bytes(&path), before);
    let staging = data.path().join("tmp/provider-sqlite");
    assert!(staging.is_dir());
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
    drop(writer);
}

#[test]
fn sqlite_probe_fails_closed_for_corruption_and_oversized_sources() {
    let temp = tempdir();
    let corrupt = temp.path().join("corrupt.db");
    fs::write(&corrupt, b"not a sqlite database").unwrap();
    assert_eq!(
        has_forgecode_conversations_table(None, &corrupt),
        BoundedProbe::IoError
    );

    let oversized = temp.path().join("oversized.db");
    fs::File::create(&oversized)
        .unwrap()
        .set_len(SQLITE_PROBE_MAX_TOTAL_BYTES + 1)
        .unwrap();
    assert_eq!(
        has_forgecode_conversations_table(None, &oversized),
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

    let outcome = sqlite_structural_probe(None, &path, limits, |connection| {
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

    let outcome =
        sqlite_structural_probe(None, &path, SqliteProbeLimits::default(), |connection| {
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
fn sqlite_probe_rejects_source_mutation_during_structural_query() {
    let temp = tempdir();
    let path = temp.path().join("mutation.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch("create table conversations (id text);")
        .unwrap();

    let outcome =
        sqlite_structural_probe(None, &path, SqliteProbeLimits::default(), |connection| {
            let present = connection.query_row(
                "select exists(select 1 from sqlite_schema where name = 'conversations')",
                [],
                |row| row.get::<_, bool>(0),
            )?;
            Connection::open(&path)?.pragma_update(None, "user_version", 7)?;
            Ok(present)
        });
    assert_eq!(outcome, BoundedProbe::IoError);
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

#[test]
fn cursor_probe_accepts_every_exact_layout_entry_point() {
    let temp = tempdir();
    let data_root = temp.path().join(".cursor");
    let projects = data_root.join("projects");
    let project = projects.join("project");
    let transcripts = project.join("agent-transcripts");
    let session = transcripts.join("session");
    let transcript = session.join("session.jsonl");
    fs::create_dir_all(&session).unwrap();
    fs::write(&transcript, b"{}\n").unwrap();

    for input in [
        data_root.as_path(),
        projects.as_path(),
        project.as_path(),
        transcripts.as_path(),
        session.as_path(),
        transcript.as_path(),
    ] {
        assert_eq!(
            has_cursor_agent_transcript(input),
            BoundedProbe::Found,
            "input {}",
            input.display()
        );
    }
}

#[test]
fn cursor_probe_rejects_mismatches_and_loose_nested_lookalikes() {
    let temp = tempdir();
    let projects = temp.path().join("projects");
    let mismatch = projects.join("project/agent-transcripts/session/wrong.jsonl");
    fs::create_dir_all(mismatch.parent().unwrap()).unwrap();
    fs::write(&mismatch, b"{}\n").unwrap();
    assert_eq!(
        has_cursor_agent_transcript(&projects),
        BoundedProbe::NotFound
    );

    let loose = temp
        .path()
        .join("loose/nested/project/agent-transcripts/session/session.jsonl");
    fs::create_dir_all(loose.parent().unwrap()).unwrap();
    fs::write(&loose, b"{}\n").unwrap();
    assert_eq!(
        has_cursor_agent_transcript(temp.path()),
        BoundedProbe::NotFound
    );
}

#[test]
fn cursor_probe_preserves_discovery_budget_and_missing_error_types() {
    const CURSOR_DIRECTORY_ENTRY_LIMIT: usize = 1_024;
    let temp = tempdir();
    let oversized = temp.path().join("oversized");
    fs::create_dir(&oversized).unwrap();
    for index in 0..=CURSOR_DIRECTORY_ENTRY_LIMIT {
        fs::write(oversized.join(format!("entry-{index:04}")), b"").unwrap();
    }
    assert_eq!(
        has_cursor_agent_transcript(&oversized),
        BoundedProbe::BudgetExhausted
    );
    assert_eq!(
        has_cursor_agent_transcript(&temp.path().join("missing")),
        BoundedProbe::NotFound
    );
}

#[cfg(unix)]
#[test]
fn cursor_probe_maps_symlink_rejection_to_io_error() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let real = temp.path().join("real");
    fs::create_dir(&real).unwrap();
    let linked = temp.path().join("linked");
    symlink(&real, &linked).unwrap();

    assert_eq!(has_cursor_agent_transcript(&linked), BoundedProbe::IoError);
}

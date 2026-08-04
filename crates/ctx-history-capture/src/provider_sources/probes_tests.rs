use std::{collections::BTreeMap, ffi::OsString, fs, time::Duration};

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

fn directory_file_bytes(path: &Path) -> BTreeMap<OsString, Vec<u8>> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
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
        .execute_batch(
            "create table conversations (
                id text primary key,
                payload text not null
            );",
        )
        .unwrap();
    writer
        .execute(
            "insert into conversations (id, payload) values (?1, ?2)",
            ("large-live-wal", "x".repeat(384 * 1024)),
        )
        .unwrap();
    let before = sqlite_component_bytes(&path);
    let before_directory = directory_file_bytes(temp.path());
    assert!(before.iter().any(|(path, bytes)| {
        path.to_string_lossy().ends_with("-wal")
            && bytes.as_ref().is_some_and(|bytes| !bytes.is_empty())
    }));

    assert_eq!(
        has_forgecode_conversations_table(Some(data.path()), &path),
        BoundedProbe::Found
    );
    assert_eq!(sqlite_component_bytes(&path), before);
    assert_eq!(directory_file_bytes(temp.path()), before_directory);
    let staging = data.path().join("tmp/provider-sqlite");
    assert!(staging.is_dir());
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
    drop(writer);
}

#[test]
fn lingma_probe_reads_large_committed_live_wal_without_mutating_provider_files() {
    let temp = tempdir();
    let data = tempdir();
    let path = temp.path().join("local.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "create table chat_record (
                session_id text not null,
                request_id text,
                chat_prompt text,
                summary text,
                error_result text,
                gmt_create integer,
                extra text
            );",
        )
        .unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute(
            "insert into chat_record (
                session_id, request_id, chat_prompt, summary,
                error_result, gmt_create, extra
            ) values (?1, ?2, ?3, null, null, 1, null)",
            ("session", "request", "x".repeat(384 * 1024)),
        )
        .unwrap();
    let before = sqlite_component_bytes(&path);
    let before_directory = directory_file_bytes(temp.path());

    assert_eq!(
        has_lingma_chat_record_table(Some(data.path()), &path),
        BoundedProbe::Found
    );
    assert_eq!(sqlite_component_bytes(&path), before);
    assert_eq!(directory_file_bytes(temp.path()), before_directory);
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

fn trae_probe_database(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute("create table ItemTable ([key] text primary key, value)", [])
        .unwrap();
    connection
}

fn replace_trae_probe_value(connection: &Connection, value: rusqlite::types::Value) {
    connection.execute("delete from ItemTable", []).unwrap();
    connection
        .execute(
            "insert into ItemTable ([key], value) values (?1, ?2)",
            rusqlite::params![TRAE_CHAT_KEYS[0], value],
        )
        .unwrap();
}

#[test]
fn trae_probe_rejects_invalid_non_text_and_unrecognized_payloads() {
    let temp = tempdir();
    let path = temp.path().join("database.db");
    let connection = trae_probe_database(&path);

    for value in [
        rusqlite::types::Value::Text("arbitrary nonempty garbage".to_owned()),
        rusqlite::types::Value::Blob(br#"{"list":[]}"#.to_vec()),
        rusqlite::types::Value::Text(r#"{"futureSessions":[]}"#.to_owned()),
    ] {
        replace_trae_probe_value(&connection, value);
        assert_eq!(
            has_trae_state_vscdb_chat_history(None, &path, 10_000),
            BoundedProbe::IoError
        );
    }
}

#[test]
fn trae_probe_rejects_values_over_the_importer_bound() {
    let temp = tempdir();
    let path = temp.path().join("database.db");
    let connection = trae_probe_database(&path);
    let oversized = u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .unwrap()
        .saturating_sub(TRAE_SQLITE_VALUE_OVERHEAD_BYTES)
        .saturating_sub(u64::try_from(TRAE_CHAT_KEYS[0].len()).unwrap())
        .saturating_add(1);
    connection
        .execute(
            "insert into ItemTable ([key], value) values (?1, cast(zeroblob(?2) as text))",
            rusqlite::params![TRAE_CHAT_KEYS[0], i64::try_from(oversized).unwrap()],
        )
        .unwrap();

    assert_eq!(
        has_trae_state_vscdb_chat_history(None, &path, 10_000),
        BoundedProbe::IoError
    );
}

#[test]
fn trae_probe_distinguishes_supported_content_from_valid_empty_containers() {
    let temp = tempdir();
    let path = temp.path().join("database.db");
    let connection = trae_probe_database(&path);

    for payload in [
        r#"{"list":[]}"#,
        r#"{"list":[{"id":"session-1","messages":[]}]}"#,
    ] {
        replace_trae_probe_value(
            &connection,
            rusqlite::types::Value::Text(payload.to_owned()),
        );
        assert_eq!(
            has_trae_state_vscdb_chat_history(None, &path, 10_000),
            BoundedProbe::NotFound
        );
    }

    replace_trae_probe_value(
        &connection,
        rusqlite::types::Value::Text(
            r#"{"list":[{"id":"session-1","messages":[{"role":"user","content":"hello"}]}]}"#
                .to_owned(),
        ),
    );
    assert_eq!(
        has_trae_state_vscdb_chat_history(None, &path, 10_000),
        BoundedProbe::Found
    );
}

#[test]
fn trae_probe_rejects_duplicate_known_keys_before_payload_precedence() {
    let temp = tempdir();
    let path = temp.path().join("database.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("create table ItemTable ([key] text, value text)", [])
        .unwrap();
    connection
        .execute(
            "insert into ItemTable ([key], value) values (?1, ?2), (?1, ?3)",
            rusqlite::params![
                TRAE_CHAT_KEYS[0],
                r#"{"list":[{"id":"supported","messages":[{"content":"hello"}]}]}"#,
                r#"{"list":[]}"#,
            ],
        )
        .unwrap();

    assert_eq!(
        has_trae_state_vscdb_chat_history(None, &path, 10_000),
        BoundedProbe::IoError
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
fn sidecar_free_sqlite_probe_leaves_the_provider_directory_unchanged() {
    let temp = tempdir();
    let data = tempdir();
    let path = temp.path().join("sidecar-free.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch("create table conversations (id text);")
        .unwrap();
    fs::write(temp.path().join("unrelated-provider-state"), b"unchanged").unwrap();
    let before = sqlite_component_bytes(&path);
    let before_directory = directory_file_bytes(temp.path());
    assert!(!before_directory.contains_key(&OsString::from("sidecar-free.db-wal")));
    assert!(!before_directory.contains_key(&OsString::from("sidecar-free.db-shm")));
    let components = sqlite_probe_components(&path, SQLITE_PROBE_MAX_TOTAL_BYTES).unwrap();

    let database = open_sqlite_probe_database(Some(data.path()), &path, &components).unwrap();
    assert!(database
        .connection()
        .query_row("select exists(select 1 from sqlite_schema)", [], |row| row
            .get::<_, bool>(
            0
        ))
        .unwrap());
    drop(database);
    assert_eq!(sqlite_component_bytes(&path), before);
    assert_eq!(directory_file_bytes(temp.path()), before_directory);
    assert!(!path.with_file_name("sidecar-free.db-wal").exists());
    assert!(!path.with_file_name("sidecar-free.db-shm").exists());
    let staging = data.path().join("tmp/provider-sqlite");
    assert!(!staging.exists() || fs::read_dir(staging).unwrap().next().is_none());
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

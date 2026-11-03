use super::*;

// Fixture construction is deliberately outside the measured process. SQLite's
// allocator counter includes its native cell buffers, unlike a Rust allocator.
#[cfg(target_os = "linux")]
#[test]
fn oversized_snapshot_cells_are_bounded_before_hydration() {
    const FIXTURE: &str = "CTX_TEST_OPENCODE_SNAPSHOT_FIXTURE";
    if let Some(database) = std::env::var_os(FIXTURE) {
        let data = tempfile::tempdir().unwrap();
        unsafe { rusqlite::ffi::sqlite3_memory_highwater(1) };
        let snapshot =
            open_root_authorized_snapshot_retained(data.path(), Path::new(&database)).unwrap();
        let peak = unsafe { rusqlite::ffi::sqlite3_memory_highwater(0) };
        assert!(peak > 0, "SQLite allocation accounting must be enabled");
        eprintln!("snapshot SQLite peak allocation: {peak} bytes");
        eprintln!(
            "{}",
            fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .unwrap()
        );
        snapshot.sqlite_snapshot.finish().unwrap();
        assert!(
            peak < 64 * 1024 * 1024,
            "snapshot materialized an oversized cell: {peak}"
        );
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("opencode.db");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type":"text","text":"valid sibling"}),
    );
    connection
        .execute_batch(
            "INSERT INTO part(id,message_id,session_id,time_created,time_updated,data)
         SELECT 'oversized',message_id,session_id,time_created+1,time_updated+1,
                CAST(zeroblob(128*1024*1024) AS TEXT) FROM part LIMIT 1;",
        )
        .unwrap();
    drop(connection);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "oversized_snapshot_cells_are_bounded_before_hydration",
            "--nocapture",
        ])
        .env(FIXTURE, &database)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn bounded_and_full_snapshots_keep_the_same_records_bytes_and_rejections() {
    for (table, mode) in [
        ("message", "legacy"),
        ("message", "parent"),
        ("part", "indexed"),
        ("part", "collated"),
        ("part", "parent-collated"),
        ("part", "fallback"),
        ("session_message", "indexed"),
        ("session_message", "fallback"),
        ("session_entry", "indexed"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let database = temp.path().join("opencode.db");
        let source = write_current_schema(
            &database,
            temp.path(),
            &json!({"type":"text","text":"valid sibling"}),
        );
        if table == "message" && mode == "legacy" {
            source.execute_batch("DROP TABLE part;
                UPDATE message SET data='{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"valid sibling\"}]}';").unwrap();
        } else if table == "session_message" || table == "session_entry" {
            source
                .execute_batch(
                    "DELETE FROM part; DELETE FROM message;
                INSERT INTO session_message VALUES('valid','current-session','user',0,
                    1782259200000,1782259200000,'{\"text\":\"valid sibling\"}');",
                )
                .unwrap();
            if table == "session_entry" || mode == "fallback" {
                source
                    .execute_batch("ALTER TABLE session_message DROP COLUMN seq")
                    .unwrap();
            }
            if table == "session_entry" {
                source
                    .execute_batch("ALTER TABLE session_message RENAME TO session_entry")
                    .unwrap();
            }
        } else if mode == "fallback" {
            source
                .execute_batch("DROP INDEX part_message_id_id_idx")
                .unwrap();
        }
        let columns = source
            .prepare(&format!("SELECT * FROM {table}"))
            .unwrap()
            .column_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        for (id, payload) in [
            ("large-text", "CAST(zeroblob(?1) AS TEXT)"),
            ("large-blob", "zeroblob(?1)"),
        ] {
            let expressions = columns
                .iter()
                .map(|column| match column.as_str() {
                    "id" => format!("'{id}'"),
                    "data" => payload.into(),
                    "seq" => "seq+1".into(),
                    _ => column.clone(),
                })
                .collect::<Vec<_>>()
                .join(",");
            source.execute(&format!("INSERT INTO {table} SELECT {expressions} FROM {table} ORDER BY rowid DESC LIMIT 1"),
                [MAX_PROVIDER_SQLITE_VALUE_BYTES as i64 + 1]).unwrap();
        }
        if mode == "parent" {
            // Small parts must retain their oversized parents' size/storage
            // rejections, not become missing-parent records or disappear.
            source
                .execute_batch(
                    "INSERT INTO part
                SELECT m.id,m.id,p.session_id,p.time_created,p.time_updated,p.data
                FROM message m CROSS JOIN part p
                WHERE m.id IN ('large-text','large-blob') AND p.id='current-part';",
                )
                .unwrap();
        } else if mode.ends_with("collated") {
            let collation = if mode == "collated" {
                "NOCASE"
            } else {
                "BINARY"
            };
            source.execute_batch(&format!(
                "INSERT INTO part SELECT 'A-valid',message_id,session_id,time_created,time_updated,data
                    FROM part WHERE id='current-part';
                 CREATE TABLE replacement_message(id TEXT COLLATE NOCASE PRIMARY KEY,
                    session_id TEXT NOT NULL,time_created INTEGER NOT NULL,time_updated INTEGER NOT NULL,data TEXT NOT NULL);
                 INSERT INTO replacement_message SELECT CASE id WHEN 'current-assistant' THEN 'b' ELSE 'Z' END,
                    session_id,1782259200000,time_updated,data FROM message;
                 DROP TABLE message; ALTER TABLE replacement_message RENAME TO message;
                 CREATE INDEX message_session_time_created_id_idx ON message(session_id,time_created,id COLLATE BINARY);
                 CREATE TABLE replacement_part(id TEXT PRIMARY KEY,message_id TEXT COLLATE {collation} NOT NULL,
                    session_id TEXT NOT NULL,time_created INTEGER NOT NULL,time_updated INTEGER NOT NULL,data TEXT NOT NULL);
                 INSERT INTO replacement_part SELECT id,CASE id WHEN 'A-valid' THEN 'z' ELSE 'B' END,
                    session_id,time_created,time_updated,data FROM part;
                 DROP TABLE part; ALTER TABLE replacement_part RENAME TO part;
                 CREATE INDEX part_message_id_id_idx ON part(message_id COLLATE BINARY,id,data);"
            )).unwrap();
        }
        drop(source);
        let retained = retain_root_authorized_source(data.path(), &database).unwrap();
        let full = retained
            .sqlite_authority
            .open_stable_snapshot(&retained.database_leaf)
            .unwrap();
        let bounded = open_root_authorized_snapshot_retained(data.path(), &database)
            .unwrap()
            .sqlite_snapshot;
        let capture = |snapshot: SqliteSourceReadSnapshot| {
            let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
            let observation = observe_logical_source(&snapshot, dialect).unwrap();
            let mut records = Vec::new();
            let mut rejections = Vec::new();
            let scan =
                scan_pinned_source(&database, dialect, &observation, snapshot, &mut |output| {
                    match output {
                        OpenCodeScanOutput::Document(record) => records.push(record),
                        OpenCodeScanOutput::Rejection(row) => {
                            rejections.push((row.line_number, row.class, row.detail))
                        }
                        _ => {}
                    }
                    Ok(())
                })
                .unwrap();
            (scan.certificate, records, rejections)
        };
        let expected = capture(full);
        assert!(
            !expected.1.is_empty(),
            "{table} must retain its valid sibling"
        );
        if mode.ends_with("collated") {
            assert_eq!(
                expected
                    .1
                    .iter()
                    .map(|r| (r.native_event_id.clone(), r.event_sequence))
                    .collect::<Vec<_>>(),
                [
                    (Some(TypedKey::utf8("A-valid").unwrap()), 0),
                    (Some(TypedKey::utf8("current-part").unwrap()), 1)
                ]
            );
        }
        if mode == "collated" {
            use ctx_history_capture_runtime::{
                document_full_snapshot_frontier, DocumentLeafFingerprint,
            };
            for (revision, replayable) in [
                (PARSER_REVISION, true),
                ("opencode-family-source-backed-v14-proven-v2-overlap", false),
            ] {
                let certificate = &expected.0;
                let base = CertifiedSource::certify_with_frontier(
                    certificate.observation().clone(),
                    certificate.observation().clone(),
                    revision,
                    *certificate.content_digest(),
                    certificate.counts(),
                    Some(
                        document_full_snapshot_frontier(
                            DocumentLeafFingerprint::new([7; 32]),
                            certificate.counts().certified_bytes,
                            *certificate.content_digest(),
                        )
                        .unwrap(),
                    ),
                )
                .unwrap();
                assert_eq!(
                    super::super::adapter::exact_replay_base(
                        &[base],
                        crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT.provider,
                        SourceAnchorScope::Unqualified
                    )
                    .is_some(),
                    replayable
                );
            }
        }
        assert_eq!(expected.2.len(), 2);
        assert!(expected
            .2
            .iter()
            .any(|row| row.2.contains("retained-content size limit")));
        assert!(expected
            .2
            .iter()
            .any(|row| row.2.contains("unsupported SQLite storage class")));
        assert_eq!(capture(bounded), expected, "{table}, mode={mode}");
    }
}

fn assert_indexed_payload_rejections(index: &str, oversized_rows: usize) {
    let temp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let database = temp.path().join("opencode.db");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type":"text","text":"valid sibling"}),
    );
    connection.execute_batch(index).unwrap();
    for i in 0..oversized_rows {
        let payload = json!({"type":"text","text":format!("{i}{}", "x".repeat(16 * 1024 * 1024))})
            .to_string();
        connection
            .execute(
                "INSERT INTO part(id,message_id,session_id,time_created,time_updated,data)
             SELECT ?1,message_id,session_id,time_created+1,time_updated+1,?2 FROM part LIMIT 1",
                rusqlite::params![format!("large-{i}"), payload],
            )
            .unwrap();
    }
    drop(connection);
    let (_, _, records, rejections) =
        scan_current_schema_result_with_rejections(&database, data.path(), 1024 * 1024 * 1024)
            .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(rejections.len(), oversized_rows);
    for rejection in rejections {
        assert!(rejection.detail.contains("retained-content size limit"));
    }
}

#[test]
fn expression_index_retains_record_local_rejection() {
    assert_indexed_payload_rejections(
        "CREATE INDEX part_payload_kind ON part(json_extract(data,'$.type'))",
        1,
    );
}

#[test]
fn unique_payload_index_retains_record_local_rejection() {
    assert_indexed_payload_rejections("CREATE UNIQUE INDEX part_payload_unique ON part(data)", 2);
}

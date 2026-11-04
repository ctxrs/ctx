#![cfg(target_os = "linux")]

use super::*;

fn assert_migrated_overlap_equivalence(oversized: bool, automatic_index: bool) {
    let provider = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let database = provider.path().join("opencode.db");
    let source = Connection::open(&database).unwrap();
    create_minimal_session_table(&source);
    create_message_part_tables(&source);
    create_session_message_table(&source);
    source.execute_batch("INSERT INTO session VALUES('session-1',1,1);
        INSERT INTO message VALUES('message-1','session-1',1,1,'{\"role\":\"user\",\"time\":{\"created\":1}}');
        INSERT INTO part VALUES('part-1','message-1','session-1',1,1,'{\"type\":\"text\",\"text\":\"valid migrated sibling\"}');
        INSERT INTO session_message VALUES('message-1','session-1','user',0,1,1,'{\"text\":\"valid migrated sibling\",\"time\":{\"created\":1}}');
        CREATE INDEX part_message_id_id_idx ON part(message_id,id,data);").unwrap();
    if oversized {
        source
            .execute(
                "INSERT INTO session_message VALUES('large','session-1','user',1,2,2,?1)",
                [json!({"text":"x".repeat(16*1024*1024)}).to_string()],
            )
            .unwrap();
    }
    if automatic_index {
        source
            .execute_batch(
                "DROP INDEX part_message_id_id_idx;
            ALTER TABLE part RENAME TO old_part;
            CREATE TABLE part(id TEXT PRIMARY KEY,message_id TEXT NOT NULL,session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,time_updated INTEGER NOT NULL,data TEXT NOT NULL,
                UNIQUE(message_id,id,data));
            INSERT INTO part SELECT * FROM old_part; DROP TABLE old_part;",
            )
            .unwrap();
    }
    drop(source);
    let capture = |snapshot: SqliteSourceReadSnapshot| {
        let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
        let observation = observe_logical_source(&snapshot, dialect)?;
        let mut records = Vec::new();
        let mut rejections = Vec::new();
        let scan = scan_pinned_source(&database, dialect, &observation, snapshot, &mut |output| {
            match output {
                OpenCodeScanOutput::Document(record) => records.push(record),
                OpenCodeScanOutput::Rejection(row) => {
                    rejections.push((row.line_number, row.class, row.detail))
                }
                _ => (),
            }
            Ok(())
        })?;
        Ok::<_, OpenCodeSourceBackedError>((scan.certificate, records, rejections))
    };
    let retained = retain_root_authorized_source(data.path(), &database).unwrap();
    let full = capture(
        retained
            .sqlite_authority
            .open_stable_snapshot(&retained.database_leaf)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(full.1.len(), 1);
    assert_eq!(full.2.len(), usize::from(oversized));
    eprintln!(
        "overlap oversized={oversized}: full capture retains {} record(s), {} typed rejection(s)",
        full.1.len(),
        full.2.len()
    );
    let snapshot = open_root_authorized_snapshot_retained(data.path(), &database)
        .unwrap()
        .sqlite_snapshot;
    assert!(snapshot.column_bound().is_some());
    let bounded = capture(snapshot).unwrap();
    assert_eq!(full, bounded);
}

#[test]
fn migrated_overlap_payload_covering_index() {
    for automatic_index in [false, true] {
        assert_migrated_overlap_equivalence(false, automatic_index);
    }
}
#[test]
fn migrated_overlap_with_oversized_sibling() {
    for automatic_index in [false, true] {
        assert_migrated_overlap_equivalence(true, automatic_index);
    }
}

#[test]
fn randomized_index_and_collation_equivalence() {
    // Authored cases. The oracle uses tuples of fixture values, not production SQL.
    let mut state = 0x1156_2026_u64;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        state >> 32
    };
    let collations = ["BINARY", "NOCASE", "RTRIM"];
    let mut comparisons = 0;
    for seed in 0..18 {
        let parent_collation = collations[seed % 3];
        let reference_collation = collations[(seed / 3) % 3];
        let provider = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let database = provider.path().join("opencode.db");
        let source = write_current_schema(
            &database,
            provider.path(),
            &json!({"type":"text","text":"seed"}),
        );
        source.execute_batch(&format!(
            "DROP TABLE part; DROP TABLE message;
             CREATE TABLE message(id TEXT COLLATE {parent_collation} PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,time_updated INTEGER NOT NULL,data TEXT NOT NULL);
             CREATE TABLE part(id TEXT PRIMARY KEY,message_id TEXT COLLATE {reference_collation} NOT NULL,
                session_id TEXT NOT NULL,time_created INTEGER NOT NULL,time_updated INTEGER NOT NULL,data TEXT NOT NULL);"
        )).unwrap();
        let mut parents = Vec::new();
        for i in 0..8 {
            let id = if i % 2 == 0 {
                format!("Z{i}")
            } else {
                format!("b{i}")
            };
            let time = 1782259200000_i64 + (next() % 3) as i64;
            source.execute("INSERT INTO message VALUES(?1,'current-session',?2,?2,'{\"role\":\"assistant\"}')", params![id,time]).unwrap();
            parents.push((id, time));
        }
        let mut expected = Vec::new();
        let mut insertions = (0..32).collect::<Vec<_>>();
        for i in (1..insertions.len()).rev() {
            insertions.swap(i, (next() as usize) % (i + 1));
        }
        for i in insertions {
            let (parent, time) = &parents[(next() as usize) % parents.len()];
            let reference = match parent_collation {
                "NOCASE" => parent.to_uppercase(),
                "RTRIM" => format!("{parent}   "),
                _ => parent.clone(),
            };
            let part_time = 1782259200000_i64 + (next() % 3) as i64;
            let id = format!("part-{i:02}");
            source.execute("INSERT INTO part(rowid,id,message_id,session_id,time_created,time_updated,data) VALUES(?1,?2,?3,'current-session',?4,?4,?5)",
                params![i as i64 * 7 - 100,id,reference,part_time,json!({"type":"text","text":format!("record {i}")}).to_string()]).unwrap();
            expected.push((*time, parent.clone(), part_time, id));
        }
        expected.sort();
        let expected_ids = expected
            .into_iter()
            .map(|(_, _, _, id)| Some(TypedKey::utf8(id).unwrap()))
            .collect::<Vec<_>>();
        let oversized = seed < 3;
        if oversized {
            // Two genuine over-bound payloads; both are valid JSON before projection.
            for i in 0..2 {
                source.execute("INSERT INTO part VALUES(?1,?2,'current-session',1782259200000,1782259200000,?3)",
                    params![format!("large-{i}"),parents[0].0,json!({"type":"text","text":format!("{i}{}","x".repeat(16*1024*1024))}).to_string()]).unwrap();
            }
        }
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
                        _ => (),
                    }
                    Ok(())
                })
                .unwrap();
            (scan.certificate, records, rejections)
        };
        let mut baseline = None;
        for indexes in 0..4 {
            source
                .execute_batch(
                    "DROP INDEX IF EXISTS message_session_time_created_id_idx;
                DROP INDEX IF EXISTS part_message_id_id_idx; DROP INDEX IF EXISTS part_session_idx;
                DROP INDEX IF EXISTS payload_kind;",
                )
                .unwrap();
            match indexes {
                0 => (),
                1 => source.execute_batch("CREATE INDEX message_session_time_created_id_idx ON message(session_id,time_created,id);
                     CREATE INDEX part_message_id_id_idx ON part(message_id,id); CREATE INDEX part_session_idx ON part(session_id);").unwrap(),
                2 => source.execute_batch("CREATE INDEX message_session_time_created_id_idx ON message(session_id,time_created,id COLLATE BINARY);
                     CREATE INDEX part_message_id_id_idx ON part(message_id COLLATE BINARY,id,data);").unwrap(),
                3 => source.execute_batch("CREATE INDEX message_session_time_created_id_idx ON message(session_id COLLATE NOCASE,time_created DESC,id COLLATE RTRIM);
                     CREATE INDEX part_message_id_id_idx ON part(message_id COLLATE RTRIM,id DESC,data);
                     CREATE INDEX payload_kind ON part(json_extract(data,'$.type')) WHERE data<>'';").unwrap(),
                _ => unreachable!(),
            }
            let retained = retain_root_authorized_source(data.path(), &database).unwrap();
            let full = capture(
                retained
                    .sqlite_authority
                    .open_stable_snapshot(&retained.database_leaf)
                    .unwrap(),
            );
            let bounded = open_root_authorized_snapshot_retained(data.path(), &database)
                .unwrap()
                .sqlite_snapshot;
            assert!(bounded.column_bound().is_some());
            let bounded = capture(bounded);
            assert_eq!(full, bounded, "full/bounded: seed={seed} indexes={indexes}");
            assert_eq!(
                full.1
                    .iter()
                    .map(|r| r.native_event_id.clone())
                    .collect::<Vec<_>>(),
                expected_ids,
                "oracle: seed={seed} indexes={indexes}"
            );
            assert_eq!(full.2.len(), if oversized { 2 } else { 0 });
            if let Some(ref baseline) = baseline {
                assert_eq!(
                    &full, baseline,
                    "index independence: seed={seed} indexes={indexes}"
                );
            } else {
                baseline = Some(full);
            }
            comparisons += 1;
        }
    }
    eprintln!("randomized equivalence: {comparisons} full/bounded pairs; 18 seeds, all 9 parent/reference collation pairs, 4 index sets, tied times, shuffled sparse rowids, independent order oracle");
}

use chrono::{TimeZone, Utc};
use ctx_history_core::HistoryRecord;
use uuid::Uuid;

use super::*;

const CATALOG_SESSION_ROW_SQL: &str = "INSERT INTO catalog_sessions
     (source_path, provider, source_format, source_root, agent_type,
      file_size_bytes, file_modified_at_ms, cataloged_at_ms)
     VALUES ('/root/a.jsonl', 'codex', 'codex_session_jsonl_tree', '/root',
             'primary', 1, 1, 1)";

fn record(title: &str) -> HistoryRecord {
    HistoryRecord {
        id: Uuid::new_v4(),
        title: title.to_owned(),
        body: "carried control record".to_owned(),
        tags: vec!["agent-history".to_owned()],
        kind: "agent_history".to_owned(),
        workspace: Some("/workspace".to_owned()),
        created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        updated_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
    }
}

#[test]
fn pristine_generation_is_empty() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    assert!(empty_generation(&store.conn).unwrap());
    assert!(store.fresh_provider_projection_eligible().unwrap());
}

#[test]
fn control_history_records_stay_empty_and_are_carried() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let store = Store::open(&target).unwrap();
    let carried = record("codex agent history");
    store.upsert_record(&carried).unwrap();
    assert!(empty_generation(&store.conn).unwrap());
    drop(store);

    let admitted = admit_empty_generation(&target).unwrap().unwrap();

    assert_eq!(admitted.records.len(), 1);
    assert_eq!(admitted.records.len(), 1);
    assert_eq!(admitted.records[0].title, carried.title);
    assert_eq!(admitted.records[0].body, carried.body);
    assert_eq!(admitted.records[0].tags, carried.tags);
    assert_eq!(admitted.records[0].kind, carried.kind);
    assert_eq!(admitted.records[0].workspace, carried.workspace);
}

/// The admission predicate must be strictly stronger than the stage predicate.
///
/// `fresh_provider_projection_eligible` intentionally tolerates catalog rows
/// because it only ever runs against a stage this builder owns. A destination
/// the user already owns must not be replaced on that weaker proof.
#[test]
fn catalog_rows_the_stage_predicate_tolerates_are_not_empty() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let store = Store::open(&target).unwrap();
    store.conn.execute_batch(CATALOG_SESSION_ROW_SQL).unwrap();

    assert!(store.fresh_provider_projection_eligible().unwrap());
    assert!(!empty_generation(&store.conn).unwrap());
    drop(store);
    assert!(admit_empty_generation(&target).unwrap().is_none());
}

/// Every table a pristine Store owns is either documented as rebuildable or
/// scanned for rows. A table added later is scanned by default, so a future
/// projection cannot be discarded by a build that predates it.
#[test]
fn every_pristine_table_is_documented_or_scanned() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let names: Vec<String> = store
        .conn
        .prepare(
            r"SELECT name FROM sqlite_master
              WHERE type = 'table' AND name NOT LIKE 'sqlite\_%' ESCAPE '\'
              ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();

    let rebuildable = names
        .iter()
        .filter(|name| is_rebuildable_table(name))
        .cloned()
        .collect::<Vec<_>>();
    let mut documented = CONTROL_TABLES
        .iter()
        .chain(CARRIED_TABLES.iter())
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    for table in FTS_TABLES {
        documented.push(table.to_owned());
        for suffix in FTS_SHADOW_SUFFIXES {
            documented.push(format!("{table}{suffix}"));
        }
    }
    documented.retain(|name| names.contains(name));
    documented.sort();
    assert_eq!(rebuildable, documented);

    for table in [
        "capture_sources",
        "capture_source_provider_routes",
        "catalog_sessions",
        "sessions",
        "session_edges",
        "runs",
        "events",
        "event_search_lookup",
        "files_touched",
        "sync_cursors",
        "artifacts",
        "summaries",
        "source_import_files",
        "provider_source_locators",
        "projection_journal_chunks",
    ] {
        assert!(names.iter().any(|name| name == table), "{table} must exist");
        assert!(
            !is_rebuildable_table(table),
            "{table} rows must block whole-generation replacement"
        );
    }
}

/// An unrecognized table with rows fails closed, so a future projection cannot
/// be silently discarded by a build that predates it.
#[test]
fn an_unknown_table_with_rows_is_not_empty() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TABLE ctx_future_projection (id INTEGER PRIMARY KEY);
             INSERT INTO ctx_future_projection (id) VALUES (1);",
        )
        .unwrap();

    assert!(!empty_generation(&store.conn).unwrap());
}

#[test]
fn carried_records_beyond_the_bound_decline_admission() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let store = Store::open(&target).unwrap();
    store
        .upsert_records(&[record("first"), record("second")])
        .unwrap();
    drop(store);

    assert!(admit_empty_generation_within(&target, 1).unwrap().is_none());
    assert!(admit_empty_generation_within(&target, 2).unwrap().is_some());
}

/// A destination that cannot be retired declines admission instead of failing
/// a completed build. Windows refuses to unlink a database another handle owns;
/// a read-only parent directory reproduces that refusal on Unix.
#[cfg(unix)]
#[test]
fn an_unretirable_target_declines_admission_and_is_left_intact() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let carried = record("codex agent history");
    let store = Store::open(&target).unwrap();
    store.upsert_record(&carried).unwrap();
    drop(store);
    let identity = Handle::from_path(&target).unwrap();
    let before = std::fs::metadata(&target).unwrap().len();
    let parent = std::fs::metadata(temp.path()).unwrap().permissions().mode();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

    let retirable = prove_target_retirable(&target, &identity);

    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(parent)).unwrap();
    assert!(!retirable.unwrap());
    assert_eq!(Handle::from_path(&target).unwrap(), identity);
    assert_eq!(std::fs::metadata(&target).unwrap().len(), before);
    let preserved = Store::open_read_only(&target).unwrap();
    assert_eq!(
        preserved.get_record(carried.id).unwrap().title,
        carried.title
    );
}

/// The probe restores the exact admitted object and leaves no adjacent name.
#[test]
fn a_retirable_target_survives_the_probe_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let carried = record("codex agent history");
    let store = Store::open(&target).unwrap();
    store.upsert_record(&carried).unwrap();
    drop(store);
    let identity = Handle::from_path(&target).unwrap();

    assert!(prove_target_retirable(&target, &identity).unwrap());

    assert_eq!(Handle::from_path(&target).unwrap(), identity);
    let preserved = Store::open_read_only(&target).unwrap();
    assert_eq!(
        preserved.get_record(carried.id).unwrap().title,
        carried.title
    );
    drop(preserved);
    let adjacent = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.contains(".ctx-native-cold-"))
        })
        .count();
    assert_eq!(adjacent, 0);
}

#[test]
fn an_unopenable_target_declines_admission() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    std::fs::write(&target, b"not-a-database").unwrap();

    assert!(admit_empty_generation(&target).unwrap().is_none());
    assert_eq!(std::fs::read(&target).unwrap(), b"not-a-database");
}

#[test]
fn revalidation_rejects_a_generation_that_gained_content() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let store = Store::open(&target).unwrap();
    drop(store);
    let admitted = admit_empty_generation(&target).unwrap().unwrap();
    revalidate_empty_generation(&target, &admitted.records_digest).unwrap();

    let store = Store::open(&target).unwrap();
    store.conn.execute_batch(CATALOG_SESSION_ROW_SQL).unwrap();
    drop(store);

    assert!(matches!(
        revalidate_empty_generation(&target, &admitted.records_digest),
        Err(StoreError::ColdStoreTargetChanged(path)) if path == target
    ));
}

/// An update that rewrites a carried record in place leaves the id set
/// identical. The digest covers every column, so the admission is still
/// invalidated and the rewrite is never replaced away.
#[test]
fn revalidation_rejects_a_rewritten_carried_record_body() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let carried = record("first");
    let store = Store::open(&target).unwrap();
    store.upsert_record(&carried).unwrap();
    drop(store);
    let admitted = admit_empty_generation(&target).unwrap().unwrap();
    revalidate_empty_generation(&target, &admitted.records_digest).unwrap();

    let store = Store::open(&target).unwrap();
    let mut rewritten = carried.clone();
    rewritten.body = "rewritten in place".to_owned();
    store.upsert_record(&rewritten).unwrap();
    let ids: Vec<String> = store
        .conn
        .prepare("SELECT id FROM history_records")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(ids, vec![carried.id.to_string()], "the id set is unchanged");
    drop(store);

    assert!(matches!(
        revalidate_empty_generation(&target, &admitted.records_digest),
        Err(StoreError::ColdStoreTargetChanged(path)) if path == target
    ));
}

#[test]
fn revalidation_rejects_a_changed_carried_record_set() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let store = Store::open(&target).unwrap();
    store.upsert_record(&record("first")).unwrap();
    drop(store);
    let admitted = admit_empty_generation(&target).unwrap().unwrap();
    revalidate_empty_generation(&target, &admitted.records_digest).unwrap();

    let store = Store::open(&target).unwrap();
    store.upsert_record(&record("second")).unwrap();
    drop(store);

    assert!(matches!(
        revalidate_empty_generation(&target, &admitted.records_digest),
        Err(StoreError::ColdStoreTargetChanged(path)) if path == target
    ));
}

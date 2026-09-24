use super::*;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

fn freed_pages(store: &Store) -> Result<()> {
    store.conn.execute_batch(
        "CREATE TABLE discarded(data BLOB);
             INSERT INTO discarded VALUES(zeroblob(262144));
             DROP TABLE discarded;",
    )?;
    Ok(())
}

#[test]
fn failed_native_publish_reports_restore_failure_and_next_writer_repairs_wal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("repair-wal.db");
    let mut store = Store::create(&path)?;
    store.prepare_native_index_write()?;
    store.conn.execute_batch(
        "CREATE TEMP TRIGGER abort_native_metadata
             BEFORE UPDATE ON metadata
             BEGIN SELECT RAISE(ABORT, 'late native failure'); END;",
    )?;
    let failure = store
        .apply_native(
            "repo",
            vec![FileFacts {
                path: "app.py".into(),
                hash: "hash".into(),
                module: "app".into(),
                nodes: vec![Node {
                    id: "entry".into(),
                    label: "entry".into(),
                    kind: "function".into(),
                    file: "app.py".into(),
                    line: Some(1),
                    end_line: Some(1),
                    qualified_name: Some("entry".into()),
                    binding_key: Some("python:app:entry".into()),
                    metadata: serde_json::json!({}),
                }],
                edges: vec![],
                references: vec![],
                diagnostics: vec![],
            }],
            vec![],
            Coverage::default(),
        )
        .unwrap_err();
    store
        .conn
        .execute_batch("DROP TRIGGER abort_native_metadata")?;

    let reader = Connection::open(&path)?;
    reader.execute_batch("BEGIN")?;
    reader.query_row("SELECT generation FROM metadata", [], |row| {
        row.get::<_, i64>(0)
    })?;
    store.conn.busy_timeout(Duration::from_millis(20))?;
    let error = store
        .finish_native_index_write::<()>(Err(failure))
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("late native failure"), "{message}");
    assert!(
        message.contains("restoring WAL mode also failed"),
        "{message}"
    );
    reader.execute_batch("ROLLBACK")?;
    drop(reader);
    drop(store);

    let repaired = Store::create(&path)?;
    assert_eq!(repaired.stats()?.generation, 0);
    let mode: String = repaired
        .conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    assert_eq!(mode, "wal");
    Ok(())
}

#[test]
fn compaction_lock_failure_and_active_transaction_leave_graph_unchanged() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("locked.db");
    let mut store = Store::create(&path)?;
    store.conn.busy_timeout(Duration::from_millis(20))?;
    freed_pages(&store)?;
    let before = serde_json::to_value(store.snapshot()?)?;
    let other = Connection::open(&path)?;
    other.execute_batch("BEGIN IMMEDIATE")?;
    let error = store.compact().unwrap_err();
    assert!(format!("{error:#}").contains("locked"), "{error:#}");
    assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
    other.execute_batch("ROLLBACK")?;
    store.conn.execute_batch("BEGIN")?;
    let error = store.compact().unwrap_err();
    assert!(format!("{error:#}").contains("active transaction"));
    store.conn.execute_batch("ROLLBACK")?;
    assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
    let report = store.compact()?;
    assert_eq!(report.free_pages_after, 0);
    assert!(report.pages_after < report.pages_before);
    assert!(!report.checkpoint_busy);
    assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
    Ok(())
}

#[test]
fn compaction_reports_a_pinned_wal_reader_without_claiming_disk_shrink() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("reader.db");
    let mut store = Store::create(&path)?;
    store.conn.busy_timeout(Duration::from_millis(20))?;
    store
        .conn
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    let reader = Connection::open(&path)?;
    reader.execute_batch("BEGIN")?;
    let generation: i64 =
        reader.query_row("SELECT generation FROM metadata", [], |row| row.get(0))?;
    let old_pages: i64 = reader.pragma_query_value(None, "page_count", |row| row.get(0))?;
    // Commit pages after the pinned reader's end mark, then vacuum them.
    freed_pages(&store)?;
    let before = serde_json::to_value(store.snapshot()?)?;
    let report = store.compact()?;
    assert!(report.checkpoint_busy);
    assert_eq!(report.free_pages_after, 0);
    assert!(std::fs::metadata(path.with_extension("db-wal"))?.len() > 0);
    assert_eq!(
        reader.query_row("SELECT generation FROM metadata", [], |row| row
            .get::<_, i64>(0))?,
        generation
    );
    assert_eq!(
        reader.pragma_query_value(None, "page_count", |row| row.get::<_, i64>(0))?,
        old_pages
    );
    assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
    reader.execute_batch("COMMIT")?;
    let report = store.compact()?;
    assert!(!report.checkpoint_busy);
    assert_eq!(
        std::fs::metadata(&path)?.len(),
        report.pages_after * report.page_size
    );
    assert_eq!(std::fs::metadata(path.with_extension("db-wal"))?.len(), 0);
    assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
    Ok(())
}

#[test]
fn checkpoint_error_explicitly_reports_that_compaction_already_completed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("checkpoint.db");
    let mut store = Store::create(&path)?;
    freed_pages(&store)?;
    let before = serde_json::to_value(store.snapshot()?)?;
    let free: i64 = store
        .conn
        .pragma_query_value(None, "freelist_count", |row| row.get(0))?;
    assert!(free > 0);
    store
        .conn
        .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Pragma {
                pragma_name: "wal_checkpoint",
                ..
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))?;
    let error = store.compact().unwrap_err();
    assert!(
        format!("{error:#}").contains("compaction completed but checkpoint failed"),
        "{error:#}"
    );
    assert_eq!(
        store
            .conn
            .pragma_query_value(None, "freelist_count", |row| row.get::<_, i64>(0))?,
        0
    );
    assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
    store
        .conn
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
    assert!(!store.compact()?.checkpoint_busy);
    Ok(())
}

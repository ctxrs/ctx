use rusqlite::Connection;
use tempfile::tempdir;

use super::*;
use crate::SCHEMA_VERSION;

/// Rewrites a store into the shape a released v0.25 store has on disk: the
/// previous `user_version` and no record of a provider-projection generation.
fn released_store_shape(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("DROP TABLE IF EXISTS ctx_provider_projection_generation")
        .unwrap();
    conn.execute_batch(&format!(
        "PRAGMA user_version = {};",
        NATIVE_PROVIDER_PROJECTION_SCHEMA_VERSION - 1
    ))
    .unwrap();
}

fn seed_provider_row(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO capture_sources
             (id, kind, provider, machine_id, started_at_ms, fidelity, visibility, sync_state)
         VALUES ('11111111-1111-7111-8111-111111111111', 'provider_import', 'codex', 'machine', 0,
                 'partial', 'local_only', 'local_only')",
        [],
    )
    .unwrap();
}

#[test]
fn fresh_store_is_native_and_records_no_prior_schema_version() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let state = store.provider_projection_state().unwrap().unwrap();

    assert_eq!(state.generation, ProviderProjectionGeneration::Native);
    assert!(!state.generation.requires_rederivation());
    assert_eq!(state.observed_schema_version, 0);
}

#[test]
fn released_store_with_provider_rows_is_superseded_exactly_once() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    drop(store);
    seed_provider_row(&path);
    released_store_shape(&path);

    let store = Store::open(&path).unwrap();
    let state = store.provider_projection_state().unwrap().unwrap();
    assert_eq!(state.generation, ProviderProjectionGeneration::Superseded);
    assert!(state.generation.requires_rederivation());
    assert_eq!(
        state.observed_schema_version,
        NATIVE_PROVIDER_PROJECTION_SCHEMA_VERSION - 1
    );
    drop(store);

    // The store is now migrated. Reopening must not re-evaluate the decision,
    // and must not clear it either: the provider rows are still superseded.
    let reopened = Store::open(&path).unwrap();
    assert_eq!(
        reopened.provider_projection_state().unwrap().unwrap(),
        state
    );
    assert_eq!(
        reopened
            .conn
            .query_row(
                "SELECT COUNT(*) FROM ctx_provider_projection_generation",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn released_store_without_provider_rows_is_native() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    drop(store);
    released_store_shape(&path);

    let store = Store::open(&path).unwrap();

    assert_eq!(
        store
            .provider_projection_state()
            .unwrap()
            .unwrap()
            .generation,
        ProviderProjectionGeneration::Native
    );
}

#[test]
fn store_at_the_native_schema_version_is_native_even_with_provider_rows() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    drop(store);
    seed_provider_row(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE IF EXISTS ctx_provider_projection_generation")
        .unwrap();
    drop(conn);

    let store = Store::open(&path).unwrap();

    assert_eq!(
        store
            .provider_projection_state()
            .unwrap()
            .unwrap()
            .generation,
        ProviderProjectionGeneration::Native
    );
    const { assert!(SCHEMA_VERSION >= NATIVE_PROVIDER_PROJECTION_SCHEMA_VERSION) };
}

#[test]
fn superseded_decision_survives_an_interrupted_migration() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    drop(store);
    seed_provider_row(&path);
    released_store_shape(&path);

    // Record the decision the way `run_migrations` does, then abandon the rest
    // of the migration by leaving `user_version` behind.
    let conn = Connection::open(&path).unwrap();
    record_provider_projection_generation(&conn, NATIVE_PROVIDER_PROJECTION_SCHEMA_VERSION - 1)
        .unwrap();
    drop(conn);

    let store = Store::open(&path).unwrap();

    assert_eq!(
        store
            .provider_projection_state()
            .unwrap()
            .unwrap()
            .generation,
        ProviderProjectionGeneration::Superseded
    );
}

#[test]
fn read_only_store_without_the_marker_reports_unknown() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    drop(store);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE IF EXISTS ctx_provider_projection_generation")
        .unwrap();
    drop(conn);

    let store = Store::open_read_only(&path).unwrap();

    assert_eq!(store.provider_projection_state().unwrap(), None);
}

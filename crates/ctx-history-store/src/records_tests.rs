use chrono::{DateTime, Utc};
use ctx_history_core::HistoryRecord;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use tempfile::tempdir;
use uuid::Uuid;

use crate::{Store, StoreError};

#[derive(Clone, Copy)]
enum DeniedProjectionWrite {
    Insert,
    Delete,
}

fn fixed_time() -> DateTime<Utc> {
    "2026-07-22T00:00:00Z".parse().unwrap()
}

fn record(id: Uuid, title: &str, body: &str) -> HistoryRecord {
    HistoryRecord {
        id,
        title: title.to_owned(),
        body: body.to_owned(),
        tags: vec!["atomicity".to_owned()],
        kind: "task".to_owned(),
        workspace: Some("/workspace".to_owned()),
        created_at: fixed_time(),
        updated_at: fixed_time(),
    }
}

fn deny_projection_write(store: &Store, write: DeniedProjectionWrite, table: &'static str) {
    store.conn.authorizer(Some(move |context: AuthContext<'_>| {
        let denied = match (write, context.action) {
            (DeniedProjectionWrite::Insert, AuthAction::Insert { table_name }) => {
                table_name == table
            }
            (DeniedProjectionWrite::Delete, AuthAction::Delete { table_name }) => {
                table_name == table
            }
            _ => false,
        };
        if denied {
            Authorization::Deny
        } else {
            Authorization::Allow
        }
    }));
}

fn clear_authorizer(store: &Store) {
    store
        .conn
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
}

fn projection_rows(store: &Store, table: &str, record_id: Uuid) -> Vec<Vec<String>> {
    let columns = match table {
        "ctx_history_search" => "title, summary, primary_user_text, context_text, tag_text",
        "ctx_history_search_scriptgram" => "token_text, '', '', '', ''",
        _ => panic!("unsupported projection table"),
    };
    let mut statement = store
        .conn
        .prepare(&format!(
            "SELECT {columns} FROM {table} WHERE record_id = ?1 ORDER BY rowid"
        ))
        .unwrap();
    statement
        .query_map([record_id.to_string()], |row| {
            (0..5)
                .map(|index| row.get::<_, String>(index))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn projection_state(store: &Store, record_id: Uuid) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    (
        projection_rows(store, "ctx_history_search", record_id),
        projection_rows(store, "ctx_history_search_scriptgram", record_id),
    )
}

#[test]
fn insert_record_rolls_back_canonical_row_when_fts_insert_fails() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let value = record(Uuid::new_v4(), "new record", "OAuth認証を追加する");
    deny_projection_write(
        &store,
        DeniedProjectionWrite::Insert,
        "ctx_history_search_scriptgram",
    );

    assert!(matches!(
        store.insert_record(&value),
        Err(StoreError::Sql(_))
    ));
    clear_authorizer(&store);

    assert!(matches!(
        store.get_record(value.id),
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(projection_state(&store, value.id), (Vec::new(), Vec::new()));
}

#[test]
fn upsert_record_rolls_back_canonical_and_fts_rows_on_late_fts_failure() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let id = Uuid::new_v4();
    let initial = record(id, "initial title", "旧認証の検索状態");
    store.insert_record(&initial).unwrap();
    let initial_projection = projection_state(&store, id);
    assert!(!initial_projection.0.is_empty());
    assert!(!initial_projection.1.is_empty());

    let updated = record(id, "updated title", "新認証の検索状態");
    deny_projection_write(
        &store,
        DeniedProjectionWrite::Insert,
        "ctx_history_search_scriptgram",
    );
    assert!(matches!(
        store.upsert_record(&updated),
        Err(StoreError::Sql(_))
    ));
    clear_authorizer(&store);

    assert_eq!(store.get_record(id).unwrap(), initial);
    assert_eq!(projection_state(&store, id), initial_projection);
}

#[test]
fn delete_orphan_record_rolls_back_canonical_row_when_fts_delete_fails() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let value = record(Uuid::new_v4(), "retained title", "削除前の認証状態");
    store.insert_record(&value).unwrap();
    let initial_projection = projection_state(&store, value.id);

    deny_projection_write(&store, DeniedProjectionWrite::Delete, "ctx_history_search");
    assert!(matches!(
        store.delete_orphan_record(value.id),
        Err(StoreError::Sql(_))
    ));
    clear_authorizer(&store);

    assert_eq!(store.get_record(value.id).unwrap(), value);
    assert_eq!(projection_state(&store, value.id), initial_projection);
}

#[test]
fn upsert_records_rolls_back_every_record_when_a_late_fts_write_fails() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let first = record(Uuid::new_v4(), "first record", "plain latin text");
    let second = record(Uuid::new_v4(), "second record", "後続の認証状態");
    deny_projection_write(
        &store,
        DeniedProjectionWrite::Insert,
        "ctx_history_search_scriptgram",
    );

    assert!(matches!(
        store.upsert_records(&[first.clone(), second.clone()]),
        Err(StoreError::Sql(_))
    ));
    clear_authorizer(&store);

    for value in [first, second] {
        assert!(matches!(
            store.get_record(value.id),
            Err(StoreError::NotFound(_))
        ));
        assert_eq!(projection_state(&store, value.id), (Vec::new(), Vec::new()));
    }
}

#[test]
fn failed_upsert_records_rolls_back_its_savepoint_without_poisoning_outer_batch() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let failed = record(Uuid::new_v4(), "failed record", "失敗する認証状態");
    let survivor = record(Uuid::new_v4(), "surviving record", "plain latin survivor");

    store.begin_immediate_batch().unwrap();
    deny_projection_write(
        &store,
        DeniedProjectionWrite::Insert,
        "ctx_history_search_scriptgram",
    );
    assert!(matches!(
        store.upsert_records(std::slice::from_ref(&failed)),
        Err(StoreError::Sql(_))
    ));
    clear_authorizer(&store);
    store.insert_record(&survivor).unwrap();
    store.commit_batch().unwrap();

    assert!(matches!(
        store.get_record(failed.id),
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        projection_state(&store, failed.id),
        (Vec::new(), Vec::new())
    );
    assert_eq!(store.get_record(survivor.id).unwrap(), survivor);
    assert!(!projection_state(&store, survivor.id).0.is_empty());
}

#[test]
fn delete_orphan_record_removes_main_and_scriptgram_projections() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let value = record(Uuid::new_v4(), "deleted record", "削除する認証状態");
    store.insert_record(&value).unwrap();
    let before = projection_state(&store, value.id);
    assert!(!before.0.is_empty());
    assert!(!before.1.is_empty());

    assert!(store.delete_orphan_record(value.id).unwrap());

    assert!(matches!(
        store.get_record(value.id),
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(projection_state(&store, value.id), (Vec::new(), Vec::new()));
}

#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_hermes_rejects_out_of_range_message_timestamp() {
    let temp = tempdir();
    let fixture = write_hermes_smoke_db(&temp);
    let conn = Connection::open(&fixture).unwrap();
    conn.execute(
        "update messages set timestamp = ?1 where content = 'bad timestamp'",
        [1.0e300_f64],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_hermes_sqlite(
        &fixture,
        &mut store,
        HermesSqliteImportOptions {
            allow_partial_failures: true,
            ..HermesSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("Hermes message timestamp"));
    assert_eq!(summary.imported_events, 1);
}

pub(crate) fn write_hermes_smoke_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("hermes-state.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            source text not null,
            started_at real not null
        );
        create table messages (
            id integer primary key autoincrement,
            session_id text not null,
            role text not null,
            content text,
            timestamp real not null,
            active integer not null default 1,
            compacted integer not null default 0
        );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, 'acp', 1782259200.0)",
        ["hermes-root"],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'user', 'bad timestamp', 1782259201.0)",
        ["hermes-root"],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'assistant', 'good timestamp', 1782259202.0)",
        ["hermes-root"],
    )
    .unwrap();
    path
}

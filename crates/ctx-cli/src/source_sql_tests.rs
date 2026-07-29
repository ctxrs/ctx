use ctx_history_core::database_path;
use ctx_history_relational::{
    RawSqlOptions, RawSqlValue, RelationalProjectionStatus, SourceBackedRelationalProjection,
};

use super::{sql_compatibility_path, SqlCompatibility};

#[test]
fn sql_compatibility_opens_only_the_independent_projection() {
    let temp = tempfile::tempdir().unwrap();
    let path = sql_compatibility_path(temp.path());
    let writer = SourceBackedRelationalProjection::open(&path).unwrap();
    assert_eq!(
        writer.metadata().unwrap().status,
        RelationalProjectionStatus::Empty
    );
    drop(writer);

    let reader = SqlCompatibility::open(&path).unwrap();
    assert_eq!(
        reader.metadata().unwrap().status,
        RelationalProjectionStatus::Empty
    );
    let result = reader
        .query(
            "SELECT COUNT(*) AS sessions FROM ctx_sessions",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(result.columns[0].name, "sessions");
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(0));
    assert!(reader
        .query(
            "DELETE FROM source_backed_relational_state",
            RawSqlOptions::default()
        )
        .is_err());
}

#[test]
fn fresh_sql_compatibility_initializes_only_the_relational_projection() {
    let temp = tempfile::tempdir().unwrap();
    let reader = SqlCompatibility::open_for_data_root(temp.path()).unwrap();

    assert!(sql_compatibility_path(temp.path()).is_file());
    assert!(!database_path(temp.path().to_path_buf()).exists());
    assert_eq!(
        reader
            .query("SELECT 1 AS one", RawSqlOptions::default())
            .unwrap()
            .rows[0][0],
        RawSqlValue::Integer(1)
    );
}

#[test]
fn committed_source_generation_without_relational_projection_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let generation_root = temp.path().join("search").join("lexical");
    std::fs::create_dir_all(&generation_root).unwrap();
    std::fs::write(generation_root.join("meta.json"), b"committed").unwrap();

    let error = SqlCompatibility::open_for_data_root(temp.path())
        .err()
        .expect("missing relational projection should fail");
    assert!(error
        .to_string()
        .contains("source-backed SQL projection is missing"));
    assert!(!database_path(temp.path().to_path_buf()).exists());
    assert!(!sql_compatibility_path(temp.path()).exists());
}

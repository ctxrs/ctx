use ctx_history_store::{
    RawSqlOptions, RawSqlValue, RelationalProjectionStatus, SourceBackedRelationalProjection,
};

use crate::{sql_compatibility_path, SqlCompatibility};

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
    assert_eq!(reader.path(), path);
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

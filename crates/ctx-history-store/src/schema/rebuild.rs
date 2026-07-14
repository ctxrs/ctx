use rusqlite::Connection;

use crate::schema::ddl::CREATE_TABLES_SQL;
use crate::Result;

pub(crate) fn create_table_rebuild_sql(table: &str, new_table: &str) -> Result<String> {
    let marker = format!("CREATE TABLE IF NOT EXISTS {table}");
    let start = CREATE_TABLES_SQL
        .find(&marker)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    let rest = &CREATE_TABLES_SQL[start..];
    let end = rest.find("\n);").ok_or(rusqlite::Error::InvalidQuery)? + 3;
    Ok(rest[..end].replacen(&marker, &format!("CREATE TABLE {new_table}"), 1))
}

pub(crate) fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(columns)
}

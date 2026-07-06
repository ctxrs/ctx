#[allow(unused_imports)]
use super::*;

pub(crate) fn drop_fts_table_if_column_exists(
    conn: &Connection,
    table: &str,
    column: &str,
) -> Result<()> {
    if table_exists(conn, table)? && table_has_column(conn, table, column)? {
        conn.execute(&format!("DROP TABLE {table}"), [])?;
    }
    Ok(())
}

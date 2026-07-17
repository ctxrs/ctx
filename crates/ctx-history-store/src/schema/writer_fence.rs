use rusqlite::{functions::FunctionFlags, Connection};

use crate::{Result, SCHEMA_VERSION};

const SCHEMA_WRITER_VERSION_FUNCTION: &str = "ctx_schema_writer_version";
const FENCED_PROVIDER_TABLES: &[&str] = &[
    "catalog_sessions",
    "source_import_files",
    "capture_sources",
    "sessions",
    "events",
];

pub(crate) fn register_schema_writer_version(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        SCHEMA_WRITER_VERSION_FUNCTION,
        0,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |_| Ok(SCHEMA_VERSION),
    )?;
    Ok(())
}

pub(crate) fn install_schema_writer_fence(conn: &Connection, minimum_version: i64) -> Result<()> {
    for table in FENCED_PROVIDER_TABLES {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            let trigger = format!(
                "CREATE TRIGGER IF NOT EXISTS ctx_writer_fence_{table}_{} \
                 BEFORE {operation} ON {table} BEGIN \
                   SELECT RAISE(ABORT, 'ctx writer is older than the migrated schema') \
                   WHERE {SCHEMA_WRITER_VERSION_FUNCTION}() < {minimum_version}; \
                 END;",
                operation.to_ascii_lowercase(),
            );
            conn.execute_batch(&trigger)?;
        }
    }
    Ok(())
}

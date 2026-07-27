use rusqlite::Connection;

use crate::Result;

/// Runs one destructive search-projection rebuild as a single publication.
/// A caller-owned canonical transaction gets a savepoint; standalone rebuilds
/// own an immediate transaction.
pub(super) fn run(conn: &Connection, rebuild: impl FnOnce() -> Result<()>) -> Result<()> {
    let owns_transaction = conn.is_autocommit();
    if owns_transaction {
        conn.execute_batch("BEGIN IMMEDIATE")?;
    } else {
        conn.execute_batch("SAVEPOINT ctx_search_projection_rebuild")?;
    }
    let result = rebuild();
    if owns_transaction {
        return match result {
            Ok(()) => {
                if let Err(error) = conn.execute_batch("COMMIT") {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(error.into())
                } else {
                    Ok(())
                }
            }
            Err(error) => {
                if let Err(rollback_error) = conn.execute_batch("ROLLBACK") {
                    return Err(rollback_error.into());
                }
                Err(error)
            }
        };
    }
    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE SAVEPOINT ctx_search_projection_rebuild")?;
            Ok(())
        }
        Err(error) => {
            conn.execute_batch(
                "ROLLBACK TO SAVEPOINT ctx_search_projection_rebuild;
                 RELEASE SAVEPOINT ctx_search_projection_rebuild;",
            )?;
            Err(error)
        }
    }
}

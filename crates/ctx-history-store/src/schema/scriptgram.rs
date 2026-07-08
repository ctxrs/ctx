use rusqlite::Connection;

use crate::schema::fts::{create_fts_tables_if_supported, drop_fts_table_if_exists};
use crate::search::projections::rebuild_search_projection;
use crate::{Result, StoreError};

pub(crate) fn migrate_to_v45(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        drop_fts_table_if_exists(conn, "ctx_history_search_scriptgram")?;
        drop_fts_table_if_exists(conn, "event_search_scriptgram")?;
        create_fts_tables_if_supported(conn)?;
        rebuild_search_projection(conn)?;
        conn.execute_batch("PRAGMA user_version = 45;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

use rusqlite::Connection;

use crate::schema::ddl::{create_event_search_lookup_table, table_exists, table_has_column};
use crate::schema::fts::{create_fts_tables_if_supported, drop_fts_table_if_exists};
use crate::schema::indexes::INDEXES_SQL;
use crate::search::projections::mark_search_projection_rebuild_required;
use crate::{Result, StoreError};

pub(crate) fn migrate_to_v45(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        mark_search_projection_rebuild_required(conn)?;
        drop_fts_table_if_exists(conn, "ctx_history_search_scriptgram")?;
        drop_fts_table_if_exists(conn, "event_search_scriptgram")?;
        create_fts_tables_if_supported(conn)?;
        if table_exists(conn, "event_search_lookup")?
            && !table_has_column(conn, "event_search_lookup", "history_record_id")?
        {
            conn.execute_batch("DROP TABLE event_search_lookup;")?;
        }
        create_event_search_lookup_table(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
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

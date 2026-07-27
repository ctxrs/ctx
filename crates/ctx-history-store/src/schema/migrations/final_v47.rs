//! Same-version v47 identity transitions for unreleased local schema revisions.

use rusqlite::{Connection, OptionalExtension};

use crate::schema::ddl::table_exists;
use crate::schema::rebuild::sanitize_v44_result_event_payloads;
use crate::search::projections::rebuild_search_projection;
use crate::{Result, StoreError, FINAL_SCHEMA_IDENTITY};

const PRE_SOURCE_BACKED_FINAL_SCHEMA_IDENTITY: &str = "ctx-store-schema-47-final-v2";
const PRE_VERIFIED_CONTENT_FINAL_SCHEMA_IDENTITY: &str = "ctx-store-schema-47-final-v3";
const PRE_SOURCE_ROUTE_FINAL_SCHEMA_IDENTITY: &str = "ctx-store-schema-47-final-v4";

pub(super) fn migrate_final_v47_provider_source_routes(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "ctx_store_schema_identity")? {
        return Ok(());
    }
    let identity = conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity
             WHERE singleton = 1 AND schema_version = 47",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if identity.as_deref() != Some(PRE_SOURCE_ROUTE_FINAL_SCHEMA_IDENTITY) {
        return Ok(());
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS capture_source_provider_routes (
                 capture_source_id TEXT PRIMARY KEY NOT NULL
                     REFERENCES capture_sources(id) ON DELETE CASCADE,
                 provider TEXT NOT NULL,
                 source_format TEXT NOT NULL,
                 machine_id TEXT NOT NULL,
                 alias_group_identity TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_capture_source_provider_routes_alias
             ON capture_source_provider_routes(
                 provider, source_format, machine_id, alias_group_identity
             );",
        )?;
        // The backfill is deliberately source-read-free. Only one exact stored
        // path-to-alias-group match is authoritative enough to bind; missing or
        // ambiguous legacy rows remain unbound until a normal provider import.
        conn.execute(
            "WITH candidates AS (
                 SELECT cs.id AS capture_source_id, cs.provider, cs.source_format,
                        cs.machine_id,
                        MIN(locator.alias_group_identity) AS alias_group_identity,
                        COUNT(DISTINCT locator.alias_group_identity) AS match_count
                 FROM capture_sources cs
                 JOIN provider_source_locators locator
                   ON locator.provider = cs.provider
                  AND locator.source_format = cs.source_format
                  AND locator.machine_id = cs.machine_id
                  AND locator.canonical_source_identity = cs.source_identity
                  AND locator.raw_source_path = cs.raw_source_path
                 WHERE cs.source_format IS NOT NULL
                   AND cs.source_identity IS NOT NULL
                   AND cs.raw_source_path IS NOT NULL
                 GROUP BY cs.id, cs.provider, cs.source_format, cs.machine_id
             )
             INSERT INTO capture_source_provider_routes
                 (capture_source_id, provider, source_format, machine_id,
                  alias_group_identity)
             SELECT capture_source_id, provider, source_format, machine_id,
                    alias_group_identity
             FROM candidates WHERE match_count = 1
             ON CONFLICT(capture_source_id) DO NOTHING",
            [],
        )?;
        let updated = conn.execute(
            "UPDATE ctx_store_schema_identity SET schema_identity = ?1
             WHERE singleton = 1 AND schema_version = 47 AND schema_identity = ?2",
            [
                FINAL_SCHEMA_IDENTITY,
                PRE_SOURCE_ROUTE_FINAL_SCHEMA_IDENTITY,
            ],
        )?;
        if updated != 1 {
            return Err(StoreError::UnsupportedSchemaIdentity(
                identity.unwrap_or_else(|| "missing".to_owned()),
            ));
        }
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            if let Err(rollback_error) = conn.execute_batch("ROLLBACK") {
                return Err(StoreError::Sql(rollback_error));
            }
            Err(error)
        }
    }
}

pub(super) fn migrate_final_v47_verified_content_locators(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "ctx_store_schema_identity")? {
        return Ok(());
    }
    let identity = conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity
             WHERE singleton = 1 AND schema_version = 47",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if identity.as_deref() != Some(PRE_VERIFIED_CONTENT_FINAL_SCHEMA_IDENTITY) {
        return Ok(());
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        conn.execute(
            "UPDATE events
             SET metadata_json = json_remove(
                 metadata_json,
                 '$.complete_content_locator_v1',
                 '$.result_content_locator_v1',
                 '$.complete_content_body_sha256'
             )
             WHERE json_type(metadata_json, '$.complete_content_locator_v1') IS NOT NULL
                OR json_type(metadata_json, '$.result_content_locator_v1') IS NOT NULL
                OR json_type(metadata_json, '$.complete_content_body_sha256') IS NOT NULL",
            [],
        )?;
        let updated = conn.execute(
            "UPDATE ctx_store_schema_identity SET schema_identity = ?1
             WHERE singleton = 1 AND schema_version = 47 AND schema_identity = ?2",
            [
                PRE_SOURCE_ROUTE_FINAL_SCHEMA_IDENTITY,
                PRE_VERIFIED_CONTENT_FINAL_SCHEMA_IDENTITY,
            ],
        )?;
        if updated != 1 {
            return Err(StoreError::UnsupportedSchemaIdentity(
                identity.unwrap_or_else(|| "missing".to_owned()),
            ));
        }
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            if let Err(rollback_error) = conn.execute_batch("ROLLBACK") {
                return Err(StoreError::Sql(rollback_error));
            }
            Err(error)
        }
    }
}

pub(super) fn migrate_final_v47_source_backed_results(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "ctx_store_schema_identity")? {
        return Ok(());
    }
    let identity = conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity
             WHERE singleton = 1 AND schema_version = 47",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if identity.as_deref() != Some(PRE_SOURCE_BACKED_FINAL_SCHEMA_IDENTITY) {
        return Ok(());
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        crate::projection_journal::reset_for_canonical_schema_rewrite(conn)?;
        sanitize_v44_result_event_payloads(conn)?;
        rebuild_search_projection(conn)?;
        let updated = conn.execute(
            "UPDATE ctx_store_schema_identity SET schema_identity = ?1
             WHERE singleton = 1 AND schema_version = 47 AND schema_identity = ?2",
            [
                PRE_VERIFIED_CONTENT_FINAL_SCHEMA_IDENTITY,
                PRE_SOURCE_BACKED_FINAL_SCHEMA_IDENTITY,
            ],
        )?;
        if updated != 1 {
            return Err(StoreError::UnsupportedSchemaIdentity(
                identity.unwrap_or_else(|| "missing".to_owned()),
            ));
        }
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            if let Err(rollback_error) = conn.execute_batch("ROLLBACK") {
                return Err(StoreError::Sql(rollback_error));
            }
            Err(error)
        }
    }
}

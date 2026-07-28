use rusqlite::{Connection, OptionalExtension};

use crate::{schema::ddl::table_exists, Result, Store};

/// First on-disk schema version whose provider-derived canonical rows are
/// projected with the current NativePath capture-source identity.
///
/// A store that reached this binary while carrying an older `user_version`
/// projected its provider rows under the previous identity, so the identities
/// this binary derives cannot address those rows. This constant is a fixed
/// historical fact about released stores; it is deliberately independent of
/// [`crate::SCHEMA_VERSION`], which keeps moving.
pub const NATIVE_PROVIDER_PROJECTION_SCHEMA_VERSION: i64 = 47;

const PROVIDER_PROJECTION_TABLE: &str = "ctx_provider_projection_generation";

const CREATE_PROVIDER_PROJECTION_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS ctx_provider_projection_generation (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    generation INTEGER NOT NULL CHECK (generation IN (0, 1)),
    observed_schema_version INTEGER NOT NULL CHECK (observed_schema_version >= 0)
);
"#;

/// Tables whose rows are provider-derived canonical projection output.
///
/// Any row here proves that a provider projection already ran under the
/// schema version being observed. Catalog rows and their owning history record
/// are intentionally excluded: the import lifecycle writes those control rows
/// before projecting provider content, so they do not prove a projection.
const PROVIDER_PROJECTION_TABLES: [&str; 6] = [
    "capture_sources",
    "sessions",
    "session_edges",
    "runs",
    "events",
    "files_touched",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderProjectionGeneration {
    /// The provider rows predate the current NativePath capture-source
    /// identity. This binary cannot address them, so re-importing appends a
    /// second copy of the same provider content instead of reconciling.
    Superseded,
    /// The provider rows were projected by this identity, or there are none.
    Native,
}

impl ProviderProjectionGeneration {
    const fn code(self) -> i64 {
        match self {
            Self::Superseded => 0,
            Self::Native => 1,
        }
    }

    const fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::Superseded),
            1 => Some(Self::Native),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Superseded => "superseded",
            Self::Native => "native",
        }
    }

    pub const fn requires_rederivation(self) -> bool {
        matches!(self, Self::Superseded)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderProjectionState {
    pub generation: ProviderProjectionGeneration,
    /// The `user_version` this store carried when the generation was first
    /// observed. Zero for a store this binary created.
    pub observed_schema_version: i64,
}

/// Records, exactly once per store, which provider-projection generation the
/// store carried when a binary that understands the current NativePath
/// identity first opened it.
///
/// Called before any migration mutates the store, so `user_version` is still
/// the version the previous binary left behind. The decision is durable: once
/// the row exists it is never re-evaluated, which is what makes the
/// re-derivation fire exactly once for an upgraded store and never for a fresh
/// one. A store this binary creates enters at `user_version = 0` with no
/// provider rows and is recorded as native, so fresh installs do no extra work.
pub(crate) fn record_provider_projection_generation(
    conn: &Connection,
    user_version: i64,
) -> Result<()> {
    conn.execute_batch(CREATE_PROVIDER_PROJECTION_TABLE_SQL)?;
    if read_state(conn)?.is_some() {
        return Ok(());
    }
    let generation = if user_version >= NATIVE_PROVIDER_PROJECTION_SCHEMA_VERSION
        || !provider_projection_present(conn)?
    {
        ProviderProjectionGeneration::Native
    } else {
        ProviderProjectionGeneration::Superseded
    };
    conn.execute(
        "INSERT OR IGNORE INTO ctx_provider_projection_generation
             (singleton, generation, observed_schema_version)
         VALUES (1, ?1, ?2)",
        [generation.code(), user_version.max(0)],
    )?;
    Ok(())
}

fn provider_projection_present(conn: &Connection) -> Result<bool> {
    for table in PROVIDER_PROJECTION_TABLES {
        if !table_exists(conn, table)? {
            continue;
        }
        let present = conn
            .query_row(&format!("SELECT 1 FROM {table} LIMIT 1"), [], |_| Ok(()))
            .optional()?
            .is_some();
        if present {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_state(conn: &Connection) -> Result<Option<ProviderProjectionState>> {
    if !table_exists(conn, PROVIDER_PROJECTION_TABLE)? {
        return Ok(None);
    }
    let row = conn
        .query_row(
            "SELECT generation, observed_schema_version
             FROM ctx_provider_projection_generation
             WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(generation, observed_schema_version)| {
        ProviderProjectionGeneration::from_code(generation).map(|generation| {
            ProviderProjectionState {
                generation,
                observed_schema_version,
            }
        })
    }))
}

impl Store {
    /// Provider-projection generation this store carries.
    ///
    /// `None` only for a store no writable open has reached yet, which cannot
    /// be interpreted either way and must not be treated as native.
    pub fn provider_projection_state(&self) -> Result<Option<ProviderProjectionState>> {
        read_state(&self.conn)
    }
}

#[cfg(test)]
#[path = "provider_projection/tests.rs"]
mod tests;

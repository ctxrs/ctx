//! Read-only SQL compatibility surface over the independent relational
//! projection.
//!
//! Command/MCP integration should open this reader at the relational
//! projection path, not at the legacy canonical Store path. The writer is
//! advanced only after a certified Core commit through
//! `SourceBackedRelationalProjection::catch_up`; therefore this reader may
//! report an older `active_core_generation_id` or `Behind` status without
//! changing Core search success.

use std::path::{Path, PathBuf};

use ctx_history_store::{
    RawSqlOptions, RawSqlResult, RelationalProjectionError, RelationalProjectionMetadata,
    SourceBackedRelationalProjection,
};

pub type SqlCompatibilityResult<T> = std::result::Result<T, RelationalProjectionError>;

/// A read-only handle to current stable `ctx_*` views and projection metadata.
pub struct SqlCompatibility {
    projection: SourceBackedRelationalProjection,
}

impl SqlCompatibility {
    pub fn open(path: impl AsRef<Path>) -> SqlCompatibilityResult<Self> {
        Ok(Self {
            projection: SourceBackedRelationalProjection::open_read_only(path)?,
        })
    }

    pub fn path(&self) -> &Path {
        self.projection.path()
    }

    pub fn metadata(&self) -> SqlCompatibilityResult<RelationalProjectionMetadata> {
        self.projection.metadata()
    }

    pub fn query(&self, sql: &str, options: RawSqlOptions) -> SqlCompatibilityResult<RawSqlResult> {
        self.projection.raw_sql_query(sql, options)
    }
}

/// Default filename for the source-backed SQL compatibility consumer.
///
/// The caller chooses the owning data root; this helper does not initialize,
/// refresh, or migrate either Core or the relational projection.
pub fn sql_compatibility_path(data_root: impl AsRef<Path>) -> PathBuf {
    data_root.as_ref().join("relational.sqlite")
}

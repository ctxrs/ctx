//! Read-only SQL compatibility surface over the independent relational
//! projection.
//!
//! Command/MCP integration should open this reader at the relational
//! projection path, not at the legacy canonical Store path. The writer is
//! advanced only after a certified Core commit through
//! `SourceBackedRelationalProjection::catch_up`. Query admission fails closed
//! while that projection is absent, behind, or bound to a different Core
//! generation; Core search success remains independent.

use std::path::{Path, PathBuf};

use ctx_history_index::VerifiedIndex;
use ctx_history_store::{
    RawSqlOptions, RawSqlResult, RelationalProjectionError, RelationalProjectionMetadata,
    RelationalProjectionStatus, SourceBackedRelationalProjection,
};

pub type SqlCompatibilityResult<T> = std::result::Result<T, RelationalProjectionError>;

const SOURCE_BACKED_INDEX_DIRECTORY: &str = "source-backed-lexical-v0";

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

    /// Selects SQL authority for one ctx data root.
    ///
    /// The source-backed projection always wins. A committed source generation
    /// without its relational consumer fails closed instead of falling back to
    /// stale canonical rows. A completely fresh root initializes only the
    /// disposable relational schema. `work.sqlite` is never inspected: v0.26
    /// source-backed history is a new data epoch and old Store rows remain
    /// runtime-inactive.
    pub fn open_for_data_root(data_root: impl AsRef<Path>) -> SqlCompatibilityResult<Self> {
        let data_root = data_root.as_ref();
        let projection_path = sql_compatibility_path(data_root);
        let generation_path = data_root.join(SOURCE_BACKED_INDEX_DIRECTORY);
        if projection_path.try_exists()? {
            let compatibility = Self::open(projection_path)?;
            if generation_path.join("meta.json").try_exists()? {
                let index = VerifiedIndex::open(&generation_path).map_err(|error| {
                    RelationalProjectionError::InvalidCoreGeneration(error.to_string())
                })?;
                let metadata = compatibility.metadata()?;
                if metadata.status != RelationalProjectionStatus::Ready
                    || metadata.active_core_generation_id.as_deref() != Some(index.generation_id())
                {
                    return Err(
                        RelationalProjectionError::SourceBackedSqlGenerationMismatch {
                            expected_generation: index.generation_id().to_owned(),
                            active_generation: metadata.active_core_generation_id,
                            status: match metadata.status {
                                RelationalProjectionStatus::Empty => "empty",
                                RelationalProjectionStatus::Ready => "ready",
                                RelationalProjectionStatus::Behind => "behind",
                            }
                            .to_owned(),
                        },
                    );
                }
            }
            return Ok(compatibility);
        }

        if generation_path.join("meta.json").try_exists()? {
            return Err(
                RelationalProjectionError::MissingSourceBackedSqlProjection {
                    projection_path,
                    generation_path,
                },
            );
        }

        let writer = SourceBackedRelationalProjection::open(&projection_path)?;
        drop(writer);
        Self::open(projection_path)
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

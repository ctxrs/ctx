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
use ctx_history_relational::{
    RawSqlOptions, RawSqlResult, RelationalProjectionError, RelationalProjectionMetadata,
    RelationalProjectionStatus, SourceBackedRelationalProjection,
};

pub type SqlCompatibilityResult<T> = std::result::Result<T, RelationalProjectionError>;

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";

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

    /// Opens the existing SQL authority for one ctx data root.
    ///
    /// This path never initializes the data root or the disposable relational
    /// projection. It is intended for inspection surfaces such as MCP that
    /// promise to query existing state only.
    pub fn open_existing_for_data_root(
        data_root: impl AsRef<Path>,
    ) -> SqlCompatibilityResult<Self> {
        let data_root = data_root.as_ref();
        let projection_path = sql_compatibility_path(data_root);
        let generation_path = source_generation_path(data_root);
        if !projection_path.try_exists()? {
            return Err(
                RelationalProjectionError::MissingSourceBackedSqlProjection {
                    projection_path,
                    generation_path,
                },
            );
        }
        Self::open_existing_projection(projection_path, generation_path)
    }

    /// Selects SQL authority for one ctx data root.
    ///
    /// The source-backed projection always wins. A committed source generation
    /// without its relational consumer fails closed instead of falling back to
    /// stale canonical rows. A completely fresh root initializes only the
    /// disposable relational schema. `work.sqlite` is never inspected: 1.0
    /// source-backed history is a new data epoch, and verified activation
    /// retires obsolete Store leaves instead of preserving a fallback.
    pub fn open_for_data_root(data_root: impl AsRef<Path>) -> SqlCompatibilityResult<Self> {
        let data_root = data_root.as_ref();
        let projection_path = sql_compatibility_path(data_root);
        let generation_path = source_generation_path(data_root);
        if projection_path.try_exists()? {
            return Self::open_existing_projection(projection_path, generation_path);
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

    fn open_existing_projection(
        projection_path: PathBuf,
        generation_path: PathBuf,
    ) -> SqlCompatibilityResult<Self> {
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
        Ok(compatibility)
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

fn source_generation_path(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}

#[cfg(test)]
#[path = "source_sql_tests.rs"]
mod tests;

#[cfg(test)]
mod existing_only_tests {
    use ctx_history_relational::{
        RawSqlOptions, RawSqlValue, RelationalProjectionError, SourceBackedRelationalProjection,
    };

    use super::{sql_compatibility_path, SqlCompatibility};

    #[test]
    fn existing_only_open_leaves_a_pristine_root_absent() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("pristine");

        let error = SqlCompatibility::open_existing_for_data_root(&data_root)
            .err()
            .expect("an absent projection must be unavailable");

        assert!(matches!(
            error,
            RelationalProjectionError::MissingSourceBackedSqlProjection { .. }
        ));
        assert!(
            !data_root.exists(),
            "existing-only SQL inspection must not create its data root"
        );
    }

    #[test]
    fn existing_only_open_queries_an_existing_projection_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let projection_path = sql_compatibility_path(temp.path());
        let writer = SourceBackedRelationalProjection::open(&projection_path).unwrap();
        drop(writer);
        let database_before = std::fs::read(&projection_path).unwrap();

        let reader = SqlCompatibility::open_existing_for_data_root(temp.path()).unwrap();
        let result = reader
            .query(
                "SELECT COUNT(*) AS sessions FROM ctx_sessions",
                RawSqlOptions::default(),
            )
            .unwrap();

        assert_eq!(result.rows[0][0], RawSqlValue::Integer(0));
        assert!(reader
            .query(
                "CREATE TABLE forbidden(value INTEGER)",
                RawSqlOptions::default(),
            )
            .is_err());
        assert_eq!(std::fs::read(projection_path).unwrap(), database_before);
    }

    #[test]
    fn existing_only_open_keeps_source_generation_validation() {
        let temp = tempfile::tempdir().unwrap();
        let projection_path = sql_compatibility_path(temp.path());
        let writer = SourceBackedRelationalProjection::open(&projection_path).unwrap();
        drop(writer);
        let generation_path = temp.path().join("search").join("lexical");
        std::fs::create_dir_all(&generation_path).unwrap();
        std::fs::write(generation_path.join("meta.json"), b"invalid generation").unwrap();

        let error = SqlCompatibility::open_existing_for_data_root(temp.path())
            .err()
            .expect("an invalid source generation must fail closed");

        assert!(matches!(
            error,
            RelationalProjectionError::InvalidCoreGeneration(_)
        ));
    }
}

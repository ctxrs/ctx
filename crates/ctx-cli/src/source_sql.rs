//! Read-only SQL compatibility surface over the independent relational
//! projection.
//!
//! Command/MCP integration should open this reader at the relational
//! projection path, not at the legacy canonical Store path. The writer is
//! advanced only after a certified Core commit through
//! `SourceBackedRelationalProjection::catch_up`. Each query pins one SQLite
//! read transaction and may use the latest coherent relational generation
//! while Core is ahead or catch-up has failed. The result reports both
//! frontiers and staleness. Without
//! active Core, only the canonical empty projection is admissible; Core search
//! success remains independent.

use std::path::{Path, PathBuf};

use ctx_history_index::VerifiedIndex;
use ctx_history_relational::{
    RawSqlOptions, RawSqlResult, RawSqlSnapshot, RelationalProjectionError,
    RelationalProjectionMetadata, RelationalProjectionStatus, SourceBackedRelationalProjection,
};

pub type SqlCompatibilityResult<T> = std::result::Result<T, RelationalProjectionError>;

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";

/// A read-only handle to current stable `ctx_*` views and projection metadata.
pub struct SqlCompatibility {
    projection: SourceBackedRelationalProjection,
    snapshot: RawSqlSnapshot,
}

impl SqlCompatibility {
    pub fn open(path: impl AsRef<Path>) -> SqlCompatibilityResult<Self> {
        let projection = SourceBackedRelationalProjection::open_read_only(path)?;
        projection.begin_read_snapshot()?;
        let metadata = projection.metadata()?;
        let observed_core_generation_id = metadata.active_core_generation_id.clone();
        Ok(Self::from_pinned_projection(
            projection,
            metadata,
            observed_core_generation_id,
        ))
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
    /// source-backed history is a new data epoch. Obsolete Store leaves are
    /// inert owner-managed files, never a fallback or an implicit delete target.
    /// An existing projection is accepted without Core only when it still has
    /// the canonical empty receipt created for a fresh root.
    pub fn open_for_data_root(data_root: impl AsRef<Path>) -> SqlCompatibilityResult<Self> {
        let data_root = data_root.as_ref();
        let projection_path = sql_compatibility_path(data_root);
        let generation_path = source_generation_path(data_root);
        if projection_path.try_exists()? {
            return Self::open_existing_projection(projection_path, generation_path);
        }

        if active_core_generation_id(&generation_path)?.is_some() {
            return Err(
                RelationalProjectionError::MissingSourceBackedSqlProjection {
                    projection_path,
                    generation_path,
                },
            );
        }

        let writer = SourceBackedRelationalProjection::open(&projection_path)?;
        drop(writer);
        // Re-enter the existing-projection admission path so Core is observed
        // after the empty relational snapshot is pinned. Core may have been
        // published between the absence check above and projection creation.
        Self::open_existing_projection(projection_path, generation_path)
    }

    fn open_existing_projection(
        projection_path: PathBuf,
        generation_path: PathBuf,
    ) -> SqlCompatibilityResult<Self> {
        let projection = SourceBackedRelationalProjection::open_read_only(projection_path)?;
        projection.begin_read_snapshot()?;
        let metadata = projection.metadata()?;
        // Observe Core only after the SQLite transaction has pinned rows and
        // relational metadata. This prevents a newly published Core pointer
        // from being reported as current for an older, not-yet-pinned read.
        let observed_core_generation_id = active_core_generation_id(&generation_path)?;
        Self::admit_pinned_projection(projection, metadata, observed_core_generation_id)
    }

    #[cfg(test)]
    fn open_existing_projection_for_observed(
        projection_path: PathBuf,
        observed_core_generation_id: Option<String>,
    ) -> SqlCompatibilityResult<Self> {
        let projection = SourceBackedRelationalProjection::open_read_only(projection_path)?;
        projection.begin_read_snapshot()?;
        let metadata = projection.metadata()?;
        Self::admit_pinned_projection(projection, metadata, observed_core_generation_id)
    }

    fn admit_pinned_projection(
        projection: SourceBackedRelationalProjection,
        metadata: RelationalProjectionMetadata,
        observed_core_generation_id: Option<String>,
    ) -> SqlCompatibilityResult<Self> {
        if observed_core_generation_id.is_none() {
            if !is_genuinely_empty_projection(&metadata) {
                return Err(RelationalProjectionError::IncompatibleState(
                    "Core generation is absent but the relational projection is not empty"
                        .to_owned(),
                ));
            }
            return Ok(Self::from_pinned_projection(
                projection,
                metadata,
                observed_core_generation_id,
            ));
        }

        if !matches!(
            metadata.status,
            RelationalProjectionStatus::Ready | RelationalProjectionStatus::Behind
        ) || metadata.active_core_generation_id.is_none()
        {
            return Err(
                RelationalProjectionError::SourceBackedSqlGenerationMismatch {
                    expected_generation: observed_core_generation_id
                        .clone()
                        .unwrap_or_else(|| "unknown".to_owned()),
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
        Ok(Self::from_pinned_projection(
            projection,
            metadata,
            observed_core_generation_id,
        ))
    }

    fn from_pinned_projection(
        projection: SourceBackedRelationalProjection,
        metadata: RelationalProjectionMetadata,
        observed_core_generation_id: Option<String>,
    ) -> Self {
        let stale = match metadata.status {
            RelationalProjectionStatus::Behind => true,
            RelationalProjectionStatus::Ready => {
                metadata.active_core_generation_id != observed_core_generation_id
            }
            RelationalProjectionStatus::Empty => {
                metadata.active_core_generation_id.is_some()
                    || observed_core_generation_id.is_some()
            }
        };
        Self {
            projection,
            snapshot: RawSqlSnapshot {
                relational_core_generation_id: metadata.active_core_generation_id,
                relational_build_generation: metadata.build_generation,
                observed_core_generation_id,
                projection_status: metadata.status,
                stale,
            },
        }
    }

    pub fn metadata(&self) -> SqlCompatibilityResult<RelationalProjectionMetadata> {
        self.projection.metadata()
    }

    pub fn query(&self, sql: &str, options: RawSqlOptions) -> SqlCompatibilityResult<RawSqlResult> {
        let mut result = self.projection.raw_sql_query(sql, options)?;
        result.snapshot = Some(self.snapshot.clone());
        Ok(result)
    }
}

fn active_core_generation_id(path: &Path) -> SqlCompatibilityResult<Option<String>> {
    VerifiedIndex::active_generation_id(path)
        .map_err(|error| RelationalProjectionError::InvalidCoreGeneration(error.to_string()))
}

fn is_genuinely_empty_projection(metadata: &RelationalProjectionMetadata) -> bool {
    metadata.build_generation == 0
        && metadata.active_core_generation_id.is_none()
        && metadata.active_manifest_version.is_none()
        && metadata.active_core_record_version.is_none()
        && metadata.active_core_record_contract_fingerprint.is_none()
        && metadata.active_lexical_schema_version.is_none()
        && metadata.active_policy_schema_hash.is_none()
        && metadata.active_materializer_revision.is_none()
        && metadata.target_core_generation_id.is_none()
        && metadata.status == RelationalProjectionStatus::Empty
        && metadata.source_count == 0
        && metadata.session_count == 0
        && metadata.event_count == 0
        && metadata.repository_binding_count == 0
        && metadata.file_touch_count == 0
        && metadata.vcs_observation_count == 0
        && metadata.last_error.is_none()
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
        std::fs::write(
            generation_path.join("active-generation.json"),
            b"invalid generation",
        )
        .unwrap();

        let error = SqlCompatibility::open_existing_for_data_root(temp.path())
            .err()
            .expect("an invalid source generation must fail closed");

        assert!(matches!(
            error,
            RelationalProjectionError::InvalidCoreGeneration(_)
        ));
    }
}

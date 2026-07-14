const CATALOG_INVENTORY_FAMILY: &str = "catalog_sessions";
const SOURCE_IMPORT_INVENTORY_FAMILY: &str = "source_import_files";
const STAGING_SCHEMA: &str = "provider_replacement_stage";
const STAGING_DIR_PREFIX: &str = "stage";
pub const PROVIDER_FILE_PREPARATION_MAX_ROWS: usize = 100_000;
pub const PROVIDER_FILE_RECONCILIATION_MAX_ROWS: usize = 100_000;
pub const PROVIDER_FILE_CHECKPOINT_RESUME_STATE_MAX_BYTES: usize = 64 * 1024;
const CLEANUP_PHASE_LINKS: i64 = 0;
const CLEANUP_PHASE_FILES: i64 = 1;
const CLEANUP_PHASE_EDGES: i64 = 2;
const CLEANUP_PHASE_SUMMARIES: i64 = 3;
const CLEANUP_PHASE_EVENTS: i64 = 4;
const CLEANUP_PHASE_RUNS: i64 = 5;
const CLEANUP_PHASE_SESSIONS: i64 = 6;
const CLEANUP_PHASE_VCS_CHANGES: i64 = 7;
const CLEANUP_PHASE_ARTIFACTS: i64 = 8;
const CLEANUP_PHASE_HISTORY_RECORD_TAGS: i64 = 9;
const CLEANUP_PHASE_RECORD_EDGES: i64 = 10;
const CLEANUP_PHASE_HISTORY_RECORDS: i64 = 11;
const CLEANUP_PHASE_VCS_WORKSPACES: i64 = 12;
const CLEANUP_PHASE_AUDIT_LOG: i64 = 13;
const CLEANUP_PHASE_COMPLETE: i64 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderFileCheckpointKey<'a> {
    pub provider: CaptureProvider,
    pub source_format: &'a str,
    pub source_root: &'a str,
    pub source_path: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFileCheckpoint {
    pub provider: CaptureProvider,
    pub source_format: String,
    pub source_root: String,
    pub source_path: String,
    pub import_revision: u32,
    pub checkpoint_version: u32,
    pub stable_file_identity: String,
    pub committed_byte_offset: u64,
    pub committed_complete_line_count: u64,
    pub head_sha256: String,
    pub boundary_sha256: String,
    pub resume_state: Option<Vec<u8>>,
    pub updated_at_ms: i64,
}

impl ProviderFileCheckpoint {
    pub fn key(&self) -> ProviderFileCheckpointKey<'_> {
        ProviderFileCheckpointKey {
            provider: self.provider,
            source_format: &self.source_format,
            source_root: &self.source_root,
            source_path: &self.source_path,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ProviderFileInventoryObservation<'a> {
    Catalog {
        source_format: &'a str,
        update: CatalogSourceIndexUpdate<'a>,
    },
    SourceImport {
        source_format: &'a str,
        update: SourceImportFileIndexUpdate<'a>,
    },
}

impl<'a> ProviderFileInventoryObservation<'a> {
    fn source_format(self) -> &'a str {
        match self {
            Self::Catalog { source_format, .. } | Self::SourceImport { source_format, .. } => {
                source_format
            }
        }
    }

    fn source_root(self) -> &'a str {
        match self {
            Self::Catalog { update, .. } => update.source_root,
            Self::SourceImport { update, .. } => update.source_root,
        }
    }

    fn source_path(self) -> &'a str {
        match self {
            Self::Catalog { update, .. } => update.source_path,
            Self::SourceImport { update, .. } => update.source_path,
        }
    }

    fn file_size_bytes(self) -> u64 {
        match self {
            Self::Catalog { update, .. } => update.file_size_bytes,
            Self::SourceImport { update, .. } => update.file_size_bytes,
        }
    }

    fn file_modified_at_ms(self) -> i64 {
        match self {
            Self::Catalog { update, .. } => update.file_modified_at_ms,
            Self::SourceImport { update, .. } => update.file_modified_at_ms,
        }
    }

    fn import_revision(self) -> u32 {
        match self {
            Self::Catalog { update, .. } => update.import_revision,
            Self::SourceImport { update, .. } => update.import_revision,
        }
    }

    fn inventory_generation(self) -> u64 {
        match self {
            Self::Catalog { update, .. } => update.inventory_generation,
            Self::SourceImport { update, .. } => update.inventory_generation,
        }
    }

    fn inventory_family(self) -> &'static str {
        match self {
            Self::Catalog { .. } => CATALOG_INVENTORY_FAMILY,
            Self::SourceImport { .. } => SOURCE_IMPORT_INVENTORY_FAMILY,
        }
    }

    fn metadata_json(self) -> Result<Option<String>> {
        match self {
            Self::Catalog { .. } => Ok(None),
            Self::SourceImport { update, .. } => serde_json::to_string(update.metadata)
                .map(Some)
                .map_err(Into::into),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderFileImportOutcome<'a> {
    pub provider: CaptureProvider,
    pub observation: ProviderFileInventoryObservation<'a>,
    pub status: CatalogIndexedStatus,
    pub error: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFilePublicationKind {
    Incremental,
    Replacement,
}

impl ProviderFilePublicationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Incremental => "incremental",
            Self::Replacement => "replacement",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ProviderFilePublicationCommit<'a> {
    Append(&'a ProviderFileCheckpoint),
    RetainCheckpoint,
    Replacement(Option<&'a ProviderFileCheckpoint>),
}

/// Owns one provider-file publication lease. Replacement owners with prior
/// material are deliberately unavailable to ordinary search/list/hydration
/// until sliced reconciliation and final publication complete. A crash leaves
/// that durable hidden marker in place; a full retry or disappeared-observation
/// retirement is required to resolve it.
#[derive(Debug)]
pub struct ProviderFilePublicationScope {
    scope_id: Uuid,
    store_identity: String,
    provider: CaptureProvider,
    inventory_source_format: String,
    inventory_source_root: String,
    source_path: String,
    material_source_format: String,
    material_source_root: String,
    inventory_family: &'static str,
    inventory_generation: u64,
    file_size_bytes: u64,
    file_modified_at_ms: i64,
    import_revision: u32,
    metadata_json: Option<String>,
    kind: ProviderFilePublicationKind,
    owner_id: String,
    staging_id: String,
    tracks_prior_material: bool,
    retires_observation: bool,
    lifecycle: Arc<AtomicBool>,
    _owner_lock: File,
    _owner_lock_path: PathBuf,
}

impl Drop for ProviderFilePublicationScope {
    fn drop(&mut self) {
        self.lifecycle.store(false, Ordering::Release);
    }
}

impl ProviderFilePublicationScope {
    pub fn kind(&self) -> ProviderFilePublicationKind {
        self.kind
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderFileReconciliationCounts {
    pub artifacts: usize,
    pub summaries: usize,
    pub history_record_links: usize,
    pub history_records: usize,
    pub history_record_tags: usize,
    pub record_edges: usize,
    pub audit_log_entries: usize,
    pub vcs_workspaces: usize,
    pub vcs_changes: usize,
    pub events: usize,
    pub runs: usize,
    pub files_touched: usize,
    pub session_edges: usize,
    pub sessions_tombstoned: usize,
}

impl ProviderFileReconciliationCounts {
    fn checked_add(self, other: Self) -> Result<Self> {
        Ok(Self {
            artifacts: self.artifacts.checked_add(other.artifacts).ok_or(
                StoreError::ProviderFileReconciliationInconsistent {
                    entity: "artifact count",
                },
            )?,
            summaries: self.summaries.checked_add(other.summaries).ok_or(
                StoreError::ProviderFileReconciliationInconsistent {
                    entity: "summary count",
                },
            )?,
            history_record_links: self
                .history_record_links
                .checked_add(other.history_record_links)
                .ok_or(StoreError::ProviderFileReconciliationInconsistent {
                    entity: "history record link count",
                })?,
            history_records: self
                .history_records
                .checked_add(other.history_records)
                .ok_or(StoreError::ProviderFileReconciliationInconsistent {
                    entity: "history record count",
                })?,
            history_record_tags: self
                .history_record_tags
                .checked_add(other.history_record_tags)
                .ok_or(StoreError::ProviderFileReconciliationInconsistent {
                    entity: "history record tag count",
                })?,
            record_edges: self.record_edges.checked_add(other.record_edges).ok_or(
                StoreError::ProviderFileReconciliationInconsistent {
                    entity: "record edge count",
                },
            )?,
            audit_log_entries: self
                .audit_log_entries
                .checked_add(other.audit_log_entries)
                .ok_or(StoreError::ProviderFileReconciliationInconsistent {
                    entity: "audit log count",
                })?,
            vcs_workspaces: self
                .vcs_workspaces
                .checked_add(other.vcs_workspaces)
                .ok_or(StoreError::ProviderFileReconciliationInconsistent {
                    entity: "VCS workspace count",
                })?,
            vcs_changes: self.vcs_changes.checked_add(other.vcs_changes).ok_or(
                StoreError::ProviderFileReconciliationInconsistent {
                    entity: "VCS change count",
                },
            )?,
            events: self.events.checked_add(other.events).ok_or(
                StoreError::ProviderFileReconciliationInconsistent {
                    entity: "event count",
                },
            )?,
            runs: self.runs.checked_add(other.runs).ok_or(
                StoreError::ProviderFileReconciliationInconsistent {
                    entity: "run count",
                },
            )?,
            files_touched: self.files_touched.checked_add(other.files_touched).ok_or(
                StoreError::ProviderFileReconciliationInconsistent {
                    entity: "file count",
                },
            )?,
            session_edges: self.session_edges.checked_add(other.session_edges).ok_or(
                StoreError::ProviderFileReconciliationInconsistent {
                    entity: "edge count",
                },
            )?,
            sessions_tombstoned: self
                .sessions_tombstoned
                .checked_add(other.sessions_tombstoned)
                .ok_or(StoreError::ProviderFileReconciliationInconsistent {
                    entity: "session count",
                })?,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderFileReconciliationProgress {
    pub rows_scanned: usize,
    pub complete: bool,
    pub counts: ProviderFileReconciliationCounts,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderFilePreparationProgress {
    pub source_ids_staged: usize,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderFileMaintenanceWarning {
    StagingCleanupDeferred {
        publication_id: String,
        operation: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFileFinalizeOutcome {
    pub reconciliation: ProviderFileReconciliationCounts,
    pub maintenance_warning: Option<ProviderFileMaintenanceWarning>,
}

pub(crate) struct ActiveProviderFilePublication {
    scope_id: Uuid,
    owner_id: String,
    lifecycle: Arc<AtomicBool>,
    provider: CaptureProvider,
    material_source_format: String,
    material_source_root: String,
    source_path: String,
    retires_observation: bool,
    _owner_lock_path: PathBuf,
    attached: bool,
    staging_dir_path: Option<PathBuf>,
    staging_path: Option<PathBuf>,
    #[cfg(test)]
    staging_file_mode: Option<u32>,
    #[cfg(test)]
    staging_dir_mode: Option<u32>,
}

struct ProviderFileWriteScopeReset<'a> {
    scope: &'a Cell<Option<Uuid>>,
}

impl Drop for ProviderFileWriteScopeReset<'_> {
    fn drop(&mut self) {
        self.scope.set(None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderFileCompletionKind {
    Replacement,
    AppendDelta,
    RetainCheckpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderFileFaultPoint {
    BeginAfterStaging,
    MutationBeforeCommit,
    PreparationBeforeCommit,
    FinalizeBeforeCommit,
    #[cfg(test)]
    RetirementFinalizeProcessExit,
    Cleanup,
}

#[derive(Debug, Clone)]
struct ReplacementMarker {
    publication_kind: ProviderFilePublicationKind,
    mutation_started: bool,
    preparation_complete: bool,
    preparation_cursor: Option<String>,
    cleanup_phase: i64,
    source_cursor: Option<String>,
    entity_cursor: Option<String>,
    counts: ProviderFileReconciliationCounts,
}

struct DurableProviderFilePublication {
    scope_id: Uuid,
    staging_id: String,
    inventory_family: &'static str,
    inventory_source_format: String,
    inventory_source_root: String,
    source_path: String,
    inventory_generation: u64,
    file_size_bytes: u64,
    file_modified_at_ms: i64,
    import_revision: u32,
    metadata_json: Option<String>,
    mutation_started: bool,
}

struct ReconciliationBatch {
    visited: usize,
    phase_complete: bool,
    source_cursor: Option<String>,
    entity_cursor: Option<String>,
    removed: ProviderFileReconciliationCounts,
}

struct ReconciliationScan {
    visited: usize,
    phase_complete: bool,
    batch_source_id: Option<String>,
    source_cursor: Option<String>,
    entity_cursor: Option<String>,
    owned_entity_ids: Vec<String>,
}

struct ReconciliationPhaseSpec {
    owner_select_sql: &'static str,
}

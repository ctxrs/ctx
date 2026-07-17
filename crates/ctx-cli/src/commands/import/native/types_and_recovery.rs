#[derive(Debug, Default)]
pub(crate) struct ProviderFileRetirementRecoveryOutcome {
    pub(crate) completed: bool,
    pub(crate) made_durable_progress: bool,
    pub(crate) maintenance_warnings: Vec<ProviderFileMaintenanceWarning>,
}

pub(crate) fn recover_provider_file_publication_retirement(
    store: &Store,
    work: &ProviderFilePublicationRetirementWork,
    drain: bool,
) -> Result<ProviderFileRetirementRecoveryOutcome> {
    let mut outcome = ProviderFileRetirementRecoveryOutcome::default();
    loop {
        let Some(scope) = store.begin_provider_file_publication_retirement(
            work.provider,
            &work.material_source_format,
            &work.material_source_root,
            &work.source_path,
            utc_now().timestamp_millis(),
        )?
        else {
            outcome.completed = true;
            return Ok(outcome);
        };
        match store.provider_file_publication_phase(&scope)? {
            ProviderFilePublicationPhase::Preparing => {
                let preparation = store.prepare_provider_file_publication_slice(
                    &scope,
                    PROVIDER_RETIREMENT_SLICE_ROWS,
                )?;
                outcome.made_durable_progress |=
                    preparation.rows_processed > 0 || preparation.complete;
                if let Some(warning) = store.abandon_provider_file_publication(scope)? {
                    outcome.maintenance_warnings.push(warning);
                }
                if drain {
                    continue;
                }
                return Ok(outcome);
            }
            ProviderFilePublicationPhase::Reconciling => {
                let reconciliation = store.reconcile_provider_file_publication_slice(
                    &scope,
                    PROVIDER_RETIREMENT_SLICE_ROWS,
                )?;
                outcome.made_durable_progress |= reconciliation.rows_scanned > 0;
                if let Some(warning) = store.abandon_provider_file_publication(scope)? {
                    outcome.maintenance_warnings.push(warning);
                }
                if drain {
                    continue;
                }
                return Ok(outcome);
            }
            ProviderFilePublicationPhase::ReadyToFinalize => {}
            ProviderFilePublicationPhase::Importing => {
                return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                    "retirement publication entered importer phase",
                )))
            }
        }
        let finalized = store.retire_provider_file_publication(scope)?;
        if let Some(warning) = finalized.maintenance_warning {
            outcome.maintenance_warnings.push(warning);
        }
        outcome.completed = true;
        outcome.made_durable_progress = true;
        return Ok(outcome);
    }
}

#[derive(Debug)]
pub(crate) struct SelectedSourceImportOutcome {
    pub(crate) summary: ProviderImportSummary,
    pub(crate) completed_units: usize,
    pub(crate) completed_bytes: u64,
    pub(crate) deferred_units: usize,
    pub(crate) durable_progress: bool,
    pub(crate) stop_admission: bool,
    #[allow(dead_code)]
    pub(crate) post_import_inventory_generation: Option<u64>,
    pub(crate) post_import_preinventory: Option<SourcePreinventory>,
}

impl SelectedSourceImportOutcome {
    pub(crate) fn made_durable_progress(&self) -> bool {
        self.durable_progress || self.completed_units > 0 || self.summary.has_accepted_content()
    }
}

impl Deref for SelectedSourceImportOutcome {
    type Target = ProviderImportSummary;

    fn deref(&self) -> &Self::Target {
        &self.summary
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderImportBatchOutcome {
    pub(crate) summary: ProviderImportSummary,
    pub(crate) completed_units: usize,
    pub(crate) completed_bytes: u64,
    pub(crate) deferred_units: usize,
    pub(crate) durable_progress: bool,
    pub(crate) stop_admission: bool,
    pub(crate) post_import_inventory_generation: Option<u64>,
    pub(crate) post_import_preinventory: Option<SourcePreinventory>,
}

impl ProviderImportBatchOutcome {
    pub(crate) fn completed(summary: ProviderImportSummary, completed_units: usize) -> Self {
        Self {
            summary,
            completed_units,
            completed_bytes: 0,
            deferred_units: 0,
            durable_progress: false,
            stop_admission: false,
            post_import_inventory_generation: None,
            post_import_preinventory: None,
        }
    }

    fn made_durable_progress(&self) -> bool {
        self.durable_progress || self.completed_units > 0 || self.summary.has_accepted_content()
    }
}

impl Deref for ProviderImportBatchOutcome {
    type Target = ProviderImportSummary;

    fn deref(&self) -> &Self::Target {
        &self.summary
    }
}

#[derive(Debug)]
pub(crate) struct ProviderImportBatchError {
    outcome: ProviderImportBatchOutcome,
    source: anyhow::Error,
}

impl ProviderImportBatchError {
    fn into_parts(self) -> (ProviderImportBatchOutcome, anyhow::Error) {
        (self.outcome, self.source)
    }
}

impl std::fmt::Display for ProviderImportBatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for ProviderImportBatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(crate) fn provider_import_batch_error(
    outcome: ProviderImportBatchOutcome,
    source: anyhow::Error,
) -> anyhow::Error {
    if !outcome.made_durable_progress() {
        return source;
    }
    anyhow::Error::new(ProviderImportBatchError { outcome, source })
}

#[derive(Debug)]
pub(crate) struct SelectedSourceImportResult {
    pub(crate) outcome: SelectedSourceImportOutcome,
    pub(crate) remaining_error: Option<anyhow::Error>,
}

impl Deref for SelectedSourceImportResult {
    type Target = SelectedSourceImportOutcome;

    fn deref(&self) -> &Self::Target {
        &self.outcome
    }
}

#[allow(dead_code)]
#[derive(Debug)]
struct PublicationRecoveryRequiredError {
    source: anyhow::Error,
    maintenance_warning: Option<ProviderFileMaintenanceWarning>,
}

impl std::fmt::Display for PublicationRecoveryRequiredError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for PublicationRecoveryRequiredError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn publication_recovery_required_error(
    source: anyhow::Error,
    maintenance_warning: Option<ProviderFileMaintenanceWarning>,
) -> anyhow::Error {
    anyhow::Error::new(PublicationRecoveryRequiredError {
        source,
        maintenance_warning,
    })
}

pub(crate) fn publication_recovery_required(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<PublicationRecoveryRequiredError>()
            .is_some()
    })
}

#[allow(dead_code)]
pub(crate) fn publication_recovery_maintenance_warning(
    error: &anyhow::Error,
) -> Option<&ProviderFileMaintenanceWarning> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<PublicationRecoveryRequiredError>()
            .and_then(|error| error.maintenance_warning.as_ref())
    })
}

fn push_publication_maintenance_warning(
    summary: &mut ProviderImportSummary,
    warning: ProviderFileMaintenanceWarning,
) {
    summary
        .maintenance_warnings
        .push(ProviderImportMaintenanceWarning {
            kind: ProviderImportMaintenanceKind::ImportInterruptedAfterCommit,
            error: warning.to_string(),
        });
}

#[cfg(test)]
thread_local! {
    static APPEND_SOURCE_FAILURE_AFTER_MUTATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn inject_append_source_failure_after_mutation() {
    APPEND_SOURCE_FAILURE_AFTER_MUTATION.with(|fault| fault.set(true));
}

#[cfg(test)]
fn take_append_source_failure_after_mutation() -> bool {
    APPEND_SOURCE_FAILURE_AFTER_MUTATION.with(|fault| fault.replace(false))
}

enum AppendInventoryUnit<'a> {
    Catalog {
        source: &'a SourceInfo,
        work: &'a CatalogImportWork,
        inventory_generation: u64,
    },
    SourceFile {
        source: &'a SourceInfo,
        work: &'a SourceImportFileWork,
        inventory_generation: u64,
    },
}

pub(crate) enum AppendImportOutcome {
    Imported(ProviderImportSummary),
    Deferred { durable_progress: bool },
}

enum AppendPublicationAttempt {
    Imported(ProviderImportSummary),
    Deferred { durable_progress: bool },
    RetryReplacement,
}

const STAGED_APPEND_PUBLICATION_VERSION: u32 = 2;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct StagedAppendPublicationCompletion {
    summary: ProviderImportSummary,
    has_accepted_content: bool,
    checkpoint: Option<StagedProviderFileCheckpoint>,
    source_prefix_sha256: Option<String>,
    indexed_at_ms: i64,
}

impl StagedAppendPublicationCompletion {
    fn into_restored(
        self,
    ) -> (
        ProviderImportSummary,
        Option<StagedProviderFileCheckpoint>,
        i64,
    ) {
        let mut summary = self.summary;
        if self.has_accepted_content {
            summary.mark_retained_existing_content();
        }
        (summary, self.checkpoint, self.indexed_at_ms)
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct StagedProviderFileCheckpoint {
    import_revision: u32,
    checkpoint_version: u32,
    stable_file_identity: String,
    committed_byte_offset: u64,
    committed_complete_line_count: u64,
    head_sha256: String,
    boundary_sha256: String,
    resume_state_base64: Option<String>,
    updated_at_ms: i64,
}

impl AppendInventoryUnit<'_> {
    fn provider(&self) -> CaptureProvider {
        match self {
            Self::Catalog { work, .. } => work.session.provider,
            Self::SourceFile { work, .. } => work.file.provider,
        }
    }

    fn reason(&self) -> ImportPendingReason {
        match self {
            Self::Catalog { work, .. } => work.reason,
            Self::SourceFile { work, .. } => work.reason,
        }
    }

    fn has_active_publication(&self) -> bool {
        match self {
            Self::Catalog { work, .. } => work.has_active_publication,
            Self::SourceFile { work, .. } => work.has_active_publication,
        }
    }

    fn source_format(&self) -> &str {
        match self {
            Self::Catalog { work, .. } => &work.session.source_format,
            Self::SourceFile { work, .. } => &work.file.source_format,
        }
    }

    fn source_root(&self) -> &str {
        match self {
            Self::Catalog { work, .. } => &work.session.source_root,
            Self::SourceFile { work, .. } => &work.file.source_root,
        }
    }

    fn source_path(&self) -> &str {
        match self {
            Self::Catalog { work, .. } => &work.session.source_path,
            Self::SourceFile { work, .. } => &work.file.source_path,
        }
    }

    fn material_source_root(&self) -> &str {
        match self {
            Self::Catalog { work, .. } => &work.session.source_root,
            Self::SourceFile { work, .. } => &work.file.source_path,
        }
    }

    fn publication_material_owner<'a>(
        &'a self,
        material_source_format: &'a str,
    ) -> ctx_history_store::ProviderFilePublicationMaterialOwner<'a> {
        match self {
            Self::Catalog { .. } => {
                ctx_history_store::ProviderFilePublicationMaterialOwner::catalog_root(
                    self.provider(),
                    material_source_format,
                    self.material_source_root(),
                )
            }
            Self::SourceFile { .. } => {
                ctx_history_store::ProviderFilePublicationMaterialOwner::source_file(
                    self.provider(),
                    material_source_format,
                    self.material_source_root(),
                )
            }
        }
    }

    fn import_revision(&self) -> u32 {
        match self {
            Self::Catalog { work, .. } => work.session.import_revision,
            Self::SourceFile { work, .. } => work.file.import_revision,
        }
    }

    fn file_size_bytes(&self) -> u64 {
        match self {
            Self::Catalog { work, .. } => work.session.file_size_bytes,
            Self::SourceFile { work, .. } => work.file.file_size_bytes,
        }
    }

    fn observation(
        &self,
        indexed_at_ms: i64,
        event_count: Option<u64>,
    ) -> ProviderFileInventoryObservation<'_> {
        match self {
            Self::Catalog {
                work,
                inventory_generation,
                ..
            } => ProviderFileInventoryObservation::ObservedCatalog {
                source_format: &work.session.source_format,
                update: CatalogSourceIndexUpdate {
                    source_root: &work.session.source_root,
                    source_path: &work.session.source_path,
                    file_size_bytes: work.session.file_size_bytes,
                    file_modified_at_ms: work.session.file_modified_at_ms,
                    import_revision: work.session.import_revision,
                    inventory_generation: *inventory_generation,
                    file_sha256: None,
                    event_count,
                    indexed_at_ms,
                },
                metadata: &work.session.metadata,
            },
            Self::SourceFile {
                work,
                inventory_generation,
                ..
            } => ProviderFileInventoryObservation::SourceImport {
                source_format: &work.file.source_format,
                update: SourceImportFileIndexUpdate {
                    source_root: &work.file.source_root,
                    source_path: &work.file.source_path,
                    file_size_bytes: work.file.file_size_bytes,
                    file_modified_at_ms: work.file.file_modified_at_ms,
                    import_revision: work.file.import_revision,
                    inventory_generation: *inventory_generation,
                    metadata: &work.file.metadata,
                    indexed_at_ms,
                },
            },
        }
    }

    fn inventory_generation(&self) -> u64 {
        match self {
            Self::Catalog {
                inventory_generation,
                ..
            }
            | Self::SourceFile {
                inventory_generation,
                ..
            } => *inventory_generation,
        }
    }
}

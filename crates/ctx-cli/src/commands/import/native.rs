use std::collections::{BTreeMap, BTreeSet};
use std::ops::{ControlFlow, Deref};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use super::*;
use crate::commands::import::inventory::observe_source_root;
use crate::commands::import::manifest::{
    collect_source_import_files, observe_selected_source_import_file,
    persist_new_source_import_observation, persist_source_import_observation_with_outcomes,
    persisted_import_identity, same_source_import_observation, source_uses_import_file_manifest,
    SourceImportObservationOutcome,
};
#[cfg(test)]
use crate::commands::import::manifest::{
    persist_source_import_files, persist_source_import_observation_with_outcomes_and_hook,
};
use ctx_history_capture::{ProviderImportMaintenanceKind, ProviderImportMaintenanceWarning};
use ctx_history_store::ProviderFileMaintenanceWarning;

const PROVIDER_PUBLICATION_SLICE_ROWS: usize = 1024;
const PROVIDER_RETIREMENT_SLICE_ROWS: usize = 64;

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
        let preparation = store
            .prepare_provider_file_publication_slice(&scope, PROVIDER_RETIREMENT_SLICE_ROWS)?;
        if !preparation.complete {
            outcome.made_durable_progress |= preparation.source_ids_staged > 0;
            if let Some(warning) = store.abandon_provider_file_publication(scope)? {
                outcome.maintenance_warnings.push(warning);
            }
            if drain {
                continue;
            }
            return Ok(outcome);
        }
        let reconciliation = store
            .reconcile_provider_file_publication_slice(&scope, PROVIDER_RETIREMENT_SLICE_ROWS)?;
        if !reconciliation.complete {
            outcome.made_durable_progress = true;
            if let Some(warning) = store.abandon_provider_file_publication(scope)? {
                outcome.maintenance_warnings.push(warning);
            }
            if drain {
                continue;
            }
            return Ok(outcome);
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
        work: &'a CatalogImportWork,
        inventory_generation: u64,
    },
    SourceFile {
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

const STAGED_APPEND_PUBLICATION_VERSION: u32 = 1;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct StagedAppendPublicationCompletion {
    summary: ProviderImportSummary,
    checkpoint: Option<StagedProviderFileCheckpoint>,
    indexed_at_ms: i64,
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
            } => ProviderFileInventoryObservation::Catalog {
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
            },
            Self::SourceFile {
                work,
                inventory_generation,
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
}

pub(crate) fn import_append_capable_catalog_work(
    store: &mut Store,
    work: &CatalogImportWork,
    inventory_generation: u64,
    record_id: Uuid,
) -> Result<AppendImportOutcome> {
    import_append_capable_work(
        store,
        AppendInventoryUnit::Catalog {
            work,
            inventory_generation,
        },
        record_id,
    )
}

fn import_append_capable_source_file_work(
    store: &mut Store,
    work: &SourceImportFileWork,
    inventory_generation: u64,
    record_id: Uuid,
) -> Result<AppendImportOutcome> {
    import_append_capable_work(
        store,
        AppendInventoryUnit::SourceFile {
            work,
            inventory_generation,
        },
        record_id,
    )
}

fn import_manifested_append_source_file_work(
    store: &mut Store,
    pending_source: &SourceInfo,
    work: &SourceImportFileWork,
    inventory_generation: u64,
) -> Result<AppendImportOutcome> {
    let record = import_record_for_source(pending_source);
    let record_existed = history_record_exists(store, record.id)?;
    store.upsert_record(&record)?;
    let result =
        import_append_capable_source_file_work(store, work, inventory_generation, record.id);
    let remove_orphan = match &result {
        Ok(AppendImportOutcome::Imported(summary)) => summary == &ProviderImportSummary::default(),
        Ok(AppendImportOutcome::Deferred) | Err(_) => true,
    };
    if remove_orphan && !record_existed {
        store
            .delete_orphan_record(record.id)
            .context("clean up manifested append history record")?;
    }
    result
}

fn import_append_capable_work(
    store: &mut Store,
    unit: AppendInventoryUnit<'_>,
    record_id: Uuid,
) -> Result<AppendImportOutcome> {
    let provider = unit.provider();
    let source_format = unit.source_format().to_owned();
    let material_source_format =
        provider_canonical_material_source_format(provider, &source_format).ok_or_else(|| {
            anyhow!(
                "missing canonical material format for {}:{source_format}",
                provider.as_str()
            )
        })?;
    if provider_file_mutation_contract(provider, &source_format)
        != ProviderFileMutationContract::AppendOnlyNewlineDelimited
    {
        return Err(anyhow!(
            "{}:{source_format} is not append-capable",
            provider.as_str()
        ));
    }

    let mut replacement = unit.reason().requires_replacement();
    for _ in 0..2 {
        let admitted_checkpoint = if replacement {
            None
        } else {
            load_admitted_append_checkpoint(store, &unit)?
        };
        let admitted_offset = admitted_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.checkpoint().committed_offset);
        if admitted_checkpoint.is_none() {
            replacement = true;
        }
        let kind = if replacement {
            ProviderFilePublicationKind::Replacement
        } else {
            ProviderFilePublicationKind::Incremental
        };
        let observation = unit.observation(utc_now().timestamp_millis(), None);
        let scope = store
            .begin_provider_file_publication(
                provider,
                observation,
                material_source_format,
                kind,
                utc_now().timestamp_millis(),
            )
            .context("begin append-capable provider publication")?;
        let mut scope = Some(scope);
        let attempt = (|| -> Result<AppendPublicationAttempt> {
            let publication = scope
                .as_ref()
                .expect("append publication scope must remain owned until completion");
            let effective_replacement =
                publication.kind() == ProviderFilePublicationKind::Replacement;
            if effective_replacement {
                match store.provider_file_publication_phase(publication)? {
                    ProviderFilePublicationPhase::Preparing => {
                        store
                            .prepare_provider_file_publication_slice(
                                publication,
                                PROVIDER_PUBLICATION_SLICE_ROWS,
                            )
                            .context("prepare append-capable replacement publication")?;
                        store.abandon_provider_file_publication(
                            scope.take().expect("append publication scope must exist"),
                        )?;
                        return Ok(AppendPublicationAttempt::Deferred {
                            durable_progress: true,
                        });
                    }
                    ProviderFilePublicationPhase::Reconciling => {
                        store
                            .reconcile_provider_file_publication_slice(
                                publication,
                                PROVIDER_PUBLICATION_SLICE_ROWS,
                            )
                            .context("reconcile append-capable replacement publication")?;
                        store.abandon_provider_file_publication(
                            scope.take().expect("append publication scope must exist"),
                        )?;
                        return Ok(AppendPublicationAttempt::Deferred {
                            durable_progress: true,
                        });
                    }
                    ProviderFilePublicationPhase::ReadyToFinalize => {
                        let completion = store
                            .load_provider_file_publication_completion(publication)?
                            .ok_or_else(|| {
                                anyhow::Error::new(CaptureError::SystemInvariant(
                                    "ready provider publication has no staged completion",
                                ))
                            })?;
                        let staged = decode_staged_append_completion(completion)?;
                        let mut summary = staged.summary;
                        let status = provider_summary_import_status(&summary);
                        if status == CatalogIndexedStatus::Rejected {
                            return Err(provider_import_summary_failure_for_unit(
                                provider,
                                unit.source_path(),
                                &summary,
                            ));
                        }
                        let outcome_error =
                            (summary.failed > 0).then(|| source_import_file_failure(&summary));
                        let event_count = Some(
                            summary
                                .imported_events
                                .saturating_add(summary.skipped_events)
                                as u64,
                        );
                        let outcome = ProviderFileImportOutcome {
                            provider,
                            observation: unit.observation(staged.indexed_at_ms, event_count),
                            status,
                            error: outcome_error.as_deref(),
                        };
                        let checkpoint = staged
                            .checkpoint
                            .map(|checkpoint| checkpoint.into_store_checkpoint(&unit))
                            .transpose()?;
                        let commit = ProviderFilePublicationCommit::Replacement(
                            (summary.failed == 0)
                                .then_some(checkpoint.as_ref())
                                .flatten(),
                        );
                        let finalized = store.finalize_provider_file_publication(
                            scope.take().expect("append publication scope must exist"),
                            outcome,
                            commit,
                        )?;
                        if let Some(warning) = finalized.maintenance_warning {
                            push_publication_maintenance_warning(&mut summary, warning);
                        }
                        return Ok(AppendPublicationAttempt::Imported(summary));
                    }
                    ProviderFilePublicationPhase::Importing => {}
                }
            }

            let mode = if effective_replacement {
                ProviderAppendFileImportMode::AppendCapableReplacement
            } else {
                ProviderAppendFileImportMode::Append(admitted_checkpoint.ok_or_else(|| {
                    anyhow::Error::new(CaptureError::SystemInvariant(
                        "incremental publication has no admitted checkpoint",
                    ))
                })?)
            };
            let decision = store
                .with_provider_file_publication_writes_mut(publication, |store| {
                    import_append_capable_provider_file(
                        provider,
                        store,
                        ProviderAppendFileImportOptions {
                            machine_id: CodexSessionImportOptions::default().machine_id,
                            inventory_source_format: source_format.clone(),
                            material_source_format: material_source_format.to_owned(),
                            source_path: PathBuf::from(unit.source_path()),
                            source_root: PathBuf::from(unit.material_source_root()),
                            imported_at: utc_now(),
                            history_record_id: Some(record_id),
                            mode,
                        },
                    )
                })
                .map_err(anyhow::Error::new)
                .context("write append-capable provider publication")?;
            #[cfg(test)]
            if take_append_source_failure_after_mutation() {
                return Err(anyhow::Error::new(CaptureError::InvalidPayload(
                    "injected append source failure after publication mutation".to_owned(),
                )));
            }

            if let ProviderAppendFileImportDecision::ReplacementRequired(_) = decision {
                let abort = store.abort_provider_file_publication(
                    scope.take().expect("append publication scope must exist"),
                )?;
                if let ControlFlow::Break(warning) = abort {
                    return Err(publication_recovery_required_error(
                        anyhow::Error::new(CaptureError::SystemInvariant(
                            "append importer requested replacement after mutating its publication",
                        )),
                        warning,
                    ));
                }
                if effective_replacement {
                    return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                        "append-capable replacement importer requested replacement",
                    )));
                }
                return Ok(AppendPublicationAttempt::RetryReplacement);
            }

            let deferred_without_boundary_progress = !effective_replacement
                && admitted_offset.is_some_and(|prior_offset| {
                    unit.file_size_bytes() > prior_offset
                        && match &decision {
                            ProviderAppendFileImportDecision::Imported(result) => {
                                result.summary == ProviderImportSummary::default()
                                    && result.checkpoint.committed_offset == prior_offset
                            }
                            ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => {
                                result.summary == ProviderImportSummary::default()
                            }
                            ProviderAppendFileImportDecision::DeferredPartial
                            | ProviderAppendFileImportDecision::ReplacementRequired(_) => false,
                        }
                });
            if deferred_without_boundary_progress {
                let abort = store.abort_provider_file_publication(
                    scope.take().expect("append publication scope must exist"),
                )?;
                if let ControlFlow::Break(warning) = abort {
                    return Err(publication_recovery_required_error(
                        anyhow::Error::new(CaptureError::SystemInvariant(
                            "append tail deferred after mutating its publication",
                        )),
                        warning,
                    ));
                }
                return Ok(AppendPublicationAttempt::Deferred {
                    durable_progress: false,
                });
            }

            let (mut summary, checkpoint, retain_checkpoint) = match decision {
                ProviderAppendFileImportDecision::Imported(result) => {
                    (result.summary, Some(result.checkpoint), false)
                }
                ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => {
                    (result.summary, None, !effective_replacement)
                }
                ProviderAppendFileImportDecision::DeferredPartial => {
                    let abort = store.abort_provider_file_publication(
                        scope.take().expect("append publication scope must exist"),
                    )?;
                    if let ControlFlow::Break(warning) = abort {
                        return Err(publication_recovery_required_error(
                            anyhow::Error::new(CaptureError::SystemInvariant(
                                "partial append deferred after mutating its publication",
                            )),
                            warning,
                        ));
                    }
                    return Ok(AppendPublicationAttempt::Deferred {
                        durable_progress: false,
                    });
                }
                ProviderAppendFileImportDecision::ReplacementRequired(_) => unreachable!(),
            };
            let mut status = provider_summary_import_status(&summary);
            if status == CatalogIndexedStatus::Rejected
                && effective_replacement
                && publication.tracks_prior_material()
            {
                let abort = store.abort_provider_file_publication(
                    scope.take().expect("append publication scope must exist"),
                )?;
                match abort {
                    ControlFlow::Continue(warning) => {
                        summary.mark_retained_existing_content();
                        if let Some(warning) = warning {
                            push_publication_maintenance_warning(&mut summary, warning);
                        }
                        return Ok(AppendPublicationAttempt::Imported(summary));
                    }
                    ControlFlow::Break(warning) => {
                        return Err(publication_recovery_required_error(
                            provider_import_summary_failure_for_unit(
                                provider,
                                unit.source_path(),
                                &summary,
                            ),
                            warning,
                        ));
                    }
                }
            }
            if status == CatalogIndexedStatus::Rejected && !effective_replacement {
                summary.mark_retained_existing_content();
                status = provider_summary_import_status(&summary);
            }
            if status == CatalogIndexedStatus::Rejected {
                return Err(provider_import_summary_failure_for_unit(
                    provider,
                    unit.source_path(),
                    &summary,
                ));
            }
            let event_count = Some(
                summary
                    .imported_events
                    .saturating_add(summary.skipped_events) as u64,
            );
            let indexed_at_ms = utc_now().timestamp_millis();
            let outcome_error = (summary.failed > 0).then(|| source_import_file_failure(&summary));
            let outcome = ProviderFileImportOutcome {
                provider,
                observation: unit.observation(indexed_at_ms, event_count),
                status,
                error: outcome_error.as_deref(),
            };
            let stored_checkpoint = checkpoint
                .as_ref()
                .map(|checkpoint| store_checkpoint_from_capture(&unit, checkpoint, indexed_at_ms))
                .transpose()?;
            let retain_safe_checkpoint = summary.failed > 0;
            if effective_replacement {
                let completion =
                    encode_staged_append_completion(summary, stored_checkpoint, indexed_at_ms)?;
                store.stage_provider_file_publication_completion(publication, &completion)?;
                store.abandon_provider_file_publication(
                    scope.take().expect("append publication scope must exist"),
                )?;
                return Ok(AppendPublicationAttempt::Deferred {
                    durable_progress: true,
                });
            }
            let commit = if effective_replacement {
                ProviderFilePublicationCommit::Replacement(if retain_safe_checkpoint {
                    None
                } else {
                    stored_checkpoint.as_ref()
                })
            } else if retain_checkpoint || retain_safe_checkpoint {
                ProviderFilePublicationCommit::RetainCheckpoint
            } else {
                ProviderFilePublicationCommit::Append(stored_checkpoint.as_ref().ok_or_else(
                    || {
                        anyhow::Error::new(CaptureError::SystemInvariant(
                            "append import completed without a checkpoint decision",
                        ))
                    },
                )?)
            };
            let finalized = store.finalize_provider_file_publication(
                scope.take().expect("append publication scope must exist"),
                outcome,
                commit,
            )?;
            if let Some(warning) = finalized.maintenance_warning {
                push_publication_maintenance_warning(&mut summary, warning);
            }
            Ok(AppendPublicationAttempt::Imported(summary))
        })();

        match attempt {
            Ok(AppendPublicationAttempt::Imported(summary)) => {
                return Ok(AppendImportOutcome::Imported(summary));
            }
            Ok(AppendPublicationAttempt::Deferred { durable_progress }) => {
                return Ok(AppendImportOutcome::Deferred { durable_progress });
            }
            Ok(AppendPublicationAttempt::RetryReplacement) => {
                replacement = true;
            }
            Err(error) => {
                if let Some(scope) = scope.take() {
                    match store.abort_provider_file_publication(scope) {
                        Ok(ControlFlow::Continue(_)) => {}
                        Ok(ControlFlow::Break(warning)) => {
                            return Err(publication_recovery_required_error(error, warning));
                        }
                        Err(abort_error) => {
                            return Err(error.context(format!(
                                "release failed append publication: {abort_error}"
                            )));
                        }
                    }
                }
                return Err(error);
            }
        }
    }
    Err(anyhow::Error::new(CaptureError::SystemInvariant(
        "append import replacement retry did not converge",
    )))
}

fn load_admitted_append_checkpoint(
    store: &Store,
    unit: &AppendInventoryUnit<'_>,
) -> Result<Option<ProviderAdmittedJsonlAppendCheckpoint>> {
    let Some(checkpoint) = store.provider_file_checkpoint(ProviderFileCheckpointKey {
        provider: unit.provider(),
        source_format: unit.source_format(),
        source_root: unit.source_root(),
        source_path: unit.source_path(),
    })?
    else {
        return Ok(None);
    };
    if checkpoint.import_revision != unit.import_revision() {
        return Ok(None);
    }
    let Some(stable_identity) =
        ProviderFileStableIdentity::from_storage_key(&checkpoint.stable_file_identity)
    else {
        return Ok(None);
    };
    let resume_state = match checkpoint.resume_state {
        Some(bytes) => {
            let Ok(json) = std::str::from_utf8(&bytes) else {
                return Ok(None);
            };
            let Ok(state) = ProviderJsonlResumeState::decode_persisted_json(json) else {
                return Ok(None);
            };
            Some(state)
        }
        None => None,
    };
    Ok(Some(
        ProviderAdmittedJsonlAppendCheckpoint::from_persisted_admitted_replacement(
            ProviderJsonlAppendCheckpoint {
                version: checkpoint.checkpoint_version,
                stable_identity,
                committed_offset: checkpoint.committed_byte_offset,
                complete_line_count: checkpoint.committed_complete_line_count,
                head_sha256: checkpoint.head_sha256,
                boundary_sha256: checkpoint.boundary_sha256,
                resume_state,
            },
        ),
    ))
}

fn store_checkpoint_from_capture(
    unit: &AppendInventoryUnit<'_>,
    checkpoint: &ProviderJsonlAppendCheckpoint,
    updated_at_ms: i64,
) -> Result<ProviderFileCheckpoint> {
    let resume_state = checkpoint
        .resume_state
        .as_ref()
        .map(ProviderJsonlResumeState::encode_persisted_json)
        .transpose()?
        .map(String::into_bytes);
    Ok(ProviderFileCheckpoint {
        provider: unit.provider(),
        source_format: unit.source_format().to_owned(),
        source_root: unit.source_root().to_owned(),
        source_path: unit.source_path().to_owned(),
        import_revision: unit.import_revision(),
        checkpoint_version: checkpoint.version,
        stable_file_identity: checkpoint.stable_identity.to_storage_key(),
        committed_byte_offset: checkpoint.committed_offset,
        committed_complete_line_count: checkpoint.complete_line_count,
        head_sha256: checkpoint.head_sha256.clone(),
        boundary_sha256: checkpoint.boundary_sha256.clone(),
        resume_state,
        updated_at_ms,
    })
}

fn encode_staged_append_completion(
    summary: ProviderImportSummary,
    checkpoint: Option<ProviderFileCheckpoint>,
    indexed_at_ms: i64,
) -> Result<ProviderFilePublicationCompletion> {
    let staged = StagedAppendPublicationCompletion {
        summary,
        checkpoint: checkpoint.map(StagedProviderFileCheckpoint::from_store_checkpoint),
        indexed_at_ms,
    };
    Ok(ProviderFilePublicationCompletion {
        version: STAGED_APPEND_PUBLICATION_VERSION,
        payload: serde_json::to_value(staged).context("encode staged append completion")?,
    })
}

fn decode_staged_append_completion(
    completion: ProviderFilePublicationCompletion,
) -> Result<StagedAppendPublicationCompletion> {
    if completion.version != STAGED_APPEND_PUBLICATION_VERSION {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "unsupported staged append publication version",
        )));
    }
    serde_json::from_value(completion.payload).context("decode staged append completion")
}

impl StagedProviderFileCheckpoint {
    fn from_store_checkpoint(checkpoint: ProviderFileCheckpoint) -> Self {
        Self {
            import_revision: checkpoint.import_revision,
            checkpoint_version: checkpoint.checkpoint_version,
            stable_file_identity: checkpoint.stable_file_identity,
            committed_byte_offset: checkpoint.committed_byte_offset,
            committed_complete_line_count: checkpoint.committed_complete_line_count,
            head_sha256: checkpoint.head_sha256,
            boundary_sha256: checkpoint.boundary_sha256,
            resume_state_base64: checkpoint.resume_state.map(|bytes| BASE64.encode(bytes)),
            updated_at_ms: checkpoint.updated_at_ms,
        }
    }

    fn into_store_checkpoint(
        self,
        unit: &AppendInventoryUnit<'_>,
    ) -> Result<ProviderFileCheckpoint> {
        let resume_state = self
            .resume_state_base64
            .map(|value| {
                BASE64
                    .decode(value)
                    .context("decode staged append resume state")
            })
            .transpose()?;
        Ok(ProviderFileCheckpoint {
            provider: unit.provider(),
            source_format: unit.source_format().to_owned(),
            source_root: unit.source_root().to_owned(),
            source_path: unit.source_path().to_owned(),
            import_revision: self.import_revision,
            checkpoint_version: self.checkpoint_version,
            stable_file_identity: self.stable_file_identity,
            committed_byte_offset: self.committed_byte_offset,
            committed_complete_line_count: self.committed_complete_line_count,
            head_sha256: self.head_sha256,
            boundary_sha256: self.boundary_sha256,
            resume_state,
            updated_at_ms: self.updated_at_ms,
        })
    }
}

fn provider_import_summary_failure_for_unit(
    provider: CaptureProvider,
    source_path: &str,
    summary: &ProviderImportSummary,
) -> anyhow::Error {
    let detail = summary
        .failures
        .first()
        .map(|failure| format!("line {}: {}", failure.line, failure.error))
        .unwrap_or_else(|| "unknown provider import failure".to_owned());
    rejected_source_error(
        format!(
            "import {} source {} failed with {} failure(s); first failure: {detail}",
            provider.as_str(),
            source_path,
            summary.failed
        ),
        summary,
    )
}

include!("native/selection.rs");
include!("native/batching.rs");
include!("native/manifested.rs");
#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

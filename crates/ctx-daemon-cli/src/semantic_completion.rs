use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;
use ctx_daemon_service::{
    classify_semantic_failure, DaemonSemanticCompletionObservation, DaemonSemanticCompletionTarget,
    DaemonSemanticConfigBinding, DaemonSemanticProgress, PinnedSourceBackedGeneration,
};
use ctx_semantic_index::{SemanticModelContract, SemanticNotReady, SemanticQueryPin};
use ctx_semantic_model::SemanticEmbeddingExecutorConfig;

use crate::query_adapter::{
    reconcile_selected_foreground_semantic, semantic_index_contract_for_selected,
};

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_NO_PROGRESS_BUDGET: Duration = Duration::from_secs(15 * 60);
const DEFAULT_CONTINUOUS_OUTAGE_BUDGET: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticCompletionDaemonConfig {
    daemon_enabled: bool,
    daemon_mode: String,
    semantic_enabled: bool,
}

impl SemanticCompletionDaemonConfig {
    pub fn new(
        daemon_enabled: bool,
        daemon_mode: impl Into<String>,
        semantic_enabled: bool,
    ) -> Self {
        Self {
            daemon_enabled,
            daemon_mode: daemon_mode.into(),
            semantic_enabled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticCompletionBudgets {
    poll_interval: Duration,
    no_progress: Duration,
    continuous_outage: Duration,
}

impl SemanticCompletionBudgets {
    pub const fn new(
        poll_interval: Duration,
        no_progress: Duration,
        continuous_outage: Duration,
    ) -> Self {
        Self {
            poll_interval,
            no_progress,
            continuous_outage,
        }
    }
}

impl Default for SemanticCompletionBudgets {
    fn default() -> Self {
        Self::new(
            DEFAULT_POLL_INTERVAL,
            DEFAULT_NO_PROGRESS_BUDGET,
            DEFAULT_CONTINUOUS_OUTAGE_BUDGET,
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SemanticCompletionError {
    #[error("could not derive the selected semantic contract for Core generation {generation_id}: {source:#}")]
    Contract {
        generation_id: String,
        #[source]
        source: anyhow::Error,
    },
    #[error(
        "Core generation {generation_id} was superseded by active generation {active_generation_id}"
    )]
    CoreSuperseded {
        generation_id: String,
        active_generation_id: String,
        retryable: bool,
    },
    #[error(
        "semantic completion checkpoint failed for Core generation {generation_id}: {source:#}"
    )]
    Checkpoint {
        generation_id: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("semantic preflight failed for Core generation {generation_id}: {source:#}")]
    Preflight {
        generation_id: String,
        retryable: bool,
        #[source]
        source: anyhow::Error,
    },
    #[error(
        "foreground semantic reconciliation failed for Core generation {generation_id}: {source:#}"
    )]
    Reconciliation {
        generation_id: String,
        retryable: bool,
        #[source]
        source: anyhow::Error,
    },
    #[error("daemon semantic activation failed for Core generation {generation_id}: {detail}")]
    DaemonActivationFailed {
        generation_id: String,
        detail: String,
        retryable: bool,
    },
    #[error("daemon configuration reload failed for Core generation {generation_id}: {detail}")]
    DaemonConfigurationFailed {
        generation_id: String,
        detail: String,
        retryable: bool,
    },
    #[error("daemon semantic job failed for Core generation {generation_id}: {detail}")]
    DaemonJobFailed {
        generation_id: String,
        detail: String,
        retryable: bool,
        failure_class: Option<String>,
    },
    #[error("daemon semantic completion made no progress for Core generation {generation_id}")]
    NoProgress {
        generation_id: String,
        retryable: bool,
    },
    #[error("daemon semantic completion was continuously unavailable for Core generation {generation_id}: {detail}")]
    ObservationOutage {
        generation_id: String,
        detail: String,
        retryable: bool,
    },
    #[error(
        "semantic completion postcondition failed for Core generation {generation_id}: {source:#}"
    )]
    Postcondition {
        generation_id: String,
        retryable: bool,
        #[source]
        source: anyhow::Error,
    },
}

impl SemanticCompletionError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Contract { .. } => "semantic_completion_contract_invalid",
            Self::CoreSuperseded { .. } => "semantic_completion_generation_superseded",
            Self::Checkpoint { .. } => "semantic_completion_interrupted",
            Self::Preflight { .. } => "semantic_completion_preflight_failed",
            Self::Reconciliation { .. } => "semantic_completion_reconciliation_failed",
            Self::DaemonActivationFailed { .. } => "semantic_completion_activation_failed",
            Self::DaemonConfigurationFailed { .. } => "semantic_completion_configuration_failed",
            Self::DaemonJobFailed { .. } => "semantic_completion_job_failed",
            Self::NoProgress { .. } => "semantic_completion_no_progress",
            Self::ObservationOutage { .. } => "semantic_completion_observation_unavailable",
            Self::Postcondition { .. } => "semantic_completion_postcondition_failed",
        }
    }

    pub const fn retryable(&self) -> bool {
        match self {
            Self::CoreSuperseded { retryable, .. }
            | Self::Preflight { retryable, .. }
            | Self::Reconciliation { retryable, .. }
            | Self::DaemonActivationFailed { retryable, .. }
            | Self::DaemonConfigurationFailed { retryable, .. }
            | Self::DaemonJobFailed { retryable, .. }
            | Self::NoProgress { retryable, .. }
            | Self::ObservationOutage { retryable, .. }
            | Self::Postcondition { retryable, .. } => *retryable,
            Self::Contract { .. } | Self::Checkpoint { .. } => false,
        }
    }

    pub fn generation_id(&self) -> &str {
        match self {
            Self::Contract { generation_id, .. }
            | Self::CoreSuperseded { generation_id, .. }
            | Self::Checkpoint { generation_id, .. }
            | Self::Preflight { generation_id, .. }
            | Self::Reconciliation { generation_id, .. }
            | Self::DaemonActivationFailed { generation_id, .. }
            | Self::DaemonConfigurationFailed { generation_id, .. }
            | Self::DaemonJobFailed { generation_id, .. }
            | Self::NoProgress { generation_id, .. }
            | Self::ObservationOutage { generation_id, .. }
            | Self::Postcondition { generation_id, .. } => generation_id,
        }
    }
}

struct SelectedSemanticCompletion {
    executor: SemanticEmbeddingExecutorConfig,
    contract: SemanticModelContract,
    source_contract_fingerprint: String,
    executor_selector: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticCompletionCheckpoint {
    Ready,
    Pending { poll_after: Duration },
}

/// Stateful, non-sleeping daemon observation for one exact Core generation.
/// The caller owns polling and may run its cancellation checkpoint between
/// calls without coupling this crate to the eventual #820 cancellation type.
pub struct DaemonSemanticCompletion {
    target: DaemonSemanticCompletionTarget,
    contract: SemanticModelContract,
    budgets: SemanticCompletionBudgets,
    progress_since: Instant,
    last_progress: Option<CompletionProgress>,
    outage_since: Option<Instant>,
}

impl DaemonSemanticCompletion {
    pub fn new(
        pin: &PinnedSourceBackedGeneration,
        executor: SemanticEmbeddingExecutorConfig,
        daemon: SemanticCompletionDaemonConfig,
        budgets: SemanticCompletionBudgets,
    ) -> std::result::Result<Self, SemanticCompletionError> {
        Self::new_at(pin, executor, daemon, budgets, Instant::now())
    }

    fn new_at(
        pin: &PinnedSourceBackedGeneration,
        executor: SemanticEmbeddingExecutorConfig,
        daemon: SemanticCompletionDaemonConfig,
        budgets: SemanticCompletionBudgets,
        now: Instant,
    ) -> std::result::Result<Self, SemanticCompletionError> {
        let generation_id = pin.generation_id();
        let selected = SelectedSemanticCompletion::new(generation_id, executor)?;
        Ok(Self {
            target: selected.daemon_target(generation_id, daemon),
            contract: selected.contract,
            budgets,
            progress_since: now,
            last_progress: None,
            outage_since: None,
        })
    }

    pub fn checkpoint(
        &mut self,
        data_root: &Path,
        pin: &PinnedSourceBackedGeneration,
    ) -> std::result::Result<SemanticCompletionCheckpoint, SemanticCompletionError> {
        let contract = self.contract.clone();
        let target = self.target.clone();
        self.checkpoint_with(
            Instant::now(),
            pin,
            || {
                crate::pin_active_verified_generation(data_root)
                    .map(|active| active.generation_id().to_owned())
            },
            |pin| semantic_preflight_ready(pin, data_root, &contract),
            || ctx_daemon_service::observe_exact_daemon_semantic_completion(data_root, &target),
        )
    }

    fn checkpoint_with<Active, Preflight, Observe>(
        &mut self,
        now: Instant,
        pin: &PinnedSourceBackedGeneration,
        mut active_generation: Active,
        mut preflight: Preflight,
        mut observe: Observe,
    ) -> std::result::Result<SemanticCompletionCheckpoint, SemanticCompletionError>
    where
        Active: FnMut() -> Result<String>,
        Preflight: FnMut(&PinnedSourceBackedGeneration) -> Result<bool>,
        Observe: FnMut() -> Result<DaemonSemanticCompletionObservation>,
    {
        let generation_id = self.target.core_generation_id().to_owned();
        if pin.generation_id() != generation_id {
            return Err(SemanticCompletionError::CoreSuperseded {
                generation_id,
                active_generation_id: pin.generation_id().to_owned(),
                retryable: true,
            });
        }
        let active_generation_id = match active_generation() {
            Ok(active) => active,
            Err(error) => {
                return self
                    .record_outage(now, format!("observe active Core generation: {error:#}"));
            }
        };
        if active_generation_id != generation_id {
            return Err(SemanticCompletionError::CoreSuperseded {
                generation_id,
                active_generation_id,
                retryable: true,
            });
        }

        match preflight(pin) {
            Ok(true) => {
                let active_generation_id = match active_generation() {
                    Ok(active) => active,
                    Err(error) => {
                        return self.record_outage(
                            now,
                            format!("revalidate active Core generation: {error:#}"),
                        );
                    }
                };
                if active_generation_id == generation_id {
                    return Ok(SemanticCompletionCheckpoint::Ready);
                }
                return Err(SemanticCompletionError::CoreSuperseded {
                    generation_id,
                    active_generation_id,
                    retryable: true,
                });
            }
            Ok(false) => {}
            Err(source) => {
                return Err(SemanticCompletionError::Preflight {
                    generation_id,
                    retryable: false,
                    source,
                });
            }
        }

        let observation = match observe() {
            Ok(observation) => observation,
            Err(error) => {
                return self.record_outage(
                    now,
                    format!("observe daemon semantic completion: {error:#}"),
                );
            }
        };
        let progress = match observation {
            DaemonSemanticCompletionObservation::Ready => CompletionProgress::ReadyAwaitingIndex,
            DaemonSemanticCompletionObservation::Pending(progress) => {
                CompletionProgress::Pending(progress)
            }
            DaemonSemanticCompletionObservation::Unavailable { detail } => {
                return self.record_outage(now, detail);
            }
            DaemonSemanticCompletionObservation::ActivationFailed { detail, retryable } => {
                return Err(SemanticCompletionError::DaemonActivationFailed {
                    generation_id,
                    detail,
                    retryable,
                });
            }
            DaemonSemanticCompletionObservation::ConfigurationFailed { detail, retryable } => {
                return Err(SemanticCompletionError::DaemonConfigurationFailed {
                    generation_id,
                    detail,
                    retryable,
                });
            }
            DaemonSemanticCompletionObservation::JobFailed {
                detail,
                retryable,
                failure_class,
            } => {
                return Err(SemanticCompletionError::DaemonJobFailed {
                    generation_id,
                    detail,
                    retryable,
                    failure_class,
                });
            }
        };
        self.record_progress(now, progress)
    }

    fn record_progress(
        &mut self,
        now: Instant,
        progress: CompletionProgress,
    ) -> std::result::Result<SemanticCompletionCheckpoint, SemanticCompletionError> {
        self.outage_since = None;
        if self.last_progress.as_ref() != Some(&progress) {
            self.progress_since = now;
            self.last_progress = Some(progress);
        } else if now.saturating_duration_since(self.progress_since) >= self.budgets.no_progress {
            return Err(SemanticCompletionError::NoProgress {
                generation_id: self.target.core_generation_id().to_owned(),
                retryable: true,
            });
        }
        Ok(SemanticCompletionCheckpoint::Pending {
            poll_after: self.budgets.poll_interval,
        })
    }

    fn record_outage(
        &mut self,
        now: Instant,
        detail: String,
    ) -> std::result::Result<SemanticCompletionCheckpoint, SemanticCompletionError> {
        let started_at = *self.outage_since.get_or_insert(now);
        if now.saturating_duration_since(started_at) >= self.budgets.continuous_outage {
            return Err(SemanticCompletionError::ObservationOutage {
                generation_id: self.target.core_generation_id().to_owned(),
                detail,
                retryable: true,
            });
        }
        Ok(SemanticCompletionCheckpoint::Pending {
            poll_after: self.budgets.poll_interval,
        })
    }
}

impl SelectedSemanticCompletion {
    fn new(
        generation_id: &str,
        executor: SemanticEmbeddingExecutorConfig,
    ) -> std::result::Result<Self, SemanticCompletionError> {
        let contract =
            semantic_index_contract_for_selected(executor.contract()).map_err(|source| {
                SemanticCompletionError::Contract {
                    generation_id: generation_id.to_owned(),
                    source,
                }
            })?;
        let source_contract_fingerprint =
            ctx_semantic_index::source_backed_semantic_contract_fingerprint(&contract).map_err(
                |source| SemanticCompletionError::Contract {
                    generation_id: generation_id.to_owned(),
                    source,
                },
            )?;
        let executor_selector = executor.http_endpoint().unwrap_or("builtin").to_owned();
        Ok(Self {
            executor,
            contract,
            source_contract_fingerprint,
            executor_selector,
        })
    }

    fn daemon_target(
        &self,
        generation_id: &str,
        daemon: SemanticCompletionDaemonConfig,
    ) -> DaemonSemanticCompletionTarget {
        DaemonSemanticCompletionTarget::new(
            generation_id,
            self.contract.fingerprint(),
            self.source_contract_fingerprint.clone(),
            DaemonSemanticConfigBinding::new(
                daemon.daemon_enabled,
                daemon.daemon_mode,
                daemon.semantic_enabled,
                self.executor_selector.clone(),
                self.executor.contract().fingerprint(),
            ),
        )
    }
}

/// Completes one exact pinned generation in the foreground without creating a
/// semantic query session. An already `Ready` or `ReadyEmpty` projection
/// returns before executor construction, endpoint traffic, model loading, or a
/// writable semantic-store open.
pub fn complete_semantic_generation_foreground(
    data_root: &Path,
    pin: PinnedSourceBackedGeneration,
    executor: SemanticEmbeddingExecutorConfig,
) -> std::result::Result<PinnedSourceBackedGeneration, SemanticCompletionError> {
    complete_semantic_generation_foreground_with_checkpoint(
        data_root,
        pin,
        executor,
        &mut || Ok(()),
    )
}

pub fn complete_semantic_generation_foreground_with_checkpoint(
    data_root: &Path,
    pin: PinnedSourceBackedGeneration,
    executor: SemanticEmbeddingExecutorConfig,
    checkpoint: &mut dyn FnMut() -> Result<()>,
) -> std::result::Result<PinnedSourceBackedGeneration, SemanticCompletionError> {
    complete_semantic_generation_foreground_with_checkpoint_and_final_preflight(
        data_root,
        pin,
        executor,
        checkpoint,
        &mut |pin, data_root, contract| {
            SemanticQueryPin::preflight(pin.verified_index(), data_root, contract).map(|_| ())
        },
    )
}

fn complete_semantic_generation_foreground_with_checkpoint_and_final_preflight(
    data_root: &Path,
    pin: PinnedSourceBackedGeneration,
    executor: SemanticEmbeddingExecutorConfig,
    checkpoint: &mut dyn FnMut() -> Result<()>,
    final_preflight: &mut dyn FnMut(
        &PinnedSourceBackedGeneration,
        &Path,
        &SemanticModelContract,
    ) -> Result<()>,
) -> std::result::Result<PinnedSourceBackedGeneration, SemanticCompletionError> {
    let generation_id = pin.generation_id().to_owned();
    let selected = SelectedSemanticCompletion::new(&generation_id, executor)?;
    run_foreground_checkpoint(data_root, checkpoint, &generation_id)?;
    if semantic_preflight_ready(&pin, data_root, &selected.contract).map_err(|source| {
        SemanticCompletionError::Preflight {
            generation_id: generation_id.clone(),
            retryable: false,
            source,
        }
    })? {
        ensure_active_generation(data_root, &generation_id)?;
        return Ok(pin);
    }

    run_foreground_checkpoint(data_root, checkpoint, &generation_id)?;
    let mut foreground_checkpoint = || -> Result<()> {
        run_foreground_checkpoint(data_root, checkpoint, &generation_id).map_err(anyhow::Error::new)
    };
    reconcile_selected_foreground_semantic(
        pin.verified_index(),
        data_root,
        selected.executor,
        &selected.contract,
        &mut foreground_checkpoint,
    )
    .map_err(|source| {
        if matches!(
            source.downcast_ref::<SemanticCompletionError>(),
            Some(SemanticCompletionError::CoreSuperseded { .. })
        ) {
            return source
                .downcast::<SemanticCompletionError>()
                .expect("matched semantic completion supersession error");
        }
        SemanticCompletionError::Reconciliation {
            generation_id: generation_id.clone(),
            retryable: reconciliation_failure_is_retryable(&source),
            source,
        }
    })?;
    run_foreground_checkpoint(data_root, checkpoint, &generation_id)?;
    match final_preflight(&pin, data_root, &selected.contract) {
        Ok(()) => {
            ensure_active_generation(data_root, &generation_id)?;
            Ok(pin)
        }
        Err(source) => Err(SemanticCompletionError::Postcondition {
            generation_id,
            retryable: source
                .downcast_ref::<SemanticNotReady>()
                .is_some_and(SemanticNotReady::retryable),
            source,
        }),
    }
}

fn reconciliation_failure_is_retryable(source: &anyhow::Error) -> bool {
    classify_semantic_failure(source).retryable()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompletionProgress {
    Pending(DaemonSemanticProgress),
    ReadyAwaitingIndex,
}

fn semantic_preflight_ready(
    pin: &PinnedSourceBackedGeneration,
    data_root: &Path,
    contract: &SemanticModelContract,
) -> Result<bool> {
    match SemanticQueryPin::preflight(pin.verified_index(), data_root, contract) {
        Ok(_) => Ok(true),
        Err(error)
            if error
                .downcast_ref::<SemanticNotReady>()
                .is_some_and(SemanticNotReady::retryable) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn ensure_active_generation(
    data_root: &Path,
    generation_id: &str,
) -> std::result::Result<(), SemanticCompletionError> {
    let active_generation_id = crate::pin_active_verified_generation(data_root)
        .map_err(|source| SemanticCompletionError::Preflight {
            generation_id: generation_id.to_owned(),
            retryable: true,
            source,
        })?
        .generation_id()
        .to_owned();
    if active_generation_id != generation_id {
        return Err(SemanticCompletionError::CoreSuperseded {
            generation_id: generation_id.to_owned(),
            active_generation_id,
            retryable: true,
        });
    }
    Ok(())
}

fn run_foreground_checkpoint(
    data_root: &Path,
    checkpoint: &mut dyn FnMut() -> Result<()>,
    generation_id: &str,
) -> std::result::Result<(), SemanticCompletionError> {
    run_checkpoint(checkpoint, generation_id)?;
    ensure_active_generation(data_root, generation_id)
}

fn run_checkpoint(
    checkpoint: &mut dyn FnMut() -> Result<()>,
    generation_id: &str,
) -> std::result::Result<(), SemanticCompletionError> {
    checkpoint().map_err(|source| SemanticCompletionError::Checkpoint {
        generation_id: generation_id.to_owned(),
        source,
    })
}

#[cfg(test)]
#[path = "semantic_completion/tests.rs"]
mod tests;

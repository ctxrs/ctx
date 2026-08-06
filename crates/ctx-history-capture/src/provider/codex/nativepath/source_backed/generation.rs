use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

type CodexSessionPlanV0 = (CodexCatalogSource, SourceKey, String);

#[derive(Debug, Clone)]
enum CodexGenerationParticipantV0 {
    SessionTree {
        roots: Arc<[PathBuf]>,
    },
    ExplicitSession {
        input: CodexExplicitSessionSourceBackedInputV0,
    },
}

#[derive(Clone)]
pub(crate) struct CodexGenerationRouteV0 {
    coordinator: Arc<CodexGenerationNormalizationCoordinatorV0>,
    participant: usize,
}

impl CodexGenerationRouteV0 {
    pub(crate) fn participant(&self) -> usize {
        self.participant
    }

    pub(super) fn prepared(&self) -> CodexSourceBackedResultV0<CodexPreparedRouteV0> {
        self.coordinator.prepared(self.participant)
    }

    #[cfg(test)]
    pub(super) fn record_worker_start(&self) {
        self.coordinator.record_worker_start();
    }
}

#[derive(Clone)]
pub(super) struct CodexPreparedRouteV0 {
    pub(super) missing: bool,
    pub(super) sources: Vec<CodexSessionPlanV0>,
    pub(super) rejections: Vec<CodexLineageRejectedSourceV0>,
    pub(super) authority: Arc<CodexOutcomeLineageAuthorityV0>,
    #[cfg(test)]
    pub(super) work: CodexCatalogWorkV0,
}

struct CodexPendingRouteV0 {
    missing: bool,
    sources: Vec<CodexSessionPlanV0>,
    rejections: Vec<CodexLineageRejectedSourceV0>,
    #[cfg(test)]
    work: CodexCatalogWorkV0,
}

struct CodexPreparedGenerationV0 {
    routes: HashMap<usize, CodexPreparedRouteV0>,
    #[cfg(test)]
    worker_start_latch: CodexWorkerStartLatchV0,
}

#[derive(Default)]
struct CodexGenerationCoordinatorStateV0 {
    next_participant: usize,
    participants: BTreeMap<usize, CodexGenerationParticipantV0>,
    prepared: Option<CodexPreparedGenerationV0>,
}

/// Owns the one selected Codex lineage graph for a source-backed generation.
///
/// Registration contributes route-local discovery authorities. Publication
/// selects the exact participating routes and asks this coordinator to freeze
/// every header and normalize their union before the first JSONL leaf worker
/// can start. Adapters then consume only their route-owned partition while
/// sharing the generation-wide lineage fact authority.
#[derive(Default)]
pub(crate) struct CodexGenerationNormalizationCoordinatorV0 {
    state: Mutex<CodexGenerationCoordinatorStateV0>,
}

impl std::fmt::Debug for CodexGenerationNormalizationCoordinatorV0 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CodexGenerationNormalizationCoordinatorV0")
    }
}

impl CodexGenerationNormalizationCoordinatorV0 {
    pub(crate) fn register_session_tree(
        self: &Arc<Self>,
        roots: Vec<PathBuf>,
    ) -> CodexSourceBackedResultV0<CodexGenerationRouteV0> {
        self.register(CodexGenerationParticipantV0::SessionTree {
            roots: roots.into(),
        })
    }

    pub(crate) fn register_explicit_session(
        self: &Arc<Self>,
        input: CodexExplicitSessionSourceBackedInputV0,
    ) -> CodexSourceBackedResultV0<CodexGenerationRouteV0> {
        self.register(CodexGenerationParticipantV0::ExplicitSession { input })
    }

    fn register(
        self: &Arc<Self>,
        participant: CodexGenerationParticipantV0,
    ) -> CodexSourceBackedResultV0<CodexGenerationRouteV0> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let id = state.next_participant;
        state.next_participant = state
            .next_participant
            .checked_add(1)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
        state.participants.insert(id, participant);
        state.prepared = None;
        Ok(CodexGenerationRouteV0 {
            coordinator: Arc::clone(self),
            participant: id,
        })
    }

    pub(crate) fn prepare(&self, selected: &[usize]) -> CodexSourceBackedResultV0<()> {
        let participants = {
            let state = self
                .state
                .lock()
                .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            selected
                .iter()
                .map(|id| {
                    state
                        .participants
                        .get(id)
                        .cloned()
                        .map(|participant| (*id, participant))
                        .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
                })
                .collect::<CodexSourceBackedResultV0<Vec<_>>>()?
        };

        let mut routes = HashMap::<usize, CodexPendingRouteV0>::with_capacity(participants.len());
        let mut plans = Vec::<CodexSessionPlanV0>::new();
        let mut owners = HashMap::<(PathBuf, String), usize>::new();
        let mut descriptors = HashMap::<[u8; 32], (SourceKey, String)>::new();

        for (participant, discovery) in participants {
            let (missing, discovered, work) = match discovery {
                CodexGenerationParticipantV0::SessionTree { roots } => {
                    let inventory = discover_codex_session_tree_inventory_v0(&roots)?;
                    #[cfg(test)]
                    let work = inventory.work;
                    #[cfg(not(test))]
                    let work = ();
                    (false, inventory.sources, work)
                }
                CodexGenerationParticipantV0::ExplicitSession { input } => {
                    let inventory = observe_codex_explicit_session_source_backed_v0(&input)?;
                    let plan = inventory.source_plan();
                    #[cfg(test)]
                    let work = CodexCatalogWorkV0::default();
                    #[cfg(not(test))]
                    let work = ();
                    (plan.is_none(), plan.into_iter().collect(), work)
                }
            };
            #[cfg(not(test))]
            let _ = work;
            routes.insert(
                participant,
                CodexPendingRouteV0 {
                    missing,
                    sources: Vec::new(),
                    rejections: Vec::new(),
                    #[cfg(test)]
                    work,
                },
            );
            for plan in discovered {
                let descriptor = plan.1.exact_descriptor_digest();
                if let Some((existing, native_session_id)) = descriptors.get(&descriptor) {
                    if !existing.exact_descriptor_eq(&plan.1) || native_session_id != &plan.2 {
                        return Err(CodexSourceBackedErrorV0::Capture(
                            CaptureError::SystemInvariant(
                                "Codex generation source descriptor digest collision",
                            ),
                        ));
                    }
                    // Overlapping automatic and explicit roots retain the
                    // first registered route's established source ownership.
                    continue;
                }
                descriptors.insert(descriptor, (plan.1.clone(), plan.2.clone()));
                owners
                    .entry((plan.0.source_path.clone(), plan.2.clone()))
                    .or_insert(participant);
                plans.push(plan);
            }
        }

        let normalized = CodexOutcomeLineageAuthorityV0::normalize_sources(&plans)?;
        let selected_native_session_ids = normalized
            .sources
            .iter()
            .map(|(_, _, native_session_id)| native_session_id.clone())
            .collect::<HashSet<_>>();
        normalized
            .authority
            .bind_route_sources(&selected_native_session_ids)?;
        let authority = Arc::new(normalized.authority);
        #[cfg(test)]
        let valid_sources = normalized.sources.len();
        #[cfg(test)]
        let rejected_sources = normalized.rejections.len();

        for plan in normalized.sources {
            let owner = owners
                .get(&(plan.0.source_path.clone(), plan.2.clone()))
                .copied()
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            routes
                .get_mut(&owner)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
                .sources
                .push(plan);
        }
        for rejection in normalized.rejections {
            let native_session_id = rejection
                .source
                .catalog_native_session_id
                .as_ref()
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            let owner = owners
                .get(&(
                    rejection.source.source_path.clone(),
                    native_session_id.clone(),
                ))
                .copied()
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            routes
                .get_mut(&owner)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
                .rejections
                .push(rejection);
        }
        let routes = routes
            .into_iter()
            .map(|(participant, route)| {
                (
                    participant,
                    CodexPreparedRouteV0 {
                        missing: route.missing,
                        sources: route.sources,
                        rejections: route.rejections,
                        authority: Arc::clone(&authority),
                        #[cfg(test)]
                        work: route.work,
                    },
                )
            })
            .collect();

        #[cfg(test)]
        let worker_start_latch = CodexWorkerStartLatchV0::default();
        let prepared = CodexPreparedGenerationV0 {
            routes,
            #[cfg(test)]
            worker_start_latch: worker_start_latch.clone(),
        };
        self.state
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
            .prepared = Some(prepared);
        #[cfg(test)]
        run_after_codex_lineage_normalization_hook_v0(
            valid_sources,
            rejected_sources,
            worker_start_latch,
        );
        Ok(())
    }

    fn prepared(&self, participant: usize) -> CodexSourceBackedResultV0<CodexPreparedRouteV0> {
        self.state
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
            .prepared
            .as_ref()
            .and_then(|prepared| prepared.routes.get(&participant))
            .cloned()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
    }

    #[cfg(test)]
    fn record_worker_start(&self) {
        if let Ok(state) = self.state.lock() {
            if let Some(prepared) = state.prepared.as_ref() {
                prepared.worker_start_latch.record_start();
            }
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct CodexWorkerStartLatchV0(Arc<AtomicU64>);

#[cfg(test)]
impl CodexWorkerStartLatchV0 {
    fn record_start(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn starts(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct CodexLineageNormalizationObservationV0 {
    pub(crate) valid_sources: usize,
    pub(crate) rejected_sources: usize,
    pub(crate) worker_starts_at_normalization: u64,
    pub(crate) worker_start_latch: CodexWorkerStartLatchV0,
}

#[cfg(test)]
std::thread_local! {
    static AFTER_CODEX_LINEAGE_NORMALIZATION_HOOK:
        std::cell::RefCell<Option<Box<dyn FnOnce(CodexLineageNormalizationObservationV0)>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn install_after_codex_lineage_normalization_hook_v0(
    hook: impl FnOnce(CodexLineageNormalizationObservationV0) + 'static,
) {
    AFTER_CODEX_LINEAGE_NORMALIZATION_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "Codex normalization hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_codex_lineage_normalization_hook_v0(
    valid_sources: usize,
    rejected_sources: usize,
    worker_start_latch: CodexWorkerStartLatchV0,
) {
    AFTER_CODEX_LINEAGE_NORMALIZATION_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(CodexLineageNormalizationObservationV0 {
                valid_sources,
                rejected_sources,
                worker_starts_at_normalization: worker_start_latch.starts(),
                worker_start_latch,
            });
        }
    });
}

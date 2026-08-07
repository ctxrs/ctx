use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

type CodexSessionPlanV0 = (CodexCatalogSource, SourceKey, String);

fn decode_certified_lineage_facts_v0(
    source: &CertifiedSource,
) -> CodexSourceBackedResultV0<Option<CodexCertifiedLineageFactsV0>> {
    if source.parser_revision() != CODEX_PARSER_REVISION {
        return Ok(None);
    }
    let Some(frontier) = source
        .frontier()
        .filter(|frontier| frontier.checkpoint_kind() == CODEX_FRONTIER_KIND)
    else {
        return Ok(None);
    };
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
    };
    let checkpoint = CodexNativeCheckpoint::decode(bytes)
        .map_err(|_| CodexSourceBackedErrorV0::InvalidCheckpoint)?;
    Ok(checkpoint.certified_lineage_facts().cloned())
}

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

pub(crate) struct CodexGenerationCarriedRouteV0 {
    pub(crate) participant: usize,
    pub(crate) sources: HashMap<SourceKey, CertifiedSource>,
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
    revalidation_sources: Vec<CodexCatalogSource>,
    sources_revalidated: bool,
    #[cfg(test)]
    worker_start_latch: CodexWorkerStartLatchV0,
}

#[derive(Default)]
struct CodexGenerationCoordinatorStateV0 {
    next_participant: usize,
    participants: BTreeMap<usize, CodexGenerationParticipantV0>,
    prepared: Option<CodexPreparedGenerationV0>,
    #[cfg(test)]
    lineage_budget_limits: Option<(usize, usize)>,
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

    pub(crate) fn prepare(
        &self,
        selected: &[usize],
        carried: Vec<CodexGenerationCarriedRouteV0>,
    ) -> CodexSourceBackedResultV0<()> {
        let selected = selected.iter().copied().collect::<HashSet<_>>();
        let carried = carried
            .into_iter()
            .map(|route| (route.participant, route.sources))
            .collect::<HashMap<_, _>>();
        let participant_ids = selected
            .iter()
            .copied()
            .chain(carried.keys().copied())
            .collect::<BTreeSet<_>>();
        let participants = {
            let state = self
                .state
                .lock()
                .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            participant_ids
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
            let carried_sources = carried.get(&participant);
            let (missing, discovered, work) = match discovery {
                CodexGenerationParticipantV0::SessionTree { roots } => {
                    let inventory = match carried_sources {
                        Some(base) => {
                            discover_codex_carried_session_tree_inventory_v0(&roots, base)?
                        }
                        None => discover_codex_session_tree_inventory_v0(&roots)?,
                    };
                    #[cfg(test)]
                    let work = inventory.work;
                    #[cfg(not(test))]
                    let work = ();
                    (false, inventory.sources, work)
                }
                CodexGenerationParticipantV0::ExplicitSession { input } => {
                    let inventory = match carried_sources {
                        Some(base) => base
                            .get(input.source())
                            .map(|base| {
                                observe_codex_carried_explicit_session_source_backed_v0(
                                    &input, base,
                                )
                            })
                            .transpose()?
                            .unwrap_or_else(CodexExplicitSessionInventoryV0::missing),
                        None => observe_codex_explicit_session_source_backed_v0(&input)?,
                    };
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
                } else {
                    descriptors.insert(descriptor, (plan.1.clone(), plan.2.clone()));
                }
                let observation = (plan.0.source_path.clone(), plan.2.clone());
                if owners.contains_key(&observation) {
                    // Overlapping automatic and explicit roots retain the
                    // first registered route's established source ownership.
                    // A distinct path carrying the same native session ID is
                    // not an overlap: both observations must reach lineage
                    // normalization so the complete component is quarantined.
                    continue;
                }
                owners.insert(observation, participant);
                plans.push(plan);
            }
        }

        let mut normalized = CodexOutcomeLineageAuthorityV0::normalize_sources(&plans)?;
        let selected_native_session_ids = normalized
            .sources
            .iter()
            .filter(|plan| {
                owners
                    .get(&(plan.0.source_path.clone(), plan.2.clone()))
                    .is_some_and(|owner| selected.contains(owner))
            })
            .map(|(_, _, native_session_id)| native_session_id.clone())
            .collect::<HashSet<_>>();
        normalized
            .authority
            .bind_route_sources(&selected_native_session_ids)?;
        #[cfg(test)]
        if let Some((byte_limit, fact_limit)) = self
            .state
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
            .lineage_budget_limits
        {
            normalized
                .authority
                .set_generation_component_budget_limits(byte_limit, fact_limit);
        }
        for plan in &normalized.sources {
            let owner = owners
                .get(&(plan.0.source_path.clone(), plan.2.clone()))
                .copied()
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            let Some(base) = carried.get(&owner).and_then(|sources| sources.get(&plan.1)) else {
                continue;
            };
            let Some(facts) = decode_certified_lineage_facts_v0(base)? else {
                continue;
            };
            if normalized.authority.needs_descendant_facts(&plan.2)? {
                normalized.authority.register_certified(&plan.2, &facts)?;
            }
        }
        let mut component_owners = HashMap::<u64, HashSet<usize>>::new();
        for plan in &normalized.sources {
            let owner = owners
                .get(&(plan.0.source_path.clone(), plan.2.clone()))
                .copied()
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            let component = normalized
                .authority
                .component_partition(&plan.2)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            if selected.contains(&owner) {
                component_owners.entry(component).or_default().insert(owner);
            }
        }
        let component_owner_counts = component_owners
            .into_iter()
            .map(|(component, owners)| (component, owners.len()))
            .collect::<HashMap<_, _>>();
        normalized
            .authority
            .initialize_generation_spill(&component_owner_counts)?;
        let lineage_fact_source_scans =
            prepare_generation_lineage_v0(&normalized.sources, &mut normalized.authority)?;
        #[cfg(not(test))]
        let _ = lineage_fact_source_scans;
        // Preparation may have consumed an explicit route's retained opening
        // capability. Route discovery must reopen and bind the current path
        // entry after the generation-wide replacement fence below. That fence
        // covers only the selected sources and their transitive ancestors;
        // unrelated carried components remain certified writer authority and
        // must not broaden an exact refresh's replacement scope.
        let mut revalidation_sources = Vec::new();
        for (source, _, native_session_id) in &mut normalized.sources {
            let participates = normalized
                .authority
                .generation_participates(native_session_id)?;
            source.opened = None;
            if participates {
                revalidation_sources.push(source.clone());
            }
        }
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
            revalidation_sources,
            sources_revalidated: false,
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
            lineage_fact_source_scans,
        );
        Ok(())
    }

    fn prepared(&self, participant: usize) -> CodexSourceBackedResultV0<CodexPreparedRouteV0> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let prepared = state
            .prepared
            .as_mut()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        if !prepared.sources_revalidated {
            for source in &prepared.revalidation_sources {
                let opened =
                    reopen_codex_source_capability(source).map_err(map_lineage_capture_error)?;
                revalidate_codex_catalog_source_capability(source, &opened)
                    .map_err(map_lineage_capture_error)?;
            }
            prepared.sources_revalidated = true;
        }
        prepared
            .routes
            .get(&participant)
            .cloned()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
    }

    #[cfg(test)]
    pub(crate) fn set_generation_lineage_budget_limits(
        &self,
        byte_limit: usize,
        fact_limit: usize,
    ) {
        if let Ok(mut state) = self.state.lock() {
            state.lineage_budget_limits = Some((byte_limit, fact_limit));
        }
    }

    #[cfg(test)]
    pub(crate) fn generation_lineage_metrics(&self) -> Option<(usize, usize, usize, usize, usize)> {
        self.state.lock().ok().and_then(|state| {
            state
                .prepared
                .as_ref()
                .and_then(|prepared| prepared.routes.values().next())
                .map(|route| route.authority.generation_component_metrics())
        })
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
    pub(crate) lineage_fact_source_scans: u64,
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
    lineage_fact_source_scans: u64,
) {
    AFTER_CODEX_LINEAGE_NORMALIZATION_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(CodexLineageNormalizationObservationV0 {
                valid_sources,
                rejected_sources,
                worker_starts_at_normalization: worker_start_latch.starts(),
                worker_start_latch,
                lineage_fact_source_scans,
            });
        }
    });
}

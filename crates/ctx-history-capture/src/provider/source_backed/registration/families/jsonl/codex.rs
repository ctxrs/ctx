use super::*;
use std::sync::Mutex;

use crate::provider::codex::nativepath::{
    discover_codex_root_inventory_v0, discover_codex_session_tree_inventory_v0,
    ingest_codex_sources_v0, CodexSessionTreeInventoryV0, CodexSourceBackedResultV0,
};

#[cfg(test)]
type ExplicitCodexStageHook = Box<dyn FnOnce(CodexSourceBackedCountersV0)>;

#[cfg(test)]
std::thread_local! {
    static AFTER_EXPLICIT_CODEX_STAGE_HOOK:
        std::cell::RefCell<Option<ExplicitCodexStageHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_after_explicit_codex_stage_hook(
    hook: impl FnOnce(CodexSourceBackedCountersV0) + 'static,
) {
    AFTER_EXPLICIT_CODEX_STAGE_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "explicit Codex stage hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_explicit_codex_stage_hook(counters: CodexSourceBackedCountersV0) {
    let hook = AFTER_EXPLICIT_CODEX_STAGE_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook(counters);
    }
}

#[derive(Clone)]
struct CodexSessionTreeTerminalEvidence {
    inventory: CertifiedSourceInventory,
    sources: HashMap<SourceKey, CodexTerminalSourceEvidenceV0>,
}

#[derive(Debug)]
struct CodexSessionTreeRuntime {
    resolver: Mutex<Arc<CodexLocatorResolverV0>>,
}

impl CodexSessionTreeRuntime {
    fn new(inventory: &CodexSessionTreeInventoryV0) -> CodexSourceBackedResultV0<Self> {
        Ok(Self {
            resolver: Mutex::new(Arc::new(
                CodexLocatorResolverV0::from_session_tree_inventory(inventory)?,
            )),
        })
    }

    fn install_inventory(
        &self,
        inventory: &CodexSessionTreeInventoryV0,
    ) -> CodexSourceBackedResultV0<()> {
        let resolver = Arc::new(CodexLocatorResolverV0::from_session_tree_inventory(
            inventory,
        )?);
        *self.resolver.lock().map_err(|_| {
            CodexSourceBackedErrorV0::Capture(CaptureError::InvalidPayload(
                "Codex resident resolver lock was poisoned".to_owned(),
            ))
        })? = resolver;
        Ok(())
    }

    fn resolver(&self) -> CodexSourceBackedResultV0<Arc<CodexLocatorResolverV0>> {
        self.resolver
            .lock()
            .map(|resolver| Arc::clone(&resolver))
            .map_err(|_| {
                CodexSourceBackedErrorV0::Capture(CaptureError::InvalidPayload(
                    "Codex resident resolver lock was poisoned".to_owned(),
                ))
            })
    }

    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        self.resolver()
            .and_then(|resolver| resolver.hydrate_event_request(request))
            .map_err(codex_locator_hydration_failure)
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        self.resolver()
            .and_then(|resolver| resolver.hydrate_batch_request(request))
            .map_err(codex_locator_hydration_failure)
    }
}

#[derive(Clone, Default)]
struct CodexSessionTreeOwnership {
    sources: HashMap<[u8; 32], Vec<SourceKey>>,
}

impl CodexSessionTreeOwnership {
    fn remember(&mut self, source: &SourceKey) {
        let sources = self
            .sources
            .entry(source.exact_descriptor_digest())
            .or_default();
        if !sources
            .iter()
            .any(|candidate| candidate.exact_descriptor_eq(source))
        {
            sources.push(source.clone());
        }
    }

    fn owns(&self, source: &SourceKey) -> bool {
        self.sources
            .get(&source.exact_descriptor_digest())
            .is_some_and(|sources| {
                sources
                    .iter()
                    .any(|candidate| candidate.exact_descriptor_eq(source))
            })
    }
}

#[cfg(test)]
type CodexSessionTreeCounterObserver =
    Arc<dyn Fn(CodexSourceBackedCountersV0) + Send + Sync + 'static>;
#[cfg(test)]
type CodexSessionTreeAfterScan = Arc<dyn Fn() + Send + Sync + 'static>;

#[derive(Clone)]
struct CodexSessionTreeScanControl {
    indexer_threads: usize,
    scanner_workers: Option<usize>,
    #[cfg(test)]
    counter_observer: Option<CodexSessionTreeCounterObserver>,
    #[cfg(test)]
    after_scan: Option<CodexSessionTreeAfterScan>,
}

pub(super) fn register_codex_session_tree_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    register_codex_session_tree_routes(registry, vec![source], selection)
}

pub(in crate::provider::source_backed) fn register_codex_session_tree_routes(
    registry: &mut SourceBackedProviderRegistry,
    sources: Vec<ProviderSource>,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    // The route sink does not expose its writer options yet. The production
    // path reserves the runtime-derived default indexer budget until a shared
    // scheduler supplies explicit scanner capacity through this seam.
    register_codex_session_tree_route_with_scan_control(
        registry,
        sources,
        selection,
        CodexSessionTreeScanControl {
            indexer_threads: WriterOptions::default().indexer_threads,
            scanner_workers: None,
            #[cfg(test)]
            counter_observer: None,
            #[cfg(test)]
            after_scan: None,
        },
    )
}

#[cfg(test)]
pub(in crate::provider::source_backed) fn register_codex_session_tree_route_for_test(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    scanner_workers: usize,
    observed_counters: Arc<Mutex<Vec<CodexSourceBackedCountersV0>>>,
    after_scan: Option<CodexSessionTreeAfterScan>,
) -> SourceBackedCoordinatorResult<()> {
    register_codex_session_tree_routes_for_test(
        registry,
        vec![source],
        selection,
        scanner_workers,
        observed_counters,
        after_scan,
    )
}

#[cfg(test)]
pub(in crate::provider::source_backed) fn register_codex_session_tree_routes_for_test(
    registry: &mut SourceBackedProviderRegistry,
    sources: Vec<ProviderSource>,
    selection: SourceBackedRouteSelection,
    scanner_workers: usize,
    observed_counters: Arc<Mutex<Vec<CodexSourceBackedCountersV0>>>,
    after_scan: Option<CodexSessionTreeAfterScan>,
) -> SourceBackedCoordinatorResult<()> {
    let observer: CodexSessionTreeCounterObserver = Arc::new(move |counters| {
        observed_counters.lock().unwrap().push(counters);
    });
    register_codex_session_tree_route_with_scan_control(
        registry,
        sources,
        selection,
        CodexSessionTreeScanControl {
            indexer_threads: 1,
            scanner_workers: Some(scanner_workers),
            counter_observer: Some(observer),
            after_scan,
        },
    )
}

fn register_codex_session_tree_route_with_scan_control(
    registry: &mut SourceBackedProviderRegistry,
    mut sources: Vec<ProviderSource>,
    selection: SourceBackedRouteSelection,
    scan_control: CodexSessionTreeScanControl,
) -> SourceBackedCoordinatorResult<()> {
    if sources.is_empty() {
        return Err(invalid_route(
            CaptureProvider::Codex,
            "Codex session-tree authority has no roots",
        ));
    }
    if sources.iter().any(|source| {
        source.provider != CaptureProvider::Codex
            || source.source_format != "codex_session_jsonl_tree"
    }) {
        return Err(invalid_route(
            CaptureProvider::Codex,
            "Codex session-tree authority contains a non-Codex root",
        ));
    }
    sources.sort_by(|left, right| {
        codex_session_root_rank(&left.path)
            .cmp(&codex_session_root_rank(&right.path))
            .then_with(|| left.path.cmp(&right.path))
    });
    sources.dedup_by(|left, right| left.path == right.path);
    let source = sources.first().cloned().ok_or_else(|| {
        invalid_route(CaptureProvider::Codex, "Codex session-tree root is absent")
    })?;
    let roots = sources
        .iter()
        .map(|source| source.path.clone())
        .collect::<Vec<_>>();
    let initial_inventory = discover_codex_route_inventory(&roots)
        .map_err(|error| invalid_route(CaptureProvider::Codex, error.to_string()))?;
    let runtime = Arc::new(
        CodexSessionTreeRuntime::new(&initial_inventory)
            .map_err(|error| invalid_route(CaptureProvider::Codex, error.to_string()))?,
    );
    let mut initial_ownership = CodexSessionTreeOwnership::default();
    for (_, source_key, _) in &initial_inventory.sources {
        initial_ownership.remember(source_key);
    }

    let capture_roots = Arc::new(roots);
    let complete_inventory_revalidation_roots = Arc::clone(&capture_roots);
    let capture_runtime = Arc::clone(&runtime);
    let hydration_runtime = Arc::clone(&runtime);
    let batch_hydration_runtime = runtime;
    let capture_initial_inventory = Arc::new(Mutex::new(Some(initial_inventory)));
    let ownership = Arc::new(Mutex::new(initial_ownership));
    let capture_ownership = Arc::clone(&ownership);
    let route_ownership = ownership;
    let terminal_evidence = Arc::new(Mutex::new(None::<CodexSessionTreeTerminalEvidence>));
    let capture_terminal_evidence = Arc::clone(&terminal_evidence);
    let source_terminal_evidence = Arc::clone(&terminal_evidence);
    let inventory_terminal_evidence = terminal_evidence;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            *capture_terminal_evidence.lock().map_err(|_| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "Codex terminal evidence lock was poisoned",
                )
            })? = None;
            let opening = capture_initial_inventory
                .lock()
                .map_err(|_| {
                    SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "Codex opening inventory lock was poisoned",
                    )
                })?
                .take()
                .map_or_else(
                    || discover_codex_route_inventory(&capture_roots),
                    Ok::<CodexSessionTreeInventoryV0, CodexSourceBackedErrorV0>,
                )
                .map_err(route_error)?;
            capture_runtime
                .install_inventory(&opening)
                .map_err(route_error)?;
            sink.certify_complete_inventory(opening.certificate.clone())
                .map_err(route_coordinator_error)?;
            let base_sources = codex_writer_base_sources(sink.writer);
            {
                let mut current = CodexSessionTreeOwnership::default();
                for (_, source_key, _) in &opening.sources {
                    current.remember(source_key);
                }
                for base in base_sources.values() {
                    let source = base.observation().source();
                    if managed_codex_session_source(source) {
                        current.remember(source);
                    }
                }
                if let Some(manifest) = sink.writer.base_manifest() {
                    for removal in &manifest.removals {
                        let source = removal.source();
                        if managed_codex_session_source(source) {
                            current.remember(source);
                        }
                    }
                }
                let mut owned = capture_ownership.lock().map_err(|_| {
                    SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "Codex source ownership lock was poisoned",
                    )
                })?;
                *owned = current;
            }
            for (_, source_key, _) in &opening.sources {
                sink.claim(source_key).map_err(route_coordinator_error)?;
            }
            let mut revalidation = HashMap::new();
            let mut timings = CodexSourceBackedPhaseTimingsV0::default();
            let mut counters = CodexSourceBackedCountersV0::default();
            ingest_codex_sources_v0(
                opening.sources.clone(),
                &base_sources,
                sink.writer,
                &mut revalidation,
                &mut timings,
                &mut counters,
                scan_control.indexer_threads,
                scan_control.scanner_workers,
            )
            .map_err(route_error)?;
            for base in base_sources.values() {
                let base_source = base.observation().source();
                if managed_codex_session_source(base_source)
                    && !opening.certificate.contains(base_source)
                {
                    sink.delete_source(
                        CertifiedSourceDeletion::from_inventory(
                            base_source.clone(),
                            &opening.certificate,
                        )
                        .map_err(route_error)?,
                        opening.certificate.clone(),
                    )
                    .map_err(route_coordinator_error)?;
                    counters.deleted_sources = counters.deleted_sources.saturating_add(1);
                }
            }
            *capture_terminal_evidence.lock().map_err(|_| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "Codex terminal evidence lock was poisoned",
                )
            })? = Some(CodexSessionTreeTerminalEvidence {
                inventory: opening.certificate,
                sources: revalidation,
            });
            #[cfg(test)]
            if let Some(observer) = &scan_control.counter_observer {
                observer(counters);
            }
            #[cfg(test)]
            if let Some(after_scan) = &scan_control.after_scan {
                after_scan();
            }
            Ok(())
        },
        move |candidate| {
            route_ownership
                .lock()
                .is_ok_and(|owned| owned.owns(candidate))
        },
        move |target| {
            let Ok(evidence) = source_terminal_evidence.lock() else {
                return false;
            };
            let Some(evidence) = evidence.as_ref() else {
                return false;
            };
            match target {
                SourceBackedRevalidationTarget::Source(expected) => evidence
                    .sources
                    .get(expected.observation().source())
                    .and_then(|evidence| {
                        codex_source_observation(
                            expected.observation().source(),
                            &evidence.observation,
                        )
                        .ok()
                    })
                    .is_some_and(|observation| observation == *expected.observation()),
                SourceBackedRevalidationTarget::Deletion(deletion) => {
                    deletion.verifies(&evidence.inventory)
                        && !evidence.sources.contains_key(deletion.source())
                }
            }
        },
        move |request| hydration_runtime.hydrate_event(request),
    )
    .with_complete_inventory_revalidation(move |expected| {
        let terminal = inventory_terminal_evidence
            .lock()
            .ok()
            .and_then(|evidence| evidence.clone());
        let Some(terminal) = terminal else {
            return false;
        };
        if terminal.inventory != *expected {
            return false;
        }
        let Ok(current) = discover_codex_route_inventory(&complete_inventory_revalidation_roots)
        else {
            return false;
        };
        current.certificate == *expected
            && current.sources.len() == terminal.sources.len()
            && current.sources.iter().all(|(source, source_key, _)| {
                terminal.sources.get(source_key).is_some_and(|certified| {
                    certified
                        .observation
                        .admits_append_only_growth(&source.catalog_observation)
                        && certified.revalidate()
                })
            })
    })
    .with_batch_hydration(move |request| batch_hydration_runtime.hydrate_batch(request));
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn codex_session_root_rank(root: &Path) -> u8 {
    match root.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("sessions") => 0,
        Some("archived_sessions") => 1,
        _ => 2,
    }
}

fn discover_codex_route_inventory(
    roots: &[PathBuf],
) -> CodexSourceBackedResultV0<CodexSessionTreeInventoryV0> {
    if let [root] = roots {
        let inventory = discover_codex_root_inventory_v0(root)?;
        return Ok(CodexSessionTreeInventoryV0 {
            sources: inventory.sources,
            certificate: inventory.certificate,
        });
    }
    discover_codex_session_tree_inventory_v0(roots)
}

pub(super) fn register_codex_explicit_session_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let input = CodexExplicitSessionSourceBackedInputV0::discover(&source.path)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let owned_source = input.source().clone();
    let scan_input = input.clone();
    let complete_inventory_revalidation_input = input.clone();
    let hydration_input = input.clone();
    let batch_hydration_input = input;
    let claimed_source = owned_source.clone();
    let terminal_evidence = Arc::new(Mutex::new(None::<CodexSessionTreeTerminalEvidence>));
    let capture_terminal_evidence = Arc::clone(&terminal_evidence);
    let source_terminal_evidence = Arc::clone(&terminal_evidence);
    let inventory_terminal_evidence = terminal_evidence;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            *capture_terminal_evidence.lock().map_err(|_| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "explicit Codex terminal evidence lock was poisoned",
                )
            })? = None;
            let opening = observe_codex_explicit_session_source_backed_v0(&scan_input)
                .map_err(route_error)?;
            let base = sink.base_source(&claimed_source).cloned();
            if opening.is_missing() {
                let closing = observe_codex_explicit_session_source_backed_v0(&scan_input)
                    .map_err(route_error)?;
                let inventory = opening.certify_against(&closing).map_err(route_error)?;
                sink.certify_complete_inventory(inventory.clone())
                    .map_err(route_coordinator_error)?;
                if let Some(base) = base {
                    let deletion = CertifiedSourceDeletion::from_inventory(
                        base.observation().source().clone(),
                        &inventory,
                    )
                    .map_err(route_error)?;
                    sink.delete_source(deletion, inventory.clone())
                        .map_err(route_coordinator_error)?;
                }
                *capture_terminal_evidence.lock().map_err(|_| {
                    SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "explicit Codex terminal evidence lock was poisoned",
                    )
                })? = Some(CodexSessionTreeTerminalEvidence {
                    inventory,
                    sources: HashMap::new(),
                });
                return Ok(());
            }

            let plan = opening.source_plan().ok_or_else(|| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "explicit Codex session disappeared after its opening observation",
                )
            })?;
            if !plan.1.exact_descriptor_eq(&claimed_source) {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "explicit Codex session changed its exact source identity",
                ));
            }
            sink.claim(&claimed_source)
                .map_err(route_coordinator_error)?;
            let base_sources = base
                .into_iter()
                .map(|base| (base.observation().source().clone(), base))
                .collect::<HashMap<_, _>>();
            let mut revalidation = HashMap::new();
            let mut timings = CodexSourceBackedPhaseTimingsV0::default();
            let mut counters = CodexSourceBackedCountersV0::default();
            ingest_codex_sources_serial_v0(
                vec![plan],
                &base_sources,
                sink.writer,
                &mut revalidation,
                &mut timings,
                &mut counters,
            )
            .map_err(route_error)?;
            #[cfg(test)]
            run_after_explicit_codex_stage_hook(counters);
            let closing = observe_codex_explicit_session_source_backed_v0(&scan_input)
                .map_err(route_error)?;
            let inventory = opening.certify_against(&closing).map_err(route_error)?;
            sink.certify_complete_inventory(inventory.clone())
                .map_err(route_coordinator_error)?;
            *capture_terminal_evidence.lock().map_err(|_| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "explicit Codex terminal evidence lock was poisoned",
                )
            })? = Some(CodexSessionTreeTerminalEvidence {
                inventory,
                sources: revalidation,
            });
            Ok(())
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| {
            let Ok(evidence) = source_terminal_evidence.lock() else {
                return false;
            };
            let Some(evidence) = evidence.as_ref() else {
                return false;
            };
            match target {
                SourceBackedRevalidationTarget::Source(expected) => evidence
                    .sources
                    .get(expected.observation().source())
                    .is_some_and(|source_evidence| {
                        codex_source_observation(
                            expected.observation().source(),
                            &source_evidence.observation,
                        )
                        .is_ok_and(|observation| observation == *expected.observation())
                            && source_evidence.revalidate()
                    }),
                SourceBackedRevalidationTarget::Deletion(deletion) => {
                    deletion.verifies(&evidence.inventory)
                        && !evidence.sources.contains_key(deletion.source())
                }
            }
        },
        move |request| hydrate_codex_explicit_event(&hydration_input, request),
    )
    .with_complete_inventory_revalidation(move |expected| {
        let terminal = inventory_terminal_evidence
            .lock()
            .ok()
            .and_then(|evidence| evidence.clone());
        let Some(terminal) = terminal else {
            return false;
        };
        if terminal.inventory != *expected {
            return false;
        }
        let Ok(opening) =
            observe_codex_explicit_session_source_backed_v0(&complete_inventory_revalidation_input)
        else {
            return false;
        };
        let Ok(closing) =
            observe_codex_explicit_session_source_backed_v0(&complete_inventory_revalidation_input)
        else {
            return false;
        };
        opening
            .certify_against(&closing)
            .is_ok_and(|current| current == *expected)
            && closing.source_plan().is_none_or(|(source, source_key, _)| {
                terminal.sources.get(&source_key).is_some_and(|evidence| {
                    evidence
                        .observation
                        .admits_append_only_growth(&source.catalog_observation)
                        && evidence.revalidate()
                })
            })
    })
    .with_batch_hydration(move |request| {
        hydrate_codex_explicit_batch(&batch_hydration_input, request)
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::ExplicitPath,
        driver,
    )?);
    Ok(())
}

fn hydrate_codex_explicit_event(
    input: &CodexExplicitSessionSourceBackedInputV0,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    codex_explicit_resolver(input)?
        .hydrate_event_request(request)
        .map_err(codex_locator_hydration_failure)
}

fn hydrate_codex_explicit_batch(
    input: &CodexExplicitSessionSourceBackedInputV0,
    request: &BatchHydrationRequest,
) -> Result<BatchHydrationResult, HydrationFailure> {
    codex_explicit_resolver(input)?
        .hydrate_batch_request(request)
        .map_err(codex_locator_hydration_failure)
}

fn codex_explicit_resolver(
    input: &CodexExplicitSessionSourceBackedInputV0,
) -> Result<CodexLocatorResolverV0, HydrationFailure> {
    let inventory = observe_codex_explicit_session_source_backed_v0(input)
        .map_err(codex_explicit_observation_hydration_failure)?;
    inventory
        .resolver()
        .map_err(codex_locator_hydration_failure)?
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "the explicit Codex session source is absent",
            )
        })
}

fn codex_explicit_observation_hydration_failure(
    error: CodexSourceBackedErrorV0,
) -> HydrationFailure {
    let kind = match &error {
        CodexSourceBackedErrorV0::ExplicitSourceIdentityChanged
        | CodexSourceBackedErrorV0::Capture(
            CaptureError::InvalidPayload(_) | CaptureError::SourceChangedDuringCapture,
        ) => HydrationFailureKind::StaleSourceEvidence,
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    hydration_failure(kind, error)
}

pub(in crate::provider::source_backed) fn codex_locator_hydration_failure(
    error: CodexSourceBackedErrorV0,
) -> HydrationFailure {
    let kind = match &error {
        CodexSourceBackedErrorV0::LocatorDigestMismatch
        | CodexSourceBackedErrorV0::LocatorRecordBoundaryMismatch => {
            HydrationFailureKind::StaleRecordEvidence
        }
        CodexSourceBackedErrorV0::LocatorRangeMissing => HydrationFailureKind::MissingRecord,
        CodexSourceBackedErrorV0::ExplicitSourceIdentityChanged
        | CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture
            | CaptureError::InvalidProviderTranscriptPath { .. },
        ) => HydrationFailureKind::StaleSourceEvidence,
        // A generation-bound locator can outlive a temporarily missing source
        // between refreshes. Only an authoritative refresh may certify
        // deletion, so absence from the current provider catalog is
        // unavailable rather than an invalid locator.
        CodexSourceBackedErrorV0::LocatorSourceNotFound(_) => {
            HydrationFailureKind::TemporarilyUnavailable
        }
        CodexSourceBackedErrorV0::InvalidCodexLocator
        | CodexSourceBackedErrorV0::LocatorRangeTooLarge
        | CodexSourceBackedErrorV0::LocatorEventMismatch => HydrationFailureKind::InvalidLocator,
        CodexSourceBackedErrorV0::LocatorRecordNotDisplayable => {
            HydrationFailureKind::UnsupportedParserRevision
        }
        CodexSourceBackedErrorV0::Json(_) => HydrationFailureKind::StaleRecordEvidence,
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    hydration_failure(kind, error)
}

// SHA-256("ctx.codex.prompt-history.default-catalog-lineage.v0"). This is
// catalog-route identity, not a digest of the user-specific source path.
pub(in crate::provider::source_backed) const CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0: [u8;
    32] = [
    0x2d, 0x2e, 0xb3, 0x41, 0xde, 0xe9, 0x7a, 0xd3, 0x15, 0xec, 0xfa, 0xb3, 0x33, 0x20, 0x7c, 0x44,
    0x53, 0x18, 0xb9, 0x32, 0x1c, 0xc1, 0x6b, 0xf2, 0x2c, 0xdb, 0x09, 0x68, 0xe0, 0xf1, 0xf5, 0x0a,
];

/// Registers Codex's one default prompt-history catalog route while retaining
/// the opened ordinary-file authority for scanning, revalidation, and exact
/// hydration. The selected path never participates in public source identity.
pub fn register_codex_prompt_history_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let input = CodexPromptHistorySourceBackedInputV0::explicit(
        source.path.clone(),
        CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0,
    );
    let retained = observe_codex_prompt_history_source_backed_explicit_v0(&input)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let owned_source = retained.source().clone();
    let capture_source = retained.clone();
    let terminal_source = retained.clone();
    let hydration_resolver = Arc::new(
        CodexPromptHistorySourceBackedResolverV0::new([retained])
            .map_err(|error| invalid_route(source.provider, error.to_string()))?,
    );
    let claimed_source = owned_source.clone();
    let capture_route = source.clone();
    let terminal_route = source.clone();
    let terminal_evidence = Arc::new(Mutex::new(None::<CodexPromptTerminalEvidence>));
    let scan_terminal_evidence = Arc::clone(&terminal_evidence);
    let source_terminal_evidence = Arc::clone(&terminal_evidence);
    let inventory_terminal_evidence = terminal_evidence;
    let terminal_capture: Arc<CodexPromptTerminalCapture> = Arc::new(move |expected| {
        revalidate_codex_prompt_history_source_backed_v0(&terminal_source, expected)
            .map_err(route_error)?;
        certify_source_inventory(&terminal_route, std::slice::from_ref(expected))
    });
    let inventory_terminal_capture = terminal_capture;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            *scan_terminal_evidence.lock().map_err(|_| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "Codex prompt-history terminal evidence lock was poisoned",
                )
            })? = None;
            let base = sink.base_source(&claimed_source).cloned();
            let Some(base) = base else {
                sink.begin_source(claimed_source.clone())
                    .map_err(route_coordinator_error)?;
                let scan = scan_codex_prompt_history_source_backed_v0(
                    capture_source.clone(),
                    None,
                    |page| {
                        if !page.source.exact_descriptor_eq(&claimed_source) {
                            return Err(CaptureError::InvalidPayload(
                                "Codex prompt-history page changed its source descriptor"
                                    .to_owned(),
                            )
                            .into());
                        }
                        let _retained_page_bytes = page.retained_bytes;
                        for document in page.documents {
                            sink.add_document(document)
                                .map_err(capture_coordinator_error)?;
                        }
                        Ok(())
                    },
                )
                .map_err(route_error)?;
                if !matches!(
                    scan.disposition,
                    CodexPromptHistorySourceBackedDispositionV0::Cold
                ) || !scan.source.source().exact_descriptor_eq(&claimed_source)
                    || scan.certificate.counts().indexed_documents != scan.emitted_documents
                {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "Codex prompt-history cold scan did not reconcile",
                    ));
                }
                sink.certify_source(scan.certificate.clone())
                    .map_err(route_coordinator_error)?;
                let certificate = scan.certificate;
                let inventory =
                    certify_source_inventory(&capture_route, std::slice::from_ref(&certificate))?;
                sink.certify_complete_inventory(inventory.clone())
                    .map_err(route_coordinator_error)?;
                remember_codex_prompt_terminal(&scan_terminal_evidence, certificate, inventory)?;
                return Ok(());
            };

            let planned =
                plan_codex_prompt_history_source_backed_v0(capture_source.clone(), Some(&base))
                    .map_err(route_error)?;
            if !planned.source.source().exact_descriptor_eq(&claimed_source) {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Codex prompt-history replay changed its source descriptor",
                ));
            }
            let certificate = match planned.disposition {
                CodexPromptHistorySourceBackedDispositionV0::Unchanged => {
                    if planned.certificate != base || planned.emitted_documents != 0 {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Codex prompt-history unchanged evidence did not match its base",
                        ));
                    }
                    let frontier = base.frontier().ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            "Codex prompt-history replay base has no append frontier",
                        )
                    })?;
                    sink.begin_source_append(claimed_source.clone())
                        .map_err(route_coordinator_error)?;
                    let append = CertifiedSourceAppend::certify(
                        &base,
                        planned.certificate.clone(),
                        frontier.certified_prefix_bytes(),
                        *frontier.certified_prefix_digest(),
                    )
                    .map_err(route_error)?;
                    sink.certify_source_append(append)
                        .map_err(route_coordinator_error)?;
                    planned.certificate
                }
                CodexPromptHistorySourceBackedDispositionV0::Append
                | CodexPromptHistorySourceBackedDispositionV0::Replacement => {
                    let is_append = matches!(
                        planned.disposition,
                        CodexPromptHistorySourceBackedDispositionV0::Append
                    );
                    if is_append {
                        sink.begin_source_append(claimed_source.clone())
                            .map_err(route_coordinator_error)?;
                    } else {
                        sink.begin_source(claimed_source.clone())
                            .map_err(route_coordinator_error)?;
                    }
                    let scan = stage_planned_codex_prompt_history_source_backed_v0(
                        capture_source.clone(),
                        Some(&base),
                        &planned,
                        |page| {
                            if !page.source.exact_descriptor_eq(&claimed_source) {
                                return Err(CaptureError::InvalidPayload(
                                    "Codex prompt-history page changed its source descriptor"
                                        .to_owned(),
                                )
                                .into());
                            }
                            let _retained_page_bytes = page.retained_bytes;
                            for document in page.documents {
                                sink.add_document(document)
                                    .map_err(capture_coordinator_error)?;
                            }
                            Ok(())
                        },
                    )
                    .map_err(route_error)?;
                    if scan.certificate != planned.certificate
                        || std::mem::discriminant(&scan.disposition)
                            != std::mem::discriminant(&planned.disposition)
                    {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Codex prompt-history changed between planning and staging",
                        ));
                    }
                    if is_append {
                        let frontier = base.frontier().ok_or_else(|| {
                            SourceBackedRouteError::new(
                                SourceBackedRouteErrorKind::Internal,
                                "Codex prompt-history append base has no frontier",
                            )
                        })?;
                        let append = CertifiedSourceAppend::certify(
                            &base,
                            scan.certificate.clone(),
                            frontier.certified_prefix_bytes(),
                            *frontier.certified_prefix_digest(),
                        )
                        .map_err(route_error)?;
                        sink.certify_source_append(append)
                            .map_err(route_coordinator_error)?;
                    } else {
                        sink.certify_source(scan.certificate.clone())
                            .map_err(route_coordinator_error)?;
                    }
                    scan.certificate
                }
                CodexPromptHistorySourceBackedDispositionV0::Cold => {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "Codex prompt-history replay unexpectedly returned a cold disposition",
                    ));
                }
            };
            let inventory =
                certify_source_inventory(&capture_route, std::slice::from_ref(&certificate))?;
            sink.certify_complete_inventory(inventory.clone())
                .map_err(route_coordinator_error)?;
            remember_codex_prompt_terminal(&scan_terminal_evidence, certificate, inventory)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| bind_codex_prompt_target(&source_terminal_evidence, target),
        move |request| hydration_resolver.hydrate_event(request),
    )
    .with_complete_inventory_revalidation(move |expected| {
        revalidate_codex_prompt_inventory(
            &inventory_terminal_evidence,
            inventory_terminal_capture.as_ref(),
            expected,
        )
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

type CodexPromptTerminalCapture = dyn Fn(&CertifiedSource) -> Result<CertifiedSourceInventory, SourceBackedRouteError>
    + Send
    + Sync;

struct CodexPromptTerminalEvidence {
    certificate: CertifiedSource,
    inventory: CertifiedSourceInventory,
}

fn remember_codex_prompt_terminal(
    state: &Mutex<Option<CodexPromptTerminalEvidence>>,
    certificate: CertifiedSource,
    inventory: CertifiedSourceInventory,
) -> SourceBackedRouteResult<()> {
    let mut state = state.lock().map_err(|_| {
        SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "Codex prompt-history terminal evidence lock was poisoned",
        )
    })?;
    *state = Some(CodexPromptTerminalEvidence {
        certificate,
        inventory,
    });
    Ok(())
}

fn bind_codex_prompt_target(
    state: &Mutex<Option<CodexPromptTerminalEvidence>>,
    target: SourceBackedRevalidationTarget<'_>,
) -> bool {
    let Ok(state) = state.lock() else {
        return false;
    };
    let Some(expected) = state.as_ref() else {
        return false;
    };
    match target {
        SourceBackedRevalidationTarget::Source(source) => expected.certificate == *source,
        SourceBackedRevalidationTarget::Deletion(deletion) => {
            deletion.verifies(&expected.inventory)
                && !expected
                    .certificate
                    .observation()
                    .source()
                    .exact_descriptor_eq(deletion.source())
        }
    }
}

fn revalidate_codex_prompt_inventory(
    state: &Mutex<Option<CodexPromptTerminalEvidence>>,
    capture: &CodexPromptTerminalCapture,
    expected_inventory: &CertifiedSourceInventory,
) -> bool {
    let Ok(state) = state.lock() else {
        return false;
    };
    let Some(expected) = state.as_ref() else {
        return false;
    };
    if expected.inventory != *expected_inventory {
        return false;
    }
    capture(&expected.certificate).is_ok_and(|inventory| inventory == expected.inventory)
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;

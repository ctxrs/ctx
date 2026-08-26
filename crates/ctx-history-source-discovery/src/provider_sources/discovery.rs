use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    thread,
};

use ctx_history_core::CaptureProvider;
use ctx_history_platform::platform_security::validate_provider_source_outside_data_root;
use thiserror::Error;

use super::{
    configured_roots::expand_configured_roots_for_provider,
    context::DiscoveryContext,
    resolvers::{dedupe_report, resolve},
    specs::PROVIDER_SPECS,
    types::{DiscoveryReport, ProviderSource, ProviderSourceStatus},
    StaticProviderProbeCatalog,
};

const MAX_PROJECT_DISCOVERY_LOCATORS: usize = 128;
const MAX_PROVIDER_DISCOVERY_WORKERS: usize = 16;
const PROVIDER_DISCOVERY_THREAD_PREFIX: &str = "ctx-src-disc";
const OPENHANDS_AUTOMATIC_CONFIGURED_OVERLAP_REASON: &str =
    "configured OpenHands history root conflicts with an active automatic OpenHands root";

#[derive(Debug, Error)]
#[error(
    "provider source root {source_root:?} is unsafe relative to ctx data root {data_root:?}: {detail}; choose or move ctx --data-root outside every provider root, or correct the explicit provider path"
)]
pub struct ProviderSourceRootBoundaryError {
    pub data_root: PathBuf,
    pub source_root: PathBuf,
    pub detail: String,
}

/// Read-only preflight for provider roots before route handles, watchers, or
/// persistent ctx state are created.
pub fn validate_provider_source_roots_outside_data_root<'a>(
    data_root: &Path,
    sources: impl IntoIterator<Item = &'a ProviderSource>,
) -> Result<(), ProviderSourceRootBoundaryError> {
    for source in sources {
        let boundary_root = provider_source_boundary_root(source);
        validate_provider_source_outside_data_root(data_root, boundary_root).map_err(|error| {
            ProviderSourceRootBoundaryError {
                data_root: data_root.to_path_buf(),
                source_root: source.path.clone(),
                detail: error.to_string(),
            }
        })?;
    }
    Ok(())
}

fn provider_source_boundary_root(source: &ProviderSource) -> &Path {
    source
        .route_provenance
        .configured_root()
        .filter(|_| source.status == ProviderSourceStatus::Unknown)
        .map_or(&source.path, |(_, root_path)| root_path)
}

mod explicit;
pub use explicit::{provider_source_for_path, provider_source_for_path_with_data_root};

pub fn discover_provider_sources(
    probes: &StaticProviderProbeCatalog,
    home: &Path,
) -> Vec<ProviderSource> {
    discover_provider_sources_report(probes, home).sources
}

pub fn discover_provider_sources_report(
    probes: &StaticProviderProbeCatalog,
    home: &Path,
) -> DiscoveryReport {
    discover_provider_sources_with_context(probes, &DiscoveryContext::from_process(home))
}

pub fn discover_provider_sources_with_context(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    for spec in PROVIDER_SPECS {
        let mut provider_report = resolve_provider(probes, context, spec);
        report.sources.append(&mut provider_report.sources);
        report.issues.append(&mut provider_report.issues);
    }
    dedupe_report(report)
}

/// Resolves canonical automatic routes only for providers with configured roots.
///
/// Configured roots use this read-only view to retain the released identity
/// when their complete expansion is the canonical automatic route set. The
/// comparison deliberately enables automatic inference even when automatic
/// refresh is disabled, so configuration does not fork released identities.
/// Providers without named roots are never resolved or content-probed by this
/// identity-reconstruction path.
pub fn discover_canonical_automatic_provider_sources_with_context(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
) -> DiscoveryReport {
    let automatic_context = context.clone().with_automatic_provider_discovery(true);
    let mut report = DiscoveryReport::default();
    for spec in PROVIDER_SPECS {
        if !context
            .configured_provider_roots()
            .iter()
            .any(|root| root.provider == spec.provider)
        {
            continue;
        }
        let mut provider_report = resolve(probes, &automatic_context, spec);
        report.sources.append(&mut provider_report.sources);
        report.issues.append(&mut provider_report.issues);
    }
    dedupe_report(report)
}

/// Resolves independent provider specifications concurrently under the
/// caller's refresh-wide worker limit, then merges reports in specification
/// order so scheduling cannot affect discovery or publication identity.
pub fn discover_provider_sources_with_context_and_work_budget(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    worker_limit: usize,
) -> DiscoveryReport {
    let reports = bounded_ordered_map(PROVIDER_SPECS.len(), worker_limit, |index| {
        resolve_provider(probes, context, &PROVIDER_SPECS[index])
    });
    let mut report = DiscoveryReport::default();
    for mut provider_report in reports {
        report.sources.append(&mut provider_report.sources);
        report.issues.append(&mut provider_report.issues);
    }
    dedupe_report(report)
}

fn bounded_ordered_map<T, F>(job_count: usize, worker_limit: usize, operation: F) -> Vec<T>
where
    T: Send,
    F: Fn(usize) -> T + Sync,
{
    if job_count == 0 {
        return Vec::new();
    }
    let worker_count = worker_limit
        .max(1)
        .min(job_count)
        .min(MAX_PROVIDER_DISCOVERY_WORKERS);
    if worker_count == 1 {
        return (0..job_count).map(operation).collect();
    }

    let next_job = AtomicUsize::new(0);
    let results = Mutex::new(
        std::iter::repeat_with(|| None)
            .take(job_count)
            .collect::<Vec<Option<T>>>(),
    );
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count.saturating_sub(1));
        for worker_index in 1..worker_count {
            let operation = &operation;
            let next_job = &next_job;
            let results = &results;
            let spawn = thread::Builder::new()
                .name(format!(
                    "{PROVIDER_DISCOVERY_THREAD_PREFIX}{worker_index:02}"
                ))
                .spawn_scoped(scope, move || {
                    run_ordered_jobs(job_count, next_job, results, operation)
                });
            if let Ok(handle) = spawn {
                handles.push(handle);
            }
        }
        run_ordered_jobs(job_count, &next_job, &results, &operation);
        for handle in handles {
            // A panicking resolver leaves its claimed slot empty. Discovery is
            // side-effect free, so the caller retries only that missing slot.
            let _ = handle.join();
        }
    });

    let slots = results
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    slots
        .into_iter()
        .enumerate()
        .map(|(index, result)| result.unwrap_or_else(|| operation(index)))
        .collect()
}

fn run_ordered_jobs<T, F>(
    job_count: usize,
    next_job: &AtomicUsize,
    results: &Mutex<Vec<Option<T>>>,
    operation: &F,
) where
    T: Send,
    F: Fn(usize) -> T + Sync,
{
    loop {
        let index = next_job.fetch_add(1, Ordering::Relaxed);
        if index >= job_count {
            return;
        }
        let result = operation(index);
        let mut slots = results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        slots[index] = Some(result);
    }
}

/// Discovers official provider roots across an explicit bounded set of activity locators.
///
/// The locators must come from captured activity or explicit authorization. This compatibility
/// entry point does not search for repositories. Each locator is evaluated through the same
/// frozen discovery context and the combined result is deduplicated.
pub fn discover_provider_sources_with_projects(
    probes: &StaticProviderProbeCatalog,
    home: &Path,
    project_locators: &[PathBuf],
) -> Vec<ProviderSource> {
    discover_with_projects(probes, home, project_locators, None).sources
}

pub fn discover_provider_sources_for_provider(
    probes: &StaticProviderProbeCatalog,
    home: &Path,
    provider: CaptureProvider,
) -> Vec<ProviderSource> {
    discover_provider_sources_for_provider_report(probes, home, provider).sources
}

pub fn discover_provider_sources_for_provider_report(
    probes: &StaticProviderProbeCatalog,
    home: &Path,
    provider: CaptureProvider,
) -> DiscoveryReport {
    discover_provider_sources_for_provider_with_context(
        probes,
        &DiscoveryContext::from_process(home),
        provider,
    )
}

pub fn discover_provider_sources_for_provider_with_context(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    provider: CaptureProvider,
) -> DiscoveryReport {
    let report = PROVIDER_SPECS
        .iter()
        .find(|spec| spec.provider == provider)
        .map_or_else(DiscoveryReport::default, |spec| {
            resolve_provider(probes, context, spec)
        });
    dedupe_report(report)
}

fn resolve_provider(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &super::types::ProviderSourceSpec,
) -> DiscoveryReport {
    let mut report = resolve(probes, context, spec);
    let mut configured = expand_configured_roots_for_provider(probes, context, spec);
    let canonical = if configured.sources.is_empty() {
        Vec::new()
    } else if context.automatic_provider_inference_enabled() {
        report.sources.clone()
    } else {
        resolve(
            probes,
            &context.clone().with_automatic_provider_discovery(true),
            spec,
        )
        .sources
    };
    if spec.provider == CaptureProvider::OpenHands {
        suppress_openhands_automatic_configured_overlaps(&report.sources, &mut configured);
    }
    preserve_matching_automatic_route_roles(&mut configured.sources, &canonical);
    report.sources.append(&mut configured.sources);
    report.issues.append(&mut configured.issues);
    report
}

fn suppress_openhands_automatic_configured_overlaps(
    automatic: &[ProviderSource],
    configured: &mut DiscoveryReport,
) {
    let mut conflicting_root_ids = HashSet::new();
    let mut conflicting_paths = Vec::new();
    for source in &configured.sources {
        if !source.exists
            || !automatic.iter().any(|automatic| {
                automatic.exists
                    && openhands_automatic_configured_sources_conflict(automatic, source)
            })
        {
            continue;
        }
        let Some((root_id, _)) = source.route_provenance.configured_root() else {
            continue;
        };
        if conflicting_root_ids.insert(root_id.to_owned()) {
            conflicting_paths.push(source.path.clone());
        }
    }

    configured.sources.retain(|source| {
        source
            .route_provenance
            .configured_root()
            .is_none_or(|(root_id, _)| !conflicting_root_ids.contains(root_id))
    });
    configured
        .issues
        .extend(conflicting_paths.into_iter().map(|path| {
            super::resolvers::issue(
                CaptureProvider::OpenHands,
                Some(path),
                super::types::DiscoveryIssueKind::ConfiguredRootConflict,
                OPENHANDS_AUTOMATIC_CONFIGURED_OVERLAP_REASON,
            )
        }));
}

fn openhands_automatic_configured_sources_conflict(
    automatic: &ProviderSource,
    configured: &ProviderSource,
) -> bool {
    if super::resolvers::provider_paths_equivalent(&automatic.path, &configured.path) {
        return automatic.source_format != configured.source_format;
    }
    provider_paths_strictly_nested(&automatic.path, &configured.path)
}

fn provider_paths_strictly_nested(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left.starts_with(&right) || right.starts_with(&left)
}

fn preserve_matching_automatic_route_roles(
    configured: &mut [ProviderSource],
    canonical: &[ProviderSource],
) {
    for source in configured {
        let Some(automatic) = canonical.iter().find(|automatic| {
            automatic.provider == source.provider
                && automatic.source_format == source.source_format
                && super::resolvers::provider_paths_equivalent(&automatic.path, &source.path)
        }) else {
            continue;
        };
        if let ctx_history_capture_model::ProviderSourceRouteProvenance::ConfiguredRoot {
            automatic_route_role,
            ..
        } = &mut source.route_provenance
        {
            *automatic_route_role = automatic.route_provenance.automatic_route_role().cloned();
        }
    }
}

/// Provider-scoped counterpart to discover_provider_sources_with_projects.
pub fn discover_provider_sources_for_provider_with_projects(
    probes: &StaticProviderProbeCatalog,
    home: &Path,
    provider: CaptureProvider,
    project_locators: &[PathBuf],
) -> Vec<ProviderSource> {
    discover_with_projects(probes, home, project_locators, Some(provider)).sources
}

fn discover_with_projects(
    probes: &StaticProviderProbeCatalog,
    home: &Path,
    project_locators: &[PathBuf],
    provider: Option<CaptureProvider>,
) -> DiscoveryReport {
    let base = DiscoveryContext::from_process(home);
    let mut report = DiscoveryReport::default();
    let mut seen = HashSet::new();
    let locators = project_locators
        .iter()
        .filter(|locator| seen.insert((*locator).clone()))
        .take(MAX_PROJECT_DISCOVERY_LOCATORS)
        .map(|locator| Some(locator.clone()))
        .collect::<Vec<_>>();
    let locators = if locators.is_empty() {
        vec![None]
    } else {
        locators
    };

    for locator in locators {
        let context = base.clone().with_cwd(locator);
        let mut next = provider.map_or_else(
            || discover_provider_sources_with_context(probes, &context),
            |provider| {
                discover_provider_sources_for_provider_with_context(probes, &context, provider)
            },
        );
        report.sources.append(&mut next.sources);
        report.issues.append(&mut next.issues);
    }
    dedupe_report(report)
}

#[cfg(test)]
mod boundary_error_tests {
    use super::*;

    static CURSOR_PROBE_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn counting_cursor_probe(
        _path: &Path,
    ) -> crate::provider_sources::CursorTranscriptProbeOutcome {
        CURSOR_PROBE_CALLS.fetch_add(1, Ordering::Relaxed);
        crate::provider_sources::CursorTranscriptProbeOutcome::NotFound
    }

    fn unknown_configured_claude_source(home: &Path) -> ProviderSource {
        ProviderSource {
            provider: CaptureProvider::Claude,
            path: home.join("projects"),
            exists: true,
            source_format: "claude_projects_jsonl_tree",
            source_kind: super::super::types::ProviderSourceKind::NativeHistory,
            import_support: super::super::types::ProviderImportSupport::Native,
            catalog_support: super::super::types::ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Unknown,
            unsupported_reason: Some("configured home unavailable"),
            route_provenance:
                ctx_history_capture_model::ProviderSourceRouteProvenance::ConfiguredRoot {
                    root_id: "fixture".to_owned(),
                    root_path: home.to_path_buf(),
                    route_role: ctx_history_capture_model::ProviderRouteRole::from_static(
                        "claude-projects",
                    ),
                    automatic_route_role: None,
                },
        }
    }

    #[test]
    fn public_boundary_error_names_a_concrete_recovery() {
        let error = ProviderSourceRootBoundaryError {
            data_root: PathBuf::from("/ctx-data"),
            source_root: PathBuf::from("/provider"),
            detail: "the roots overlap".to_owned(),
        };
        let rendered = error.to_string();
        assert!(rendered.contains("choose or move ctx --data-root"));
        assert!(rendered.contains("correct the explicit provider path"));
    }

    #[test]
    fn unknown_configured_child_validates_the_home_without_weakening_overlap_rejection() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let home = temp.path().join("claude-home");
        std::fs::create_dir_all(&home).unwrap();
        let source = unknown_configured_claude_source(&home);
        assert_eq!(provider_source_boundary_root(&source), home);

        let disjoint_data = temp.path().join("ctx-data");
        validate_provider_source_roots_outside_data_root(&disjoint_data, [&source]).unwrap();
        let nested_data = home.join("ctx-data");
        assert!(
            validate_provider_source_roots_outside_data_root(&nested_data, [&source]).is_err(),
            "configured-home fallback must still reject a nested ctx data root"
        );
    }

    #[test]
    fn automatic_false_probes_only_providers_with_configured_roots() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = temp.path().join("cwd");
        let claude = home.join(".claude");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(claude.join("projects")).unwrap();
        std::fs::write(claude.join("projects/session.jsonl"), b"{}\n").unwrap();
        std::fs::create_dir_all(home.join(".cursor/projects")).unwrap();
        let probes = crate::provider_sources::StaticProviderProbeCatalog::new(
            crate::provider_sources::CursorProbeFragment::new(counting_cursor_probe),
        );
        let context = DiscoveryContext::new(
            &home,
            &cwd,
            crate::provider_sources::DiscoveryPlatform::Linux,
            crate::provider_sources::DiscoveryPlatformDirs::default(),
        )
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![
            ctx_history_capture_model::ProviderRootDefinition {
                id: "released-claude".to_owned(),
                provider: CaptureProvider::Claude,
                path: claude,
                group: None,
                kind: None,
            },
        ]);

        CURSOR_PROBE_CALLS.store(0, Ordering::Relaxed);
        super::super::probes::reset_default_location_probe_calls();
        let report = discover_provider_sources_with_context(&probes, &context);

        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.sources[0].provider, CaptureProvider::Claude);
        assert_eq!(CURSOR_PROBE_CALLS.load(Ordering::Relaxed), 0);
        assert_eq!(super::super::probes::default_location_probe_calls(), 2);
    }

    #[test]
    fn bounded_discovery_work_is_concurrent_and_returns_input_order() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let workers = std::sync::Arc::new(Mutex::new(HashSet::new()));
        let barrier_for_jobs = std::sync::Arc::clone(&barrier);
        let workers_for_jobs = std::sync::Arc::clone(&workers);

        let results = bounded_ordered_map(4, 4, move |index| {
            workers_for_jobs
                .lock()
                .unwrap()
                .insert(std::thread::current().id());
            barrier_for_jobs.wait();
            index
        });

        assert_eq!(results, [0, 1, 2, 3]);
        assert_eq!(workers.lock().unwrap().len(), 4);
    }

    #[test]
    fn bounded_discovery_work_clamps_zero_and_oversized_limits() {
        assert_eq!(bounded_ordered_map(3, 0, |index| index), [0, 1, 2]);
        let names = std::sync::Arc::new(Mutex::new(HashSet::new()));
        let names_for_jobs = std::sync::Arc::clone(&names);
        let _ = bounded_ordered_map(MAX_PROVIDER_DISCOVERY_WORKERS + 4, usize::MAX, move |_| {
            names_for_jobs
                .lock()
                .unwrap()
                .insert(std::thread::current().name().map(str::to_owned));
        });
        assert!(names.lock().unwrap().len() <= MAX_PROVIDER_DISCOVERY_WORKERS);
    }
}

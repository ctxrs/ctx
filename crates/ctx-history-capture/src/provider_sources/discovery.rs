use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    thread,
};

use ctx_history_core::platform_security::validate_provider_source_outside_data_root;
use ctx_history_core::CaptureProvider;
use thiserror::Error;

use super::{
    context::DiscoveryContext,
    resolvers::{dedupe_report, resolve},
    specs::PROVIDER_SPECS,
    types::{DiscoveryReport, ProviderSource},
};

const MAX_PROJECT_DISCOVERY_LOCATORS: usize = 128;
const MAX_PROVIDER_DISCOVERY_WORKERS: usize = 16;
const PROVIDER_DISCOVERY_THREAD_PREFIX: &str = "ctx-src-disc";

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
        validate_provider_source_outside_data_root(data_root, &source.path).map_err(|error| {
            ProviderSourceRootBoundaryError {
                data_root: data_root.to_path_buf(),
                source_root: source.path.clone(),
                detail: error.to_string(),
            }
        })?;
    }
    Ok(())
}

mod explicit;
pub use explicit::provider_source_for_path;

pub fn discover_provider_sources(home: &Path) -> Vec<ProviderSource> {
    discover_provider_sources_report(home).sources
}

pub fn discover_provider_sources_report(home: &Path) -> DiscoveryReport {
    discover_provider_sources_with_context(&DiscoveryContext::from_process(home))
}

pub fn discover_provider_sources_with_context(context: &DiscoveryContext) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    for spec in PROVIDER_SPECS {
        let mut provider_report = resolve(context, spec);
        report.sources.append(&mut provider_report.sources);
        report.issues.append(&mut provider_report.issues);
    }
    dedupe_report(report)
}

/// Resolves independent provider specifications concurrently under the
/// caller's refresh-wide worker limit, then merges reports in specification
/// order so scheduling cannot affect discovery or publication identity.
pub fn discover_provider_sources_with_context_and_work_budget(
    context: &DiscoveryContext,
    worker_limit: usize,
) -> DiscoveryReport {
    let reports = bounded_ordered_map(PROVIDER_SPECS.len(), worker_limit, |index| {
        resolve(context, &PROVIDER_SPECS[index])
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
    home: &Path,
    project_locators: &[PathBuf],
) -> Vec<ProviderSource> {
    discover_with_projects(home, project_locators, None).sources
}

pub fn discover_provider_sources_for_provider(
    home: &Path,
    provider: CaptureProvider,
) -> Vec<ProviderSource> {
    discover_provider_sources_for_provider_report(home, provider).sources
}

pub fn discover_provider_sources_for_provider_report(
    home: &Path,
    provider: CaptureProvider,
) -> DiscoveryReport {
    discover_provider_sources_for_provider_with_context(
        &DiscoveryContext::from_process(home),
        provider,
    )
}

pub fn discover_provider_sources_for_provider_with_context(
    context: &DiscoveryContext,
    provider: CaptureProvider,
) -> DiscoveryReport {
    let report = PROVIDER_SPECS
        .iter()
        .find(|spec| spec.provider == provider)
        .map_or_else(DiscoveryReport::default, |spec| resolve(context, spec));
    dedupe_report(report)
}

/// Provider-scoped counterpart to discover_provider_sources_with_projects.
pub fn discover_provider_sources_for_provider_with_projects(
    home: &Path,
    provider: CaptureProvider,
    project_locators: &[PathBuf],
) -> Vec<ProviderSource> {
    discover_with_projects(home, project_locators, Some(provider)).sources
}

fn discover_with_projects(
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
            || discover_provider_sources_with_context(&context),
            |provider| discover_provider_sources_for_provider_with_context(&context, provider),
        );
        report.sources.append(&mut next.sources);
        report.issues.append(&mut next.issues);
    }
    dedupe_report(report)
}

#[cfg(test)]
mod boundary_error_tests {
    use super::*;

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

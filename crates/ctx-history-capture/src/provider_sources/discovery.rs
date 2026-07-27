use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;

use super::{
    context::DiscoveryContext,
    resolvers::{dedupe_report, resolve},
    specs::PROVIDER_SPECS,
    types::{DiscoveryReport, ProviderSource},
};

const MAX_PROJECT_DISCOVERY_LOCATORS: usize = 128;

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

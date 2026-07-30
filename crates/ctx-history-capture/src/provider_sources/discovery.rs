use std::{
    collections::HashSet,
    path::{Path, PathBuf},
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
}

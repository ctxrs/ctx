use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use ctx_history_capture::{
    DiscoveryIssue, ProviderSource, ProviderSourceRouteProvenance, ProviderSourceStatus,
};
use ctx_history_read_application::{HistoryHealthReport, HistoryRootCoverage};

use ctx_app_config as config;

/// Joins the verified-generation view to one current, bounded discovery
/// inventory for human presentation. This does not alter command JSON or
/// refresh receipts, and it never treats an undiscovered root as indexed.
pub(super) fn reconcile_history_inventory(
    health: &mut Option<HistoryHealthReport>,
    data_root: &Path,
    config: &config::AppConfig,
) -> Result<()> {
    let Some(health) = health.as_mut() else {
        return Ok(());
    };
    let home = crate::identity::home_dir();
    let configured_roots = config.provider_root_definitions();
    if home.is_none() && configured_roots.is_empty() {
        health.record_inventory(
            HistoryRootCoverage {
                unknown: 1,
                ..HistoryRootCoverage::default()
            },
            None,
        );
        return Ok(());
    }

    let discovery = ctx_history_cli::discovered_sources_report_with_data_root_and_provider_roots(
        home.as_deref(),
        data_root,
        config.automatic_source_discovery_enabled(),
        &configured_roots,
    );
    let mut roots = HashMap::<RootIdentity, RootState>::new();
    let roots_by_provider_path = source_root_lookup(&discovery.sources);
    for source in &discovery.sources {
        let state = roots.entry(source_root_identity(source)).or_default();
        record_source_coverage(state, source);
    }
    for issue in &discovery.issues {
        roots
            .entry(issue_root_identity(
                issue,
                &roots_by_provider_path,
                &configured_roots,
            ))
            .or_default()
            .unknown = true;
    }

    let coverage = root_coverage(&roots)?;
    let bytes_reconciled = coverage.partial == 0 && coverage.excluded == 0 && coverage.unknown == 0;
    let excluded_bytes =
        (bytes_reconciled && health.source_failures == 0 && health.rejected_records == 0)
            .then_some(0);
    health.record_inventory(coverage, excluded_bytes);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RootIdentity {
    Configured(String),
    Automatic {
        provider: String,
        role: Vec<u8>,
    },
    Location {
        provider: String,
        path: std::path::PathBuf,
    },
    Unresolved(String),
}

type SourceRootLookup = HashMap<(String, PathBuf), RootIdentity>;

#[derive(Debug, Default)]
struct RootState {
    included: bool,
    empty: bool,
    excluded: bool,
    unknown: bool,
    missing_configured: bool,
}

fn source_root_identity(source: &ProviderSource) -> RootIdentity {
    match &source.route_provenance {
        ProviderSourceRouteProvenance::ConfiguredRoot { root_id, .. } => {
            RootIdentity::Configured(root_id.clone())
        }
        ProviderSourceRouteProvenance::Automatic { route_role } => RootIdentity::Automatic {
            provider: source.provider.as_str().to_owned(),
            role: route_role.as_bytes().to_vec(),
        },
        ProviderSourceRouteProvenance::Unroled => RootIdentity::Location {
            provider: source.provider.as_str().to_owned(),
            path: source.path.clone(),
        },
    }
}

fn source_root_lookup(sources: &[ProviderSource]) -> SourceRootLookup {
    sources
        .iter()
        .map(|source| {
            (
                (source.provider.as_str().to_owned(), source.path.clone()),
                source_root_identity(source),
            )
        })
        .collect()
}

fn record_source_coverage(state: &mut RootState, source: &ProviderSource) {
    match source.status {
        ProviderSourceStatus::Available => state.included = true,
        ProviderSourceStatus::Empty if source.exists => state.empty = true,
        ProviderSourceStatus::Unknown if source.exists => state.unknown = true,
        ProviderSourceStatus::Unsupported if source.exists => state.excluded = true,
        ProviderSourceStatus::Missing if source.route_provenance.configured_root().is_some() => {
            state.missing_configured = true;
        }
        ProviderSourceStatus::Empty
        | ProviderSourceStatus::Unknown
        | ProviderSourceStatus::Missing
        | ProviderSourceStatus::Unsupported => {}
    }
}

fn issue_root_identity(
    issue: &DiscoveryIssue,
    roots_by_provider_path: &SourceRootLookup,
    configured_roots: &[ctx_history_capture::ProviderRootDefinition],
) -> RootIdentity {
    if let Some(root) = issue.path.as_ref().and_then(|path| {
        roots_by_provider_path.get(&(issue.provider.as_str().to_owned(), path.clone()))
    }) {
        return root.clone();
    }
    if let Some(root) = configured_roots.iter().find(|root| {
        root.provider == issue.provider
            && issue.path.as_ref().is_some_and(|path| path == &root.path)
    }) {
        return RootIdentity::Configured(root.id.clone());
    }
    issue.path.as_ref().map_or_else(
        || RootIdentity::Unresolved(issue.provider.as_str().to_owned()),
        |path| RootIdentity::Location {
            provider: issue.provider.as_str().to_owned(),
            path: path.clone(),
        },
    )
}

fn root_coverage(roots: &HashMap<RootIdentity, RootState>) -> Result<HistoryRootCoverage> {
    roots
        .values()
        .try_fold(HistoryRootCoverage::default(), |mut coverage, root| {
            let root_missing =
                root.missing_configured && !root.included && !root.empty && !root.excluded;
            let counter = if root.included && (root.excluded || root.unknown) {
                &mut coverage.partial
            } else if root.included {
                &mut coverage.included
            } else if root.unknown || root_missing {
                &mut coverage.unknown
            } else if root.excluded {
                &mut coverage.excluded
            } else {
                return Ok(coverage);
            };
            *counter = checked_increment(*counter)?;
            Ok::<_, anyhow::Error>(coverage)
        })
}

fn checked_increment(value: u64) -> Result<u64> {
    value
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("provider root count overflow"))
}

#[cfg(test)]
#[path = "history_health_tests.rs"]
mod tests;

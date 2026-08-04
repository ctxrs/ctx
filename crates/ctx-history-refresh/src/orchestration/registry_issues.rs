use super::*;

/// Rejects only registry issues whose unsafe root makes route-local execution
/// incapable of establishing a safe publication boundary.
pub(in super::super) fn reject_blocking_automatic_registry_issues(
    issues: &[SourceBackedAutomaticRegistryIssue],
) -> Result<()> {
    let mut blocker_count = 0usize;
    let mut blocker_details = Vec::new();
    for issue in issues {
        let SourceBackedAutomaticRegistryIssue::Unavailable { source, reason } = issue else {
            continue;
        };
        if !matches!(
            reason,
            SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { .. }
        ) {
            continue;
        }
        blocker_count = blocker_count.saturating_add(1);
        if blocker_details.len() < SOURCE_REFRESH_BUILD_ISSUE_LIMIT {
            blocker_details.push(format!(
                "{} {}: {}",
                source.provider.as_str(),
                source.path.display(),
                automatic_registry_issue_reason(reason),
            ));
        }
    }
    if blocker_count == 0 {
        return Ok(());
    }
    let omitted = blocker_count.saturating_sub(blocker_details.len());
    let omitted = if omitted == 0 {
        String::new()
    } else {
        format!("; {omitted} additional systemic safety issue(s) omitted")
    };
    Err(anyhow!(
        "{TERMINAL_COVERAGE_ERROR_CODE}: capture automatic registry has {blocker_count} systemic safety issue(s): {}{omitted}",
        blocker_details.join("; ")
    ))
}

pub(in super::super) fn automatic_registry_route_failures(
    issues: &[SourceBackedAutomaticRegistryIssue],
    retained_generation: Option<&VerifiedIndex>,
) -> Result<Vec<ctx_history_capture::SourceBackedFailedRoute>> {
    let mut failures = BTreeMap::new();
    for issue in issues {
        let SourceBackedAutomaticRegistryIssue::Unavailable { source, reason } = issue else {
            continue;
        };
        let Some(class) = automatic_registry_issue_failure_class(source, reason) else {
            continue;
        };
        let route_identity = automatic_registry_issue_route_identity(source)?;
        let carried_forward = retained_generation.is_some_and(|index| {
            index
                .manifest()
                .source_route(&route_identity)
                .is_some_and(|route| !route.sources().is_empty())
        });
        let source_identity = automatic_registry_issue_source_identity(source)?;
        failures.entry(route_identity.clone()).or_insert_with(|| {
            ctx_history_capture::SourceBackedFailedRoute::new(
                route_identity,
                source_identity,
                source.provider,
                class,
                carried_forward,
                source.path.display().to_string(),
                automatic_registry_issue_reason(reason),
            )
        });
    }
    Ok(failures.into_values().collect())
}

fn automatic_registry_issue_failure_class(
    source: &ctx_history_capture::ProviderSource,
    reason: &SourceBackedAutomaticUnavailableReason,
) -> Option<SourceBackedSourceFailureClass> {
    match reason {
        SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { .. }
        | SourceBackedAutomaticUnavailableReason::SourceStatus(
            ProviderSourceStatus::Missing | ProviderSourceStatus::Unknown,
        ) => None,
        SourceBackedAutomaticUnavailableReason::SourceStatus(_) if source.exists => {
            Some(SourceBackedSourceFailureClass::Unavailable)
        }
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { .. }
        | SourceBackedAutomaticUnavailableReason::RegistrationRejected { .. }
            if source.exists =>
        {
            Some(SourceBackedSourceFailureClass::Incompatible)
        }
        SourceBackedAutomaticUnavailableReason::SourceStatus(_)
        | SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { .. }
        | SourceBackedAutomaticUnavailableReason::RegistrationRejected { .. } => None,
    }
}

fn automatic_registry_issue_route_identity(
    source: &ctx_history_capture::ProviderSource,
) -> Result<SourceRouteIdentity> {
    automatic_source_backed_route_identity(source).map_err(Into::into)
}

fn automatic_registry_issue_metadata(
    source: &ctx_history_capture::ProviderSource,
) -> Result<&'static ctx_history_capture::SourceBackedProviderRouteMetadata> {
    source_backed_route_inventory()
        .iter()
        .find(|route| {
            route.provider == source.provider && route.source_format == source.source_format
        })
        .filter(|route| route.automatic)
        .ok_or_else(|| {
            anyhow!(
                "automatic registry issue for {}/{} has no prior executable route contract",
                source.provider.as_str(),
                source.source_format,
            )
        })
}

fn automatic_registry_issue_source_identity(
    source: &ctx_history_capture::ProviderSource,
) -> Result<String> {
    let metadata = automatic_registry_issue_metadata(source)?;
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-failure-identity-v1\0");
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(metadata.certified_source_format.as_bytes());
    digest.update([0]);
    let path = source.path.as_os_str().as_encoded_bytes();
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) fn selected_registry_route_count(
    registry: &SourceBackedProviderRegistry,
    scope: &SourceBackedRefreshScope,
) -> usize {
    registry
        .routes()
        .filter(|route| route.selection.is_some())
        .filter(|route| match scope {
            SourceBackedRefreshScope::All => true,
            SourceBackedRefreshScope::Exact(selected) => route
                .route_identity
                .as_ref()
                .is_some_and(|identity| selected.contains(identity)),
        })
        .count()
}

fn automatic_registry_issue_reason(reason: &SourceBackedAutomaticUnavailableReason) -> String {
    match reason {
        SourceBackedAutomaticUnavailableReason::SourceStatus(status) => {
            format!("source status is {}", status.as_str())
        }
        SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { detail }
        | SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail } => detail.clone(),
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
    }
}

use super::*;

#[derive(Debug, Clone, Default)]
pub(super) struct RouteLessRegistryBlockers {
    pub(super) total: usize,
    details: Vec<String>,
}

impl RouteLessRegistryBlockers {
    pub(super) fn publication_error(&self) -> ZeroSourcePublicationBlocked {
        let omitted = self.total.saturating_sub(self.details.len());
        let omitted = if omitted == 0 {
            String::new()
        } else {
            format!("; {omitted} additional route-less blocker(s) omitted")
        };
        ZeroSourcePublicationBlocked::new(format!(
            "zero-source publication has {} unsupported or unavailable catalog blocker(s): {}{}",
            self.total,
            self.details.join("; "),
            omitted,
        ))
    }
}

/// Rejects only registry issues whose unsafe root makes route-local execution
/// incapable of establishing a safe publication boundary.
#[doc(hidden)]
pub fn reject_blocking_automatic_registry_issues(
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

#[doc(hidden)]
pub fn automatic_registry_route_failures(
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
        let Ok(route_identity) = automatic_registry_issue_route_identity(source) else {
            // Unknown registrations have no executable route identity. They
            // remain route-less blockers and must never be assigned a
            // fabricated identity merely to fit the route-result vector.
            continue;
        };
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

pub(super) fn terminal_registry_route_failures(
    failures: Vec<ctx_history_capture::SourceBackedFailedRoute>,
    registry: &SourceBackedProviderRegistry,
    scope: &SourceBackedRefreshScope,
) -> Vec<ctx_history_capture::SourceBackedFailedRoute> {
    // Discovered-winner identities deliberately coalesce alternate physical
    // candidates. If any selected candidate registered executable or
    // certified-missing capture authority, capture owns the route's one
    // terminal outcome. A registry issue for a losing candidate must not
    // fabricate a second, overlapping route-level failure.
    let capture_owned_routes = registry
        .routes()
        .filter(|route| route.selection.is_some())
        .filter_map(|route| route.route_identity.as_ref())
        .filter(|route| match scope {
            SourceBackedRefreshScope::All => true,
            SourceBackedRefreshScope::Exact(selected) => selected.contains(*route),
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    failures
        .into_iter()
        .filter(|failure| !capture_owned_routes.contains(&failure.route_identity))
        .collect()
}

#[derive(Clone, Copy)]
pub(super) enum AutomaticRegistryAdmissionFailurePolicy<'a> {
    SystemicOnly,
    ScopedSelection,
    ExactRoutes(&'a BTreeSet<SourceRouteIdentity>),
}

pub(super) fn automatic_registry_admission_failures(
    issues: &[SourceBackedAutomaticRegistryIssue],
    policy: AutomaticRegistryAdmissionFailurePolicy<'_>,
) -> Result<Option<SourceBackedAdmissionRouteFailures>> {
    let failures = issues
        .iter()
        .filter_map(|issue| {
            let SourceBackedAutomaticRegistryIssue::Unavailable { source, reason } = issue else {
                return None;
            };
            let SourceBackedAutomaticUnavailableReason::RegistrationRejected { kind, detail } =
                reason
            else {
                return None;
            };
            if !source.exists {
                return None;
            }
            if matches!(
                policy,
                AutomaticRegistryAdmissionFailurePolicy::SystemicOnly
            ) && !matches!(
                kind,
                SourceBackedRouteErrorKind::ResourceUnavailable
                    | SourceBackedRouteErrorKind::Internal
            ) {
                return None;
            }
            Some((source, *kind, detail))
        })
        .filter_map(|(source, kind, detail)| {
            let route_identity = match automatic_registry_issue_route_identity(source) {
                Ok(route_identity) => route_identity,
                Err(error) => return Some(Err(error)),
            };
            if matches!(
                policy,
                AutomaticRegistryAdmissionFailurePolicy::ExactRoutes(selected)
                    if !selected.contains(&route_identity)
            ) {
                return None;
            }
            Some(Ok(SourceBackedAdmissionRouteFailure::new(
                route_identity,
                kind,
                detail.clone(),
            )))
        })
        .collect::<Result<Vec<_>>>()?;
    if failures.is_empty() {
        return Ok(None);
    }
    SourceBackedAdmissionRouteFailures::try_from_failures(failures).map(Some)
}

pub(super) fn automatic_registry_route_less_blockers(
    issues: &[SourceBackedAutomaticRegistryIssue],
    route_failures: &[ctx_history_capture::SourceBackedFailedRoute],
) -> RouteLessRegistryBlockers {
    let represented_routes = route_failures
        .iter()
        .map(|failure| failure.route_identity.clone())
        .collect::<BTreeSet<_>>();
    let mut blockers = RouteLessRegistryBlockers::default();
    for issue in issues {
        let detail = match issue {
            SourceBackedAutomaticRegistryIssue::Discovery(issue)
                if matches!(
                    issue.kind,
                    DiscoveryIssueKind::NoDiskHistory
                        | DiscoveryIssueKind::InsufficientOfficialEvidence
                ) =>
            {
                continue;
            }
            SourceBackedAutomaticRegistryIssue::Discovery(issue) => {
                format!("{} {}", issue.provider.as_str(), issue.reason,)
            }
            SourceBackedAutomaticRegistryIssue::Unavailable { source, reason } => {
                if !source.exists
                    && matches!(
                        reason,
                        SourceBackedAutomaticUnavailableReason::SourceStatus(
                            ProviderSourceStatus::Missing | ProviderSourceStatus::Unsupported
                        ) | SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. }
                    )
                {
                    continue;
                }
                if automatic_registry_issue_route_identity(source)
                    .ok()
                    .is_some_and(|route| represented_routes.contains(&route))
                {
                    continue;
                }
                format!(
                    "{} {}: {}",
                    source.provider.as_str(),
                    source.path.display(),
                    automatic_registry_issue_reason(reason),
                )
            }
        };
        blockers.total = blockers.total.saturating_add(1);
        if blockers.details.len() < SOURCE_REFRESH_BUILD_ISSUE_LIMIT {
            blockers.details.push(detail);
        }
    }
    blockers
}

fn automatic_registry_issue_failure_class(
    source: &ProviderSource,
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
        SourceBackedAutomaticUnavailableReason::RegistrationRejected { kind, .. }
            if source.exists =>
        {
            kind.source_failure_class()
        }
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { .. }
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

fn automatic_registry_issue_route_identity(source: &ProviderSource) -> Result<SourceRouteIdentity> {
    automatic_source_backed_route_identity(source).map_err(Into::into)
}

fn automatic_registry_issue_source_identity(source: &ProviderSource) -> Result<String> {
    ctx_history_capture::source_backed_source_failure_identity(source).map_err(Into::into)
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
        | SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail, .. } => {
            detail.clone()
        }
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
    }
}

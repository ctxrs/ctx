use super::*;

#[derive(Debug, Clone)]
pub(super) struct DeferredConfiguredRootRetirement {
    pub(super) owner: SourceRouteIdentity,
    pub(super) predecessor: SourceRouteIdentity,
    pub(super) cohort: BTreeSet<SourceRouteIdentity>,
    pub(super) cohort_complete: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct PendingConfiguredRootReplacementCohort {
    pub(super) routes: BTreeSet<SourceRouteIdentity>,
    pub(super) has_unidentified_member: bool,
    pub(super) has_scanning_member: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ScheduledReplacementRoute {
    pub(super) route_index: usize,
    pub(super) cohort_id: Option<String>,
    pub(super) cohort_last: bool,
}

pub(super) fn replacement_route_schedule(
    registry: &SourceBackedProviderRegistry,
    selected: &BTreeSet<SourceRouteIdentity>,
    cohorts: &BTreeMap<String, PendingConfiguredRootReplacementCohort>,
) -> Vec<ScheduledReplacementRoute> {
    let cohort_by_route = cohorts
        .iter()
        .flat_map(|(root_id, cohort)| {
            cohort
                .routes
                .iter()
                .cloned()
                .map(|route| (route, root_id.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let executable = registry
        .routes
        .iter()
        .enumerate()
        .filter_map(|(route_index, route)| {
            let route_identity = route.metadata.route_identity.as_ref()?;
            (selected.contains(route_identity)
                && (route.driver.is_some() || !route.certified_missing_paths.is_empty()))
            .then(|| (route_index, route_identity.clone()))
        })
        .collect::<Vec<_>>();
    let mut emitted_cohorts = BTreeSet::new();
    let mut schedule = Vec::with_capacity(executable.len());
    for (route_index, route_identity) in &executable {
        let Some(cohort_id) = cohort_by_route.get(route_identity) else {
            schedule.push(ScheduledReplacementRoute {
                route_index: *route_index,
                cohort_id: None,
                cohort_last: false,
            });
            continue;
        };
        if !emitted_cohorts.insert(cohort_id.clone()) {
            continue;
        }
        let mut cohort_indices = executable
            .iter()
            .filter(|(_, candidate)| cohort_by_route.get(candidate) == Some(cohort_id))
            .map(|(candidate_index, _)| *candidate_index)
            .collect::<Vec<_>>();
        // Certified-missing members must verify before the final scanning
        // member. Only a scanning route can own predecessor retirement, and
        // registry construction deliberately assigns that retirement to the
        // last scanning member in registry order.
        cohort_indices.sort_by_key(|candidate_index| {
            (
                registry.routes[*candidate_index].driver.is_some(),
                *candidate_index,
            )
        });
        let last = cohort_indices.last().copied();
        schedule.extend(cohort_indices.into_iter().map(|candidate_index| {
            ScheduledReplacementRoute {
                route_index: candidate_index,
                cohort_id: Some(cohort_id.clone()),
                cohort_last: Some(candidate_index) == last,
            }
        }));
    }
    schedule
}

pub(super) fn applied_provider_root_config_digest(
    automatic_provider_discovery: bool,
    roots: &[AppliedProviderRoot],
) -> String {
    let definitions = roots
        .iter()
        .map(|root| root.definition().clone())
        .collect::<Vec<_>>();
    provider_source_config_digest(automatic_provider_discovery, &definitions)
}

pub(super) fn pending_configured_root_replacement_cohorts(
    registry: &SourceBackedProviderRegistry,
    base_roots: &[AppliedProviderRoot],
) -> BTreeMap<String, PendingConfiguredRootReplacementCohort> {
    let Some((_, _, current_roots)) = registry.applied_provider_roots.as_ref() else {
        return BTreeMap::new();
    };
    current_roots
        .iter()
        .filter(|current| {
            base_roots.iter().any(|predecessor| {
                predecessor.definition().id == current.definition().id
                    && (predecessor.definition().provider != current.definition().provider
                        || predecessor.definition().kind != current.definition().kind)
            })
        })
        .map(|current| {
            let matching_routes = registry
                .routes
                .iter()
                .filter(|route| {
                    route.metadata.source.provider == current.definition().provider
                        && route
                            .metadata
                            .source
                            .route_provenance
                            .configured_root()
                            .is_some_and(|(root_id, _)| root_id == current.definition().id)
                })
                .collect::<Vec<_>>();
            let cohort = PendingConfiguredRootReplacementCohort {
                routes: matching_routes
                    .iter()
                    .filter_map(|route| route.metadata.route_identity.clone())
                    .collect(),
                has_unidentified_member: matching_routes
                    .iter()
                    .any(|route| route.metadata.route_identity.is_none()),
                has_scanning_member: matching_routes.iter().any(|route| route.driver.is_some()),
            };
            (current.definition().id.clone(), cohort)
        })
        .collect()
}

pub(super) fn roots_with_pending_configured_root_replacements(
    current_roots: Vec<AppliedProviderRoot>,
    base_roots: &[AppliedProviderRoot],
    replacement_cohorts: &BTreeMap<String, PendingConfiguredRootReplacementCohort>,
    terminal_routes: Option<(
        &BTreeSet<SourceRouteIdentity>,
        &BTreeSet<SourceRouteIdentity>,
    )>,
) -> ctx_history_index::Result<Vec<AppliedProviderRoot>> {
    current_roots
        .into_iter()
        .map(|current| {
            let Some(predecessor) = base_roots.iter().find(|predecessor| {
                predecessor.definition().id == current.definition().id
                    && (predecessor.definition().provider != current.definition().provider
                        || predecessor.definition().kind != current.definition().kind)
            }) else {
                return Ok(current);
            };
            let cohort = replacement_cohorts.get(&current.definition().id);
            let replacement_succeeded =
                terminal_routes.is_some_and(|(successful, partial)| {
                    cohort.map_or_else(
                        || {
                            !current.routes().is_empty()
                                && current.routes().iter().all(|route| {
                                    successful.contains(route) && !partial.contains(route)
                                })
                        },
                        |cohort| {
                            cohort.has_scanning_member
                                && !cohort.has_unidentified_member
                                && !cohort.routes.is_empty()
                                && cohort.routes.iter().all(|route| {
                                    successful.contains(route) && !partial.contains(route)
                                })
                        },
                    )
                });
            if replacement_succeeded {
                return Ok(current);
            }
            // Keep the complete predecessor authority until the replacement
            // cohort succeeds. Publishing the successor definition early
            // would erase the provider/kind transition marker and make a
            // later retry unable to retire the predecessor atomically.
            Ok(predecessor.clone())
        })
        .collect()
}

pub(super) fn deferred_configured_root_retirements(
    registry: &SourceBackedProviderRegistry,
    base_roots: &[AppliedProviderRoot],
    replacement_cohorts: &BTreeMap<String, PendingConfiguredRootReplacementCohort>,
) -> Vec<DeferredConfiguredRootRetirement> {
    let Some((_, _, current_roots)) = registry.applied_provider_roots.as_ref() else {
        return Vec::new();
    };
    let mut retirements = Vec::new();
    for current in current_roots {
        let Some(predecessor) = base_roots
            .iter()
            .find(|root| root.definition().id == current.definition().id)
        else {
            continue;
        };
        if predecessor.definition().provider == current.definition().provider
            && predecessor.definition().kind == current.definition().kind
        {
            continue;
        }
        let cohort = replacement_cohorts
            .get(&current.definition().id)
            .cloned()
            .unwrap_or_else(|| PendingConfiguredRootReplacementCohort {
                routes: current.routes().iter().cloned().collect(),
                has_unidentified_member: false,
                has_scanning_member: true,
            });
        if cohort.routes.len() + usize::from(cohort.has_unidentified_member) < 2 {
            continue;
        }
        for route in &registry.routes {
            let Some(owner) = route.metadata.route_identity.as_ref() else {
                continue;
            };
            if !cohort.routes.contains(owner) {
                continue;
            }
            retirements.extend(
                route
                    .retire_after_success
                    .iter()
                    .filter(|retired| {
                        predecessor.routes().contains(*retired)
                            && predecessor.exact_source_tokens_for_route(retired).is_none()
                    })
                    .cloned()
                    .map(|predecessor| DeferredConfiguredRootRetirement {
                        owner: owner.clone(),
                        predecessor,
                        cohort: cohort.routes.clone(),
                        cohort_complete: !cohort.has_unidentified_member,
                    }),
            );
        }
    }
    retirements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_root_replacement_keeps_exact_predecessor_membership_until_terminal_success() {
        let definition = |provider| ProviderRootDefinition {
            id: "work".to_owned(),
            provider,
            path: PathBuf::from(format!("/fixture/{}", provider.as_str())),
            group: Some("work".to_owned()),
            kind: None,
        };
        let predecessor_route = SourceRouteIdentity::from_sha256("a1".repeat(32)).unwrap();
        let successor_route = SourceRouteIdentity::from_sha256("b2".repeat(32)).unwrap();
        let predecessor = AppliedProviderRoot::with_source_identity(
            definition(CaptureProvider::Claude),
            ProviderRootSourceIdentity::Released,
            vec![predecessor_route.clone()],
        )
        .unwrap()
        .with_exact_source_memberships(vec![AppliedProviderRootSourceMembership::exact(
            predecessor_route.clone(),
            vec!["c3".repeat(32)],
        )
        .unwrap()])
        .unwrap();
        let successor = AppliedProviderRoot::with_source_identity(
            definition(CaptureProvider::Codex),
            ProviderRootSourceIdentity::NamedV1,
            vec![successor_route.clone()],
        )
        .unwrap();

        let pending = roots_with_pending_configured_root_replacements(
            vec![successor.clone()],
            std::slice::from_ref(&predecessor),
            &BTreeMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(pending[0].definition(), predecessor.definition());
        assert_eq!(
            pending[0].source_identity(),
            ProviderRootSourceIdentity::Released
        );
        assert_eq!(
            pending[0].routes(),
            std::slice::from_ref(&predecessor_route)
        );
        assert_eq!(
            pending[0].exact_source_memberships(),
            predecessor.exact_source_memberships()
        );

        let failed = roots_with_pending_configured_root_replacements(
            vec![successor.clone()],
            std::slice::from_ref(&predecessor),
            &BTreeMap::new(),
            Some((&BTreeSet::new(), &BTreeSet::new())),
        )
        .unwrap();
        assert_eq!(failed, pending);

        let successful = BTreeSet::from([successor_route]);
        let finalized = roots_with_pending_configured_root_replacements(
            vec![successor],
            std::slice::from_ref(&predecessor),
            &BTreeMap::new(),
            Some((&successful, &BTreeSet::new())),
        )
        .unwrap();
        assert_eq!(
            finalized[0].routes(),
            &[SourceRouteIdentity::from_sha256("b2".repeat(32)).unwrap()]
        );
        assert!(finalized[0].exact_source_memberships().is_empty());
    }

    #[test]
    fn pending_root_replacement_waits_for_complete_nonpartial_cohort() {
        let definition = |provider| ProviderRootDefinition {
            id: "work".to_owned(),
            provider,
            path: PathBuf::from(format!("/fixture/{}", provider.as_str())),
            group: None,
            kind: None,
        };
        let predecessor_route = SourceRouteIdentity::from_sha256("d1".repeat(32)).unwrap();
        let first = SourceRouteIdentity::from_sha256("e2".repeat(32)).unwrap();
        let second = SourceRouteIdentity::from_sha256("f3".repeat(32)).unwrap();
        let predecessor = AppliedProviderRoot::with_source_identity(
            definition(CaptureProvider::Claude),
            ProviderRootSourceIdentity::NamedV1,
            vec![predecessor_route],
        )
        .unwrap();
        let successor = AppliedProviderRoot::with_source_identity(
            definition(CaptureProvider::Codex),
            ProviderRootSourceIdentity::NamedV1,
            vec![first.clone(), second.clone()],
        )
        .unwrap();
        let cohorts = BTreeMap::from([(
            "work".to_owned(),
            PendingConfiguredRootReplacementCohort {
                routes: BTreeSet::from([first.clone(), second.clone()]),
                has_unidentified_member: false,
                has_scanning_member: true,
            },
        )]);
        let successful = BTreeSet::from([first.clone(), second.clone()]);

        let partial = roots_with_pending_configured_root_replacements(
            vec![successor.clone()],
            std::slice::from_ref(&predecessor),
            &cohorts,
            Some((&successful, &BTreeSet::from([second]))),
        )
        .unwrap();
        assert_eq!(partial, vec![predecessor.clone()]);

        let complete = roots_with_pending_configured_root_replacements(
            vec![successor.clone()],
            std::slice::from_ref(&predecessor),
            &cohorts,
            Some((&successful, &BTreeSet::new())),
        )
        .unwrap();
        assert_eq!(complete, vec![successor.clone()]);

        let unidentified = BTreeMap::from([(
            "work".to_owned(),
            PendingConfiguredRootReplacementCohort {
                routes: BTreeSet::from([first]),
                has_unidentified_member: true,
                has_scanning_member: true,
            },
        )]);
        let blocked = roots_with_pending_configured_root_replacements(
            complete,
            std::slice::from_ref(&predecessor),
            &unidentified,
            Some((&successful, &BTreeSet::new())),
        )
        .unwrap();
        assert_eq!(blocked, vec![predecessor.clone()]);

        let missing_only = BTreeMap::from([(
            "work".to_owned(),
            PendingConfiguredRootReplacementCohort {
                routes: successful.clone(),
                has_unidentified_member: false,
                has_scanning_member: false,
            },
        )]);
        let blocked = roots_with_pending_configured_root_replacements(
            vec![successor],
            std::slice::from_ref(&predecessor),
            &missing_only,
            Some((&successful, &BTreeSet::new())),
        )
        .unwrap();
        assert_eq!(blocked, vec![predecessor]);
    }
}

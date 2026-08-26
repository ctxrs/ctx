use super::*;

impl GenerationWriter {
    pub(super) fn next_manifest(&self) -> Result<GenerationManifest> {
        self.validate_source_route_plan_complete()?;
        if let Some(base) = self.base_publication.as_ref() {
            if self.source_replacement_manifest_is_route_stable(base.manifest()) {
                #[cfg(any(test, feature = "test-support"))]
                SOURCE_REPLACEMENT_MANIFESTS
                    .with(|visits| visits.set(visits.get().saturating_add(1)));
                return base.successor_manifest_from_source_replacements(
                    staging::manifest_source_replacements(self)?,
                );
            }
        }
        let deleted_sources = self
            .deletions
            .keys()
            .chain(&self.route_deletions)
            .map(|source| source.identity().digest())
            .collect::<BTreeSet<_>>();
        let mut source_upserts = BTreeMap::<[u8; 32], CertifiedSource>::new();
        for pending in self.pending.values() {
            let certificate = pending.certificate.as_ref().ok_or_else(|| {
                IndexError::SourceNotCertified(pending.source.identity().to_string())
            })?;
            source_upserts.insert(pending.source.identity().digest(), certificate.clone());
        }
        let sources = merge_manifest_sources(
            self.base_manifest().map_or(&[][..], |base| &base.sources),
            source_upserts,
            &deleted_sources,
        );
        let record_aggregates = staging::manifest_record_aggregates(self, &sources)?;
        let mut source_routes = if let Some(routes) = &self.present_source_routes {
            routes.clone()
        } else {
            implicit_source_routes(&sources)?
        };
        for route in &mut source_routes {
            let Some(delta) = self.partial_source_route_deltas.get(route.route_identity()) else {
                continue;
            };
            if route.missing_state().is_some() {
                return Err(IndexError::InvalidSourceRoutePlan(format!(
                    "partial route {} cannot carry missing state",
                    route.route_identity().as_str()
                )));
            }
            *route = SourceRouteSnapshot::present(
                route.route_identity().clone(),
                merge_partial_route_members(route.sources(), delta),
            )?;
        }
        source_routes.extend(self.observed_missing_routes.values().cloned());
        let (automatic_provider_discovery, provider_root_config_digest, provider_roots) = self
            .applied_provider_roots
            .clone()
            .or_else(|| {
                self.base_manifest().map(|base| {
                    (
                        base.automatic_provider_discovery(),
                        base.provider_root_config_digest().to_owned(),
                        base.provider_roots().to_vec(),
                    )
                })
            })
            .unwrap_or_else(|| (true, provider_source_config_digest(true, &[]), Vec::new()));
        let detached_released_provider_roots =
            detached_released_provider_root_authorities(self.base_manifest(), &provider_roots)?;
        GenerationManifest::from_parts_with_record_aggregates_and_provider_roots_and_detached_authorities(
            sources,
            record_aggregates,
            source_routes,
            automatic_provider_discovery,
            provider_root_config_digest,
            provider_roots,
            detached_released_provider_roots,
        )
    }

    fn source_replacement_manifest_is_route_stable(&self, base: &GenerationManifest) -> bool {
        if self
            .applied_provider_roots
            .as_ref()
            .is_some_and(|(automatic, digest, roots)| {
                *automatic != base.automatic_provider_discovery()
                    || digest != base.provider_root_config_digest()
                    || roots != base.provider_roots()
            })
        {
            return false;
        }
        if self.pending.is_empty()
            || !self.deletions.is_empty()
            || !self.route_deletions.is_empty()
            || !self.observed_missing_routes.is_empty()
        {
            return false;
        }
        let Some(routes) = self.present_source_routes.as_deref() else {
            return false;
        };
        if routes.len() != base.source_routes().len()
            || routes
                .iter()
                .zip(base.source_routes())
                .any(|(current, base)| !current.exact_snapshot_eq(base))
        {
            return false;
        }
        for (route_identity, delta) in &self.partial_source_route_deltas {
            if !delta.deletions.is_empty() {
                return false;
            }
            let Some(base_route) = base.source_route(route_identity) else {
                return false;
            };
            for (digest, source) in &delta.upserts {
                let Some(base_source) = base_route
                    .sources()
                    .binary_search_by_key(digest, |source| source.identity().digest())
                    .ok()
                    .and_then(|index| base_route.sources().get(index))
                else {
                    return false;
                };
                if !base_source.exact_descriptor_eq(source) {
                    return false;
                }
            }
        }
        self.pending.values().all(|pending| {
            base.sources
                .binary_search_by_key(&pending.source.identity().digest(), |source| {
                    source.observation().source().identity().digest()
                })
                .ok()
                .and_then(|index| base.sources.get(index))
                .is_some_and(|source| {
                    source
                        .observation()
                        .source()
                        .exact_descriptor_eq(&pending.source)
                })
        })
    }
}

fn detached_released_provider_root_authorities(
    base: Option<&GenerationManifest>,
    current: &[AppliedProviderRoot],
) -> Result<Vec<DetachedReleasedProviderRootAuthority>> {
    let Some(base) = base else {
        return Ok(Vec::new());
    };
    let mut authorities = BTreeMap::new();
    for authority in base.detached_released_provider_roots() {
        if current
            .iter()
            .all(|root| root.definition().id != authority.id())
        {
            authorities.insert(authority.id().to_owned(), authority.clone());
        }
    }
    for root in base.provider_roots() {
        if current
            .iter()
            .any(|current| current.definition().id == root.definition().id)
        {
            continue;
        }
        if let Some(authority) = DetachedReleasedProviderRootAuthority::from_applied(root)? {
            if authorities.contains_key(authority.id()) {
                continue;
            }
            if authorities.len() == MAX_DETACHED_RELEASED_PROVIDER_ROOTS {
                return Err(IndexError::InvalidProviderRoots(
                    "detached released root authority capacity is exhausted".to_owned(),
                ));
            }
            authorities.insert(authority.id().to_owned(), authority);
        }
    }
    // Active roots consume a compatible authority. A same-name incompatible
    // replacement is destructive, so it drops the old detached authority too.
    Ok(authorities.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_core::CaptureProvider;

    fn released_root(id: impl Into<String>) -> AppliedProviderRoot {
        let id = id.into();
        AppliedProviderRoot::with_source_identity(
            ProviderRootDefinition {
                path: std::env::temp_dir().join(format!("ctx-detached-authority-{id}")),
                group: None,
                kind: None,
                provider: CaptureProvider::Codex,
                id,
            },
            ProviderRootSourceIdentity::Released,
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn detached_authority_capacity_rejects_manifest_planning_before_publication() {
        let detached = (0..MAX_DETACHED_RELEASED_PROVIDER_ROOTS)
            .map(|index| {
                DetachedReleasedProviderRootAuthority::from_applied(&released_root(format!(
                    "detached-{index}"
                )))
                .unwrap()
                .unwrap()
            })
            .collect::<Vec<_>>();
        let active = released_root("overflow");
        let base = GenerationManifest::from_parts_with_record_aggregates_and_provider_roots_and_detached_authorities(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            true,
            provider_source_config_digest(true, std::slice::from_ref(active.definition())),
            vec![active],
            detached,
        )
        .unwrap();

        assert!(matches!(
            detached_released_provider_root_authorities(Some(&base), &[]),
            Err(IndexError::InvalidProviderRoots(detail))
                if detail == "detached released root authority capacity is exhausted"
        ));
        // The failed planning step has only read the predecessor, so no
        // publication can replace it or discard the authority it protects.
        assert_eq!(
            base.detached_released_provider_roots().len(),
            MAX_DETACHED_RELEASED_PROVIDER_ROOTS
        );
        assert_eq!(base.provider_roots().len(), 1);
    }

    #[test]
    fn same_name_incompatible_replacement_discards_detached_authority() {
        let detached = DetachedReleasedProviderRootAuthority::from_applied(&released_root("work"))
            .unwrap()
            .unwrap();
        let base = GenerationManifest::from_parts_with_record_aggregates_and_provider_roots_and_detached_authorities(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            true,
            provider_source_config_digest(true, &[]),
            Vec::new(),
            vec![detached],
        )
        .unwrap();
        let replacement = AppliedProviderRoot::new(
            ProviderRootDefinition {
                id: "work".to_owned(),
                provider: CaptureProvider::Claude,
                path: std::env::temp_dir().join("ctx-detached-authority-replacement"),
                group: None,
                kind: None,
            },
            Vec::new(),
        )
        .unwrap();

        assert!(
            detached_released_provider_root_authorities(Some(&base), &[replacement])
                .unwrap()
                .is_empty()
        );
    }
}

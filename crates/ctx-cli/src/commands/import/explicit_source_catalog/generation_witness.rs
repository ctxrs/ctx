use super::*;
use std::collections::{BTreeMap, BTreeSet};

impl ExplicitSourceCatalogAuthority {
    pub(crate) fn carries_request(&self, requested: &Self) -> bool {
        if requested.entries.is_empty() {
            return self == requested;
        }
        self.revision >= requested.revision
            && requested
                .entries
                .iter()
                .all(|entry| self.entries.contains(entry))
    }

    pub(crate) fn reconcile_generation_witness(
        previous: Option<(&Self, &[ExplicitSourceCatalogRouteBinding])>,
        requested: Option<(&Self, &[ExplicitSourceCatalogRouteBinding])>,
        retained_routes: &BTreeSet<ctx_history_index::SourceRouteIdentity>,
        published_requested_routes: &BTreeSet<ctx_history_index::SourceRouteIdentity>,
    ) -> Result<(Option<Self>, Vec<ExplicitSourceCatalogRouteBinding>)> {
        let mut requested_entries = Vec::new();
        let mut requested_bindings = Vec::new();
        if let Some((catalog, catalog_bindings)) = requested {
            catalog.collect_generation_witness(
                catalog_bindings,
                published_requested_routes,
                &BTreeSet::new(),
                &mut requested_entries,
                &mut requested_bindings,
            )?;
        }
        let requested_authorities = Self::authority_keys(&requested_entries)?;
        let mut entries = Vec::new();
        let mut bindings = Vec::new();
        if let Some((catalog, catalog_bindings)) = previous {
            catalog.collect_generation_witness(
                catalog_bindings,
                retained_routes,
                &requested_authorities,
                &mut entries,
                &mut bindings,
            )?;
        }
        let previous_contributed = !entries.is_empty();
        let requested_contributed = !requested_entries.is_empty();
        entries.extend(requested_entries);
        bindings.extend(requested_bindings);
        if entries.is_empty() {
            return Ok((None, Vec::new()));
        }
        sort_and_validate_entries(&mut entries)?;
        bindings.sort_by(|left, right| left.catalog_lineage.cmp(&right.catalog_lineage));
        let revision = previous
            .filter(|_| previous_contributed)
            .map(|(catalog, _)| catalog.revision)
            .into_iter()
            .chain(
                requested
                    .filter(|_| requested_contributed)
                    .map(|(catalog, _)| catalog.revision),
            )
            .max()
            .unwrap_or_default();
        let authority = authority_for(revision, &entries)?;
        authority.validate_request_wire_budget()?;
        Ok((Some(authority), bindings))
    }

    fn authority_keys(entries: &[CatalogEntry]) -> Result<BTreeSet<(String, String)>> {
        entries
            .iter()
            .map(|entry| {
                Ok((
                    entry.provider()?.as_str().to_owned(),
                    entry.certified_source_format()?.to_owned(),
                ))
            })
            .collect()
    }

    fn collect_generation_witness(
        &self,
        bindings: &[ExplicitSourceCatalogRouteBinding],
        retained_routes: &BTreeSet<ctx_history_index::SourceRouteIdentity>,
        replaced_authorities: &BTreeSet<(String, String)>,
        output_entries: &mut Vec<CatalogEntry>,
        output_bindings: &mut Vec<ExplicitSourceCatalogRouteBinding>,
    ) -> Result<()> {
        let mut bindings_by_lineage = BTreeMap::new();
        for binding in bindings {
            decode_digest(&binding.catalog_lineage)
                .context("validate published explicit catalog binding lineage")?;
            let route =
                ctx_history_index::SourceRouteIdentity::from_sha256(binding.route_identity.clone())
                    .context("validate published explicit catalog binding route")?;
            if bindings_by_lineage
                .insert(binding.catalog_lineage.as_str(), (binding, route))
                .is_some()
            {
                bail!("published explicit catalog contains duplicate lineage bindings");
            }
        }
        if self
            .entries
            .iter()
            .any(|entry| !bindings_by_lineage.contains_key(entry.catalog_lineage.as_str()))
        {
            bail!("published explicit catalog has incomplete lineage bindings");
        }
        for entry in &self.entries {
            let authority = (
                entry.provider()?.as_str().to_owned(),
                entry.certified_source_format()?.to_owned(),
            );
            if replaced_authorities.contains(&authority) {
                continue;
            }
            let (binding, route) = bindings_by_lineage
                .get(entry.catalog_lineage.as_str())
                .expect("catalog binding completeness checked above");
            if !retained_routes.contains(route) {
                continue;
            }
            output_entries.push(entry.clone());
            output_bindings.push((*binding).clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_source(path: PathBuf) -> ProviderSource {
        custom_provider_source(path, true).unwrap()
    }

    fn binding(
        request: &ExplicitSourceCatalogUpsert,
        byte: u8,
    ) -> (
        ExplicitSourceCatalogRouteBinding,
        ctx_history_index::SourceRouteIdentity,
    ) {
        let route =
            ctx_history_index::SourceRouteIdentity::from_sha256(format!("{byte:02x}").repeat(32))
                .unwrap();
        (
            ExplicitSourceCatalogRouteBinding {
                catalog_lineage: request.catalog_lineage_hex(),
                route_identity: route.as_str().to_owned(),
            },
            route,
        )
    }

    #[test]
    fn retained_generation_routes_keep_their_exact_catalog_witness() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let path = temp.path().join("retained.jsonl");
        fs::write(&path, b"\n").unwrap();
        let request = upsert_explicit_source(&data_root, &custom_source(path)).unwrap();
        let (binding, route) = binding(&request, 1);

        let (catalog, bindings) = ExplicitSourceCatalogAuthority::reconcile_generation_witness(
            Some((&request.authority, std::slice::from_ref(&binding))),
            None,
            &BTreeSet::from([route]),
            &BTreeSet::new(),
        )
        .unwrap();

        assert_eq!(catalog, Some(request.authority));
        assert_eq!(bindings, vec![binding]);
    }

    #[test]
    fn removed_or_replaced_routes_cannot_reuse_a_stale_catalog_witness() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let old_path = temp.path().join("old.jsonl");
        let replacement_path = temp.path().join("replacement.jsonl");
        fs::write(&old_path, b"\n").unwrap();
        fs::write(&replacement_path, b"\n").unwrap();
        let old = upsert_explicit_source(&data_root, &custom_source(old_path.clone())).unwrap();
        let replacement =
            upsert_explicit_source(&data_root, &custom_source(replacement_path)).unwrap();
        let (old_binding, old_route) = binding(&old, 2);
        let (replacement_binding, replacement_route) = binding(&replacement, 3);

        let removed = ExplicitSourceCatalogAuthority::reconcile_generation_witness(
            Some((&old.authority, std::slice::from_ref(&old_binding))),
            None,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(removed, (None, Vec::new()));

        let (catalog, bindings) = ExplicitSourceCatalogAuthority::reconcile_generation_witness(
            Some((&old.authority, std::slice::from_ref(&old_binding))),
            Some((
                &replacement.authority,
                std::slice::from_ref(&replacement_binding),
            )),
            &BTreeSet::from([old_route, replacement_route.clone()]),
            &BTreeSet::from([replacement_route]),
        )
        .unwrap();
        let catalog = catalog.unwrap();
        assert!(catalog.carries_request(&replacement.authority));
        assert_eq!(bindings, vec![replacement_binding]);
        assert!(catalog
            .relocation_authority(&old_path, &bindings)
            .unwrap()
            .is_none());
    }

    #[test]
    fn failed_replacement_keeps_the_retained_prior_witness() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let old_path = temp.path().join("old.jsonl");
        let failed_path = temp.path().join("failed.jsonl");
        fs::write(&old_path, b"\n").unwrap();
        fs::write(&failed_path, b"").unwrap();
        let old = upsert_explicit_source(&data_root, &custom_source(old_path)).unwrap();
        let failed = upsert_explicit_source(&data_root, &custom_source(failed_path)).unwrap();
        let (old_binding, old_route) = binding(&old, 4);
        let (failed_binding, _) = binding(&failed, 5);

        let (catalog, bindings) = ExplicitSourceCatalogAuthority::reconcile_generation_witness(
            Some((&old.authority, std::slice::from_ref(&old_binding))),
            Some((&failed.authority, std::slice::from_ref(&failed_binding))),
            &BTreeSet::from([old_route]),
            &BTreeSet::new(),
        )
        .unwrap();

        assert_eq!(catalog, Some(old.authority));
        assert_eq!(bindings, vec![old_binding]);
    }
}

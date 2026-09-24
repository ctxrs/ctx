use super::*;

impl crate::store::Store {
    /// Convenience lookup and traversal in one snapshot. Endpoints use the same
    /// exact-first tiers as resolve_endpoint. A relation filter selects a unique
    /// incident relation by exact spelling, normalized spelling, prefix, then
    /// substring; ties are errors. neighbors_extended remains exact-only.
    pub fn neighbors_resolved(&self, text: &str, options: &SearchOptions) -> Result<SearchResult> {
        catalog_budgeted(&self.conn, || {
            let mut options = options.clone();
            if let Some(relation) = &options.graph.relation {
                ensure!(
                    relation.len() <= 1024,
                    "relation must be at most 1024 bytes"
                );
                if relation.trim().is_empty() {
                    options.graph.relation = None;
                }
            }
            validate_search(&options)?;
            let text = validate_text(text)?;
            let tx = self.conn.unchecked_transaction()?;
            let mut output = new_search(&tx, contexts("", &options))?;
            let seed = resolve_endpoint_in(&tx, text, &options)?;
            if let Some(relation) = &options.graph.relation {
                options.graph.relation =
                    Some(resolve_relation(&tx, &seed.id, relation, &options.graph)?);
            }
            explore(&tx, vec![seed], &options, &mut output)?;
            tx.commit()?;
            finish_search(output)
        })
    }

    /// Resolve a unique endpoint: exact ID/label/scope, exact file, then literal
    /// Unicode/accent-insensitive exact, scoped qualified-tail, prefix and
    /// substring tiers. Punctuation stays literal; ties and incomplete scans
    /// return errors.
    pub fn resolve_endpoint(&self, text: &str, options: &SearchOptions) -> Result<Node> {
        catalog_budgeted(&self.conn, || {
            validate_search(options)?;
            let tx = self.conn.unchecked_transaction()?;
            generation(&tx)?;
            storage_layout(&tx)?;
            let node = resolve_endpoint_in(&tx, validate_text(text)?, options)?;
            tx.commit()?;
            Ok(node)
        })
    }

    /// Include grounded root/member/file seeds, then reverse dependencies.
    /// SearchResult.seeds distinguishes starting evidence from affected nodes.
    pub fn impact_extended(&self, text: &str, options: &ImpactOptions) -> Result<SearchResult> {
        catalog_budgeted(&self.conn, || {
            validate_search(&options.search)?;
            let text = validate_text(text)?;
            ensure!(options.relations.len() <= 32, "at most 32 impact relations");
            ensure!(
                options
                    .relations
                    .iter()
                    .all(|s| !s.trim().is_empty() && s.len() <= 1024),
                "impact relations must be nonempty and at most 1024 bytes each"
            );
            let mut relations: BTreeSet<String> = if options.relations.is_empty() {
                DEFAULT_IMPACT_RELATIONS
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect()
            } else {
                options.relations.iter().cloned().collect()
            };
            if let Some(relation) = &options.search.graph.relation {
                ensure!(
                    options.relations.is_empty() || relations.contains(relation),
                    "single relation conflicts with impact relations"
                );
                relations = BTreeSet::from([relation.clone()]);
            }
            ensure!(
                !relations
                    .iter()
                    .any(|s| MEMBERSHIP_RELATIONS.contains(&s.as_str())),
                "containment relations only expand impact seeds, not dependencies"
            );
            let relations: Vec<_> = relations.into_iter().collect();
            let mut search = options.search.clone();
            search.graph.direction = Direction::Incoming;
            let tx = self.conn.unchecked_transaction()?;
            let mut output = new_search(&tx, contexts(text, &search))?;
            let (seeds, examined) = impact_seeds(&tx, text, &search, &mut output)?;
            explore_filtered(
                &tx,
                seeds,
                &search,
                &mut output,
                &relations,
                examined,
                search.graph.limit,
            )?;
            tx.commit()?;
            finish_search(output)
        })
    }
}

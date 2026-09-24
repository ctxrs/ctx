use super::*;

impl<'a> GraphIndex<'a> {
    pub(super) fn new(graph: &'a GraphSnapshot) -> Result<Self> {
        ensure!(
            graph.nodes.len() <= 100_000 && graph.edges.len() <= 1_000_000,
            "PR impact snapshot exceeds node/edge count limits"
        );
        crate::analysis::validate(graph)?;
        let preserved = crate::analysis::preserved_communities(graph);
        let mut memberships = BTreeMap::new();
        let mut communities = BTreeMap::new();
        let community_notice = if preserved.is_empty() {
            community_limit_notice(graph)?
        } else {
            None
        };
        if preserved.is_empty() && community_notice.is_none() {
            let analysis = crate::analysis::analyze(graph, &Default::default())?;
            for group in analysis.communities {
                let key = group.id.to_string();
                for node in group.nodes {
                    memberships.insert(node, key.clone());
                }
                communities.insert(
                    key,
                    CommunityIdentity {
                        source: "computed".into(),
                        project: vec![],
                        id: serde_json::json!(group.id),
                        names: vec![group.label],
                    },
                );
            }
        } else {
            // Do not mix newly computed IDs with partially recorded memberships.
            for group in preserved {
                let key = serde_json::json!([group.project, group.id]).to_string();
                for node in group.nodes {
                    memberships.insert(node, key.clone());
                }
                communities.insert(
                    key,
                    CommunityIdentity {
                        source: "stored".into(),
                        project: group.project,
                        id: group.id,
                        names: group.names,
                    },
                );
            }
        }
        let mut files: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for node in &graph.nodes {
            if !node.file.is_empty() {
                files
                    .entry((namespace(node), node.file.clone()))
                    .or_default()
                    .push(node);
            }
        }
        Ok(Self {
            graph,
            files,
            memberships,
            communities,
            community_notice,
        })
    }
    pub(super) fn impact(&self, paths: &[ChangedFile]) -> Impact {
        let mut selected: BTreeSet<(String, String)> = BTreeSet::new();
        let mut unmatched = Vec::new();
        let mut ambiguous = Vec::new();
        for changed in paths {
            let matches: Vec<_> = self
                .files
                .keys()
                .filter(|(_, file)| {
                    let relative = self
                        .graph
                        .root
                        .as_deref()
                        .and_then(|root| file.strip_prefix(root.trim_end_matches('/')))
                        .and_then(|tail| tail.strip_prefix('/'))
                        .unwrap_or(file);
                    boundary_match(relative, &changed.path)
                })
                .collect();
            // Exact path wins over a weaker suffix, but duplicate composed sources
            // remain ambiguous: choosing a project by basename would erase provenance.
            let exact: Vec<_> = matches
                .iter()
                .copied()
                .filter(|(_, file)| {
                    file == &changed.path
                        || self.graph.root.as_deref().is_some_and(|root| {
                            file == &format!(
                                "{}/{path}",
                                root.trim_end_matches('/'),
                                path = changed.path
                            )
                        })
                })
                .collect();
            let candidates = if exact.is_empty() { matches } else { exact };
            match candidates.as_slice() {
                [] => unmatched.push(changed.path.clone()),
                [key] => {
                    selected.insert((**key).clone());
                }
                _ => ambiguous.push(changed.path.clone()),
            }
        }
        let mut nodes: Vec<Node> = selected
            .iter()
            .flat_map(|key| self.files[key].iter().map(|node| (*node).clone()))
            .collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        let nodes_without_community = nodes
            .iter()
            .filter(|node| !self.memberships.contains_key(&node.id))
            .count();
        let communities = nodes
            .iter()
            .filter_map(|node| self.memberships.get(&node.id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|key| self.communities[key].clone())
            .collect();
        Impact {
            generation: self.graph.generation,
            graph_kind: self.graph.kind.clone(),
            graph_root: self.graph.root.clone(),
            graph_metadata: self.graph.metadata.clone(),
            nodes,
            communities,
            nodes_without_community,
            unmatched_files: unmatched,
            ambiguous_files: ambiguous,
        }
    }
}

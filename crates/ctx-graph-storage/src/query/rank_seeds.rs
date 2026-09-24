use super::*;

pub(super) fn rank_seeds(
    conn: &Connection,
    text: &str,
    options: &SearchOptions,
) -> Result<Vec<Node>> {
    let exact = exact_filtered(conn, text, options, true)?;
    if !exact.is_empty() || text.contains("::") {
        return Ok(exact);
    }
    let terms = search_terms(conn, text)?;
    let normalized_terms: Vec<_> = terms.iter().map(|term| normalize(term)).collect();
    let mut candidates: BTreeMap<String, (Node, Vec<f64>)> = BTreeMap::new();
    let start = Instant::now();
    let mut examined = 0;
    let mut bytes = 0;
    for (index, literal) in terms.iter().enumerate() {
        let expression = format!("\"{literal}\"*");
        let term = &normalized_terms[index];
        let mut values: Vec<rusqlite::types::Value> = vec![expression.into()];
        let filters = filter_sql(conn, options, &mut values)?;
        // Enumerate postings, not the snapshot, and rank only after the complete
        // candidate set is known. Refuse an incomplete set: ranking a rowid
        // sample can silently exclude the best hit and depend on insert order.
        // FTS5 BM25 is unsuitable here: its IDF scan escapes the VM-step guard.
        let sql = format!(
            "SELECT n.payload FROM node_search CROSS JOIN nodes n ON n.rowid=node_search.rowid WHERE node_search MATCH ?{filters} ORDER BY node_search.rowid LIMIT {}",
            MAX_RANK_POSTINGS - examined + 1
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(values))?;
        while let Some(row) = rows.next()? {
            ensure!(
                examined < MAX_RANK_POSTINGS && start.elapsed() < Duration::from_secs(2),
                "search candidate enumeration exceeded its work/time budget; use a more specific query or a smaller file/kind scope"
            );
            examined += 1;
            let payload = row.get_ref(0)?.as_str()?;
            bytes += payload.len();
            ensure!(
                payload.len() <= MAX_SEARCH_BYTES && bytes <= MAX_RANK_BYTES,
                "search candidate enumeration exceeded its byte budget; use a more specific query or a smaller file/kind scope"
            );
            let node: Node = serde_json::from_str(payload)?;
            let label = normalize(&node.label);
            let label = label.trim_end_matches("()");
            let tier = if label == term || normalize(&node.id) == *term {
                12.0
            } else if label
                .split(|c: char| !c.is_alphanumeric())
                .any(|part| part == term)
            {
                8.0
            } else if label.starts_with(term) {
                6.0
            } else if label.contains(term) {
                3.0
            } else if node
                .qualified_name
                .as_deref()
                .is_some_and(|s| normalize(s).contains(term))
            {
                2.0
            } else if [
                "rationale",
                "description",
                "summary",
                "text",
                "excerpt",
                "evidence",
            ]
            .iter()
            .any(|field| {
                attributes(&node.metadata)
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| normalize(value).contains(term))
            }) {
                1.5
            } else {
                1.0
            };
            let id = node.id.clone();
            let entry = candidates
                .entry(id)
                .or_insert_with(|| (node, vec![0.0; terms.len()]));
            entry.1[index] = tier;
        }
    }
    let score = |node: &Node, weights: &[f64]| {
        let label = normalize(&node.label);
        let coverage = normalized_terms
            .iter()
            .filter(|t| label.contains(t.as_str()))
            .count() as f64
            / terms.len() as f64;
        weights.iter().sum::<f64>() * (1.0 + coverage * coverage)
    };
    let mut ranked: Vec<_> = candidates
        .into_values()
        .map(|(node, weights)| {
            let score = score(&node, &weights);
            (node, weights, score)
        })
        .collect();
    ranked.sort_by(|(a, _, a_score), (b, _, b_score)| {
        b_score
            .total_cmp(a_score)
            .then(a.label.len().cmp(&b.label.len()))
            .then(a.id.cmp(&b.id))
    });
    ensure!(
        start.elapsed() < Duration::from_secs(2),
        "search ranking exceeded its time budget; use a more specific query or a smaller file/kind scope"
    );
    let Some((_, _, top_score)) = ranked.first() else {
        return Ok(Vec::new());
    };
    let cutoff = top_score * 0.2;
    let mut chosen = BTreeSet::new();
    let mut labels = BTreeSet::new();
    let mut seeds = Vec::new();
    for (node, _, score) in &ranked {
        if seeds.len() == 3 || *score < cutoff {
            break;
        }
        if labels.insert(normalize(&node.label)) {
            chosen.insert(node.id.clone());
            seeds.push(node.clone());
        }
    }
    // Distinct terms get a candidate even if a common exact label dominates
    // combined scores. This is still <= 3 + 8 seeds, below MAX_SEEDS.
    for index in 0..terms.len() {
        if let Some((node, _, _)) =
            ranked
                .iter()
                .filter(|(_, w, _)| w[index] > 0.0)
                .max_by(|(a, aw, _), (b, bw, _)| {
                    aw[index]
                        .total_cmp(&bw[index])
                        .then_with(|| b.id.cmp(&a.id))
                })
            && !chosen.contains(&node.id)
            && labels.insert(normalize(&node.label))
        {
            chosen.insert(node.id.clone());
            seeds.push(node.clone());
        }
    }
    Ok(seeds)
}

pub(super) fn truncate(output: &mut SearchResult, reason: &str) {
    output.graph.truncated = true;
    if !output.truncation_reasons.iter().any(|r| r == reason) {
        output.truncation_reasons.push(reason.into());
    }
}

pub(super) fn new_search(conn: &Connection, contexts: Vec<String>) -> Result<SearchResult> {
    Ok(SearchResult {
        graph: empty(conn)?,
        seeds: Vec::new(),
        estimated_tokens: 0,
        contexts,
        truncation_reasons: Vec::new(),
    })
}

pub(super) fn finish_search(mut output: SearchResult) -> Result<SearchResult> {
    output.estimated_tokens = serde_json::to_vec(&output.graph)?.len().div_ceil(4);
    Ok(output)
}

pub(super) fn explore(
    conn: &Connection,
    seeds: Vec<Node>,
    options: &SearchOptions,
    output: &mut SearchResult,
) -> Result<()> {
    explore_filtered(conn, seeds, options, output, &[], 0, MAX_SEEDS)
}

pub(super) fn explore_filtered(
    conn: &Connection,
    seeds: Vec<Node>,
    options: &SearchOptions,
    output: &mut SearchResult,
    relations: &[String],
    mut examined: usize,
    seed_limit: usize,
) -> Result<()> {
    let mut budget = OutputBudget::new(&output.graph, options)?;
    let mut depths = BTreeMap::new();
    let mut edge_ids = BTreeSet::new();
    let mut steps = VecDeque::new();
    for (index, node) in seeds.into_iter().enumerate() {
        if index == seed_limit {
            truncate(output, "seed_limit");
            break;
        }
        if output.graph.nodes.len() == options.graph.limit {
            truncate(output, "node_limit");
            break;
        }
        let fits = budget.take(OutputBudget::cost(&node)?, output);
        ensure!(
            fits || !output.graph.nodes.is_empty(),
            "token budget cannot hold the primary seed; increase the budget"
        );
        if !fits {
            continue;
        }
        depths.insert(node.id.clone(), 0);
        output.seeds.push(node.id.clone());
        steps.push_back(Step::Expand(node.id.clone(), 0));
        output.graph.nodes.push(node);
    }
    if options.traversal == Traversal::Dfs {
        steps.make_contiguous().reverse();
    }
    let mut expanded = BTreeMap::<String, u32>::new();
    loop {
        let next = if options.traversal == Traversal::Bfs {
            steps.pop_front()
        } else {
            steps.pop_back()
        };
        let Some(step) = next else {
            break;
        };
        match step {
            Step::Expand(id, depth) => {
                if depth >= options.graph.depth
                    || expanded.get(&id).is_some_and(|old| *old <= depth)
                {
                    continue;
                }
                expanded.insert(id.clone(), depth);
                if examined == MAX_EXAMINED {
                    truncate(output, "work_limit");
                    break;
                }
                let mut edges = adjacency_filtered(
                    conn,
                    &id,
                    &options.graph,
                    MAX_EXAMINED - examined,
                    relations,
                )?;
                examined += edges.len();
                if examined == MAX_EXAMINED {
                    truncate(output, "work_limit");
                }
                if options.traversal == Traversal::Dfs {
                    edges.reverse();
                }
                for edge in edges {
                    if context_matches(&edge, &output.contexts) {
                        steps.push_back(Step::Follow(id.clone(), depth + 1, edge));
                    }
                }
            }
            Step::Follow(id, depth, edge) => {
                let next = opposite(&edge, &id).to_owned();
                let candidate = if !depths.contains_key(&next) {
                    let node = node(conn, &next)?
                        .ok_or_else(|| anyhow::anyhow!("edge points to missing node {next}"))?;
                    if !node_matches(&node, options) {
                        continue;
                    }
                    if output.graph.nodes.len() == options.graph.limit {
                        truncate(output, "node_limit");
                        continue;
                    }
                    Some(node)
                } else {
                    None
                };
                let edge_cost = if edge_ids.contains(&edge.id) {
                    0
                } else {
                    OutputBudget::cost(&edge)?
                };
                let node_cost = candidate
                    .as_ref()
                    .map(OutputBudget::cost)
                    .transpose()?
                    .unwrap_or(0);
                if !budget.take(edge_cost + node_cost, output) {
                    continue;
                }
                if let Some(node) = candidate {
                    output.graph.nodes.push(node);
                }
                if edge_ids.insert(edge.id.clone()) {
                    output.graph.edges.push(edge);
                }
                if depths.get(&next).is_none_or(|old| depth < *old) {
                    depths.insert(next.clone(), depth);
                    steps.push_back(Step::Expand(next, depth));
                }
            }
        }
    }
    if options.induced_edges {
        close_edges(conn, options, output, &mut budget, &mut examined, relations)?;
    }
    collect_unresolved(conn, options, output, &mut budget, &mut examined)?;
    Ok(())
}

pub(super) fn close_edges(
    conn: &Connection,
    options: &SearchOptions,
    output: &mut SearchResult,
    budget: &mut OutputBudget,
    examined: &mut usize,
    relations: &[String],
) -> Result<()> {
    let ids: BTreeSet<_> = output.graph.nodes.iter().map(|n| n.id.clone()).collect();
    let mut edge_ids: BTreeSet<_> = output.graph.edges.iter().map(|e| e.id.clone()).collect();
    let layout = storage_layout(conn)?;
    let selected_relations = relation_selection(options.graph.relation.as_deref(), relations);
    for id in &ids {
        if *examined == MAX_EXAMINED {
            truncate(output, "work_limit");
            break;
        }
        // Outgoing storage streams visit every induced edge only once,
        // including undirected edges, mutual arcs, parallel relations and loops.
        let identity = node_identity(conn, layout, id)?;
        let rows = edge_relation_rows(
            conn,
            layout,
            &identity,
            true,
            false,
            selected_relations.as_deref(),
            MAX_EXAMINED - *examined,
        )?;
        for edge in rows {
            *examined += 1;
            if ids.contains(&edge.target)
                && !edge_ids.contains(&edge.id)
                && context_matches(&edge, &output.contexts)
                && budget.take(OutputBudget::cost(&edge)?, output)
            {
                edge_ids.insert(edge.id.clone());
                output.graph.edges.push(edge);
            }
        }
        if *examined == MAX_EXAMINED {
            truncate(output, "work_limit");
        }
    }
    Ok(())
}

pub(super) fn collect_unresolved(
    conn: &Connection,
    options: &SearchOptions,
    output: &mut SearchResult,
    budget: &mut OutputBudget,
    examined: &mut usize,
) -> Result<()> {
    if options.graph.direction == Direction::Incoming {
        return Ok(());
    }
    let ids: Vec<_> = output.graph.nodes.iter().map(|n| n.id.clone()).collect();
    let layout = storage_layout(conn)?;
    for id in ids {
        if *examined == MAX_EXAMINED {
            truncate(output, "work_limit");
            break;
        }
        let limit = MAX_EXAMINED - *examined;
        let identity = node_identity(conn, layout, &id)?;
        let rows = unresolved_relation_rows(
            conn,
            layout,
            &identity,
            options.graph.relation.as_deref(),
            limit,
        )?;
        for (reference, reason) in rows {
            *examined += 1;
            if !output.contexts.is_empty()
                && !output
                    .contexts
                    .contains(&context_alias(&reference.relation))
            {
                continue;
            }
            if output.graph.unresolved.len() == options.graph.limit {
                truncate(output, "unresolved_limit");
                return Ok(());
            }
            let reference = UnresolvedReference {
                source: reference.source,
                label: reference.label,
                relation: reference.relation,
                file: reference.file,
                line: reference.line,
                reason,
            };
            if budget.take(OutputBudget::cost(&reference)?, output) {
                output.graph.unresolved.push(reference);
            }
        }
        if *examined == MAX_EXAMINED {
            truncate(output, "work_limit");
        }
    }
    Ok(())
}

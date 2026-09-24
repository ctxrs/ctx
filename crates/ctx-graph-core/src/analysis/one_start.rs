use super::*;

pub(super) fn one_start() -> u32 {
    1
}

pub(super) fn is_one_start(starts: &u32) -> bool {
    *starts == 1
}

pub(super) fn weight(edge: &Edge) -> Result<f64> {
    let weight = match attributes(&edge.metadata).get("weight") {
        Some(value) => value
            .as_f64()
            .context("edge metadata.weight must be a number")?,
        None => 1.0,
    };
    ensure!(
        weight.is_finite() && weight >= 0.0,
        "edge {} weight must be finite and nonnegative",
        edge.id
    );
    Ok(weight)
}

pub(super) fn pagerank(
    arcs: &[BTreeMap<usize, f64>],
    outgoing: &[f64],
    options: &AnalysisOptions,
) -> (Vec<f64>, usize, bool) {
    let n = arcs.len();
    if n == 0 {
        return (Vec::new(), 0, true);
    }
    let mut ranks = vec![1.0 / n as f64; n];
    for iteration in 1..=options.max_iterations {
        let dangling: f64 = ranks
            .iter()
            .zip(outgoing)
            .filter(|(_, w)| **w == 0.0)
            .map(|(r, _)| r)
            .sum();
        let base = (1.0 - options.damping + options.damping * dangling) / n as f64;
        let mut next = vec![base; n];
        for (i, row) in arcs.iter().enumerate() {
            if outgoing[i] > 0.0 {
                for (&j, &w) in row {
                    next[j] += options.damping * ranks[i] * (w / outgoing[i]);
                }
            }
        }
        let residual: f64 = next.iter().zip(&ranks).map(|(a, b)| (a - b).abs()).sum();
        ranks = next;
        if residual <= options.tolerance {
            return (ranks, iteration, true);
        }
    }
    (ranks, options.max_iterations, false)
}

pub(super) fn local_move(
    adjacency: &[BTreeMap<usize, f64>],
    strengths: &[f64],
    max_passes: usize,
    resolution: f64,
) -> (Vec<usize>, f64, usize, bool) {
    let n = adjacency.len();
    let total: f64 = strengths.iter().sum();
    let mut member: Vec<_> = (0..n).collect();
    if total == 0.0 {
        return (member, 0.0, 0, true);
    }
    // Normalize first to avoid overflow when weights are large.
    let degree: Vec<_> = strengths.iter().map(|v| v / total).collect();
    let mut totals = degree.clone();
    let mut sizes = vec![1usize; n];
    let mut empty = BTreeSet::new();
    let mut passes = 0;
    let mut converged = false;
    // Each sweep is O((V + E) log V); caller shares one pass budget across levels.
    for pass in 1..=max_passes {
        passes = pass;
        let mut moved = false;
        for i in 0..n {
            if degree[i] == 0.0 {
                continue;
            }
            let old = member[i];
            totals[old] -= degree[i];
            sizes[old] -= 1;
            if sizes[old] == 0 {
                empty.insert(old);
            }
            let mut neighbors = BTreeMap::<usize, f64>::new();
            for (&j, &w) in &adjacency[i] {
                if i != j {
                    *neighbors.entry(member[j]).or_default() += w / total;
                }
            }
            neighbors.entry(old).or_default();
            // A node may return to a singleton instead of being trapped in a bad group.
            if let Some(&id) = empty.first() {
                neighbors.entry(id).or_default();
            }
            let score = |c: usize, w: f64| w - resolution * (degree[i] * totals[c]);
            let mut best = old;
            let mut best_score = score(old, neighbors[&old]);
            for (&c, &w) in &neighbors {
                let candidate = score(c, w);
                if candidate > best_score + 1e-14 {
                    best = c;
                    best_score = candidate;
                }
            }
            totals[best] += degree[i];
            sizes[best] += 1;
            empty.remove(&best);
            member[i] = best;
            moved |= best != old;
        }
        if !moved {
            converged = true;
            break;
        }
    }
    let internal: f64 = adjacency
        .iter()
        .enumerate()
        .map(|(i, row)| {
            row.iter()
                .filter(|(j, _)| member[i] == member[**j])
                .map(|(_, w)| w / total)
                .sum::<f64>()
        })
        .sum();
    let modularity =
        internal - (resolution * totals.iter().map(|t| t * t).sum::<f64>()).min(f64::MAX);
    (member, modularity, passes, converged)
}

pub(super) fn connected_membership(
    adjacency: &[BTreeMap<usize, f64>],
    membership: &[usize],
) -> Vec<usize> {
    let mut refined = vec![usize::MAX; adjacency.len()];
    let mut group = 0;
    for seed in 0..adjacency.len() {
        if refined[seed] != usize::MAX {
            continue;
        }
        refined[seed] = group;
        let mut pending = vec![seed];
        while let Some(i) = pending.pop() {
            for (&j, &w) in &adjacency[i] {
                if w > 0.0 && membership[j] == membership[seed] && refined[j] == usize::MAX {
                    refined[j] = group;
                    pending.push(j);
                }
            }
        }
        group += 1;
    }
    refined
}

pub(super) fn partition(
    adjacency: &[BTreeMap<usize, f64>],
    strengths: &[f64],
    max_passes: usize,
    resolution: f64,
    options: &AnalysisOptions,
) -> Result<(Vec<usize>, f64, usize, bool)> {
    if options.community_algorithm == CommunityAlgorithm::Louvain {
        return Ok(communities(adjacency, strengths, max_passes, resolution));
    }
    let mut membership: Vec<_> = (0..adjacency.len()).collect();
    let total: f64 = strengths.iter().sum();
    if total == 0.0 {
        return Ok((membership, 0.0, 0, true));
    }
    // Normalize to total strength two, avoiding overflow in core products.
    // CSR diagonals store loop weight once; node strengths count it twice.
    let node_weights: Vec<_> = strengths.iter().map(|w| (w / total) * 2.0).collect();
    let mut offsets = vec![0];
    let mut indices = Vec::new();
    let mut weights = Vec::new();
    for (i, row) in adjacency.iter().enumerate() {
        for (&j, &w) in row {
            if w > 0.0 {
                indices.push(j);
                weights.push(if i == j { w / total } else { (w / total) * 2.0 });
            }
        }
        offsets.push(indices.len());
    }
    let network = CsrNetworkView::new(&offsets, &indices, &weights, &node_weights)
        .context("cannot construct Leiden projection")?;
    let mut rng = SmallRng::seed_from_u64(options.community_seed);
    let mut passes = 0;
    let mut best_membership = None;
    let mut best_modularity = f64::NEG_INFINITY;
    let starts = options.community_starts as usize;
    for trial in 0..starts {
        if trial > 0 {
            membership = (0..adjacency.len()).collect();
        }
        let allocation = max_passes / starts + usize::from(trial < max_passes % starts);
        for _ in 0..allocation {
            let groups = membership.iter().max().map_or(0, |id| id + 1);
            let initial = Clustering::as_defined(membership.clone(), groups);
            let (_, output) = leiden_view(
                &network,
                Some(initial),
                Some(1),
                Some(resolution),
                Some(LEIDEN_RANDOMNESS),
                &mut rng,
                true,
                Some(options.community_local_max_passes),
            )
            .map_err(|error| anyhow::anyhow!("Leiden failed: {error:?}"))?;
            let next = (0..adjacency.len())
                .map(|i| {
                    output
                        .cluster_at(i)
                        .map_err(|e| anyhow::anyhow!("invalid Leiden partition: {e:?}"))
                })
                .collect::<Result<Vec<_>>>()?;
            // Capped local moving may stop before refinement can repair every group.
            // Repair also keeps isolates separate before the next warm start.
            let next = connected_membership(adjacency, &next);
            let candidate_modularity =
                partition_modularity(adjacency, strengths, &next, resolution);
            ensure!(
                candidate_modularity.is_finite(),
                "Leiden produced non-finite modularity"
            );
            passes += 1;
            let unchanged = next == membership;
            if candidate_modularity > best_modularity {
                best_modularity = candidate_modularity;
                best_membership = Some(next.clone());
            }
            membership = next;
            if unchanged && starts == 1 {
                break;
            }
        }
    }
    let membership =
        best_membership.ok_or_else(|| anyhow::anyhow!("Leiden produced no candidate"))?;
    // The core exposes improvement, not convergence or cap exhaustion. Even an
    // unchanged partition is only an observed stopping point under this seed.
    // Keep the best evaluated candidate because stochastic refinement can make a
    // later warm start worse even though it reports that some change occurred.
    Ok((membership, best_modularity, passes, false))
}

pub(super) fn communities(
    adjacency: &[BTreeMap<usize, f64>],
    strengths: &[f64],
    max_passes: usize,
    resolution: f64,
) -> (Vec<usize>, f64, usize, bool) {
    let total: f64 = strengths.iter().sum();
    let mut original: Vec<_> = (0..adjacency.len()).collect();
    if total == 0.0 {
        return (original, 0.0, 0, true);
    }
    // Normalize once so coarsening cannot overflow or change weight scale.
    let mut level: Vec<BTreeMap<usize, f64>> = adjacency
        .iter()
        .map(|row| row.iter().map(|(&j, &w)| (j, w / total)).collect())
        .collect();
    let mut passes = 0;
    let mut converged = false;
    while passes < max_passes {
        let degrees: Vec<f64> = level.iter().map(|row| row.values().sum()).collect();
        let (membership, _, used, stable) =
            local_move(&level, &degrees, max_passes - passes, resolution);
        passes += used;
        // Louvain can leave disconnected communities after a vertex moves away.
        // Split them along positive-weight connectivity before every aggregation.
        let refined = connected_membership(&level, &membership);
        let groups = refined.iter().max().map_or(0, |id| id + 1);
        for group in &mut original {
            *group = refined[*group];
        }
        if groups == level.len() {
            converged = stable;
            break;
        }
        let mut next = vec![BTreeMap::<usize, f64>::new(); groups];
        for (i, row) in level.iter().enumerate() {
            for (&j, &weight) in row {
                *next[refined[i]].entry(refined[j]).or_default() += weight;
            }
        }
        level = next;
    }
    let modularity = partition_modularity(adjacency, strengths, &original, resolution);
    (original, modularity, passes, converged)
}

pub(super) fn partition_modularity(
    adjacency: &[BTreeMap<usize, f64>],
    strengths: &[f64],
    membership: &[usize],
    resolution: f64,
) -> f64 {
    let total: f64 = strengths.iter().sum();
    if total == 0.0 {
        return 0.0;
    }
    let mut totals = BTreeMap::<usize, f64>::new();
    for (i, &w) in strengths.iter().enumerate() {
        *totals.entry(membership[i]).or_default() += w / total;
    }
    let internal: f64 = adjacency
        .iter()
        .enumerate()
        .map(|(i, row)| {
            row.iter()
                .filter(|(j, _)| membership[i] == membership[**j])
                .map(|(_, w)| w / total)
                .sum::<f64>()
        })
        .sum();
    internal - (resolution * totals.values().map(|t| t * t).sum::<f64>()).min(f64::MAX)
}

pub(super) fn pair_cohesion(adjacency: &[BTreeMap<usize, f64>], members: &BTreeSet<usize>) -> f64 {
    if members.len() < 2 {
        return 0.0;
    }
    let pairs: usize = members
        .iter()
        .map(|&i| {
            adjacency[i]
                .iter()
                .filter(|(j, w)| **j != i && **w > 0.0 && members.contains(j))
                .count()
        })
        .sum();
    pairs as f64 / (members.len() as f64 * (members.len() - 1) as f64)
}

pub(super) fn needs_split(size: usize, cohesion: f64, options: &AnalysisOptions) -> bool {
    size > 1
        && (options.max_community_size.is_some_and(|limit| size > limit)
            || options
                .min_cohesion
                .is_some_and(|minimum| cohesion < minimum))
}

pub(super) fn repartition(
    adjacency: &[BTreeMap<usize, f64>],
    membership: &mut [usize],
    options: &AnalysisOptions,
    budget: usize,
) -> Result<(usize, usize, bool)> {
    if options.max_community_size.is_none() && options.min_cohesion.is_none() {
        return Ok((0, 0, true));
    }
    let mut groups = BTreeMap::<usize, Vec<usize>>::new();
    for (node, &group) in membership.iter().enumerate() {
        groups.entry(group).or_default().push(node);
    }
    let mut pending: Vec<_> = groups.into_values().rev().collect();
    let mut next_group = membership.iter().max().map_or(0, |id| id + 1);
    let mut attempts = 0;
    let mut passes = 0;
    let mut converged = true;
    while let Some(nodes) = pending.pop() {
        let members = nodes.iter().copied().collect();
        if !needs_split(nodes.len(), pair_cohesion(adjacency, &members), options) {
            continue;
        }
        if passes == budget {
            converged = false;
            break;
        }
        let indices: BTreeMap<_, _> = nodes
            .iter()
            .enumerate()
            .map(|(i, &node)| (node, i))
            .collect();
        let induced: Vec<BTreeMap<usize, f64>> = nodes
            .iter()
            .map(|&i| {
                adjacency[i]
                    .iter()
                    .filter_map(|(j, &w)| indices.get(j).map(|&local| (local, w)))
                    .collect()
            })
            .collect();
        let strengths: Vec<f64> = induced.iter().map(|row| row.values().sum()).collect();
        let (split, _, used, stable) = partition(
            &induced,
            &strengths,
            budget - passes,
            options.resolution.max(1.0),
            options,
        )?;
        attempts += 1;
        passes += used;
        converged &= stable;
        let mut parts = BTreeMap::<usize, Vec<usize>>::new();
        for (local, &group) in split.iter().enumerate() {
            parts.entry(group).or_default().push(nodes[local]);
        }
        if parts.len() <= 1 {
            continue;
        }
        for part in parts.values() {
            for &node in part {
                membership[node] = next_group;
            }
            next_group += 1;
        }
        pending.extend(parts.into_values().rev());
    }
    Ok((attempts, passes, converged))
}

pub(super) fn is_noise(node: &crate::model::Node) -> bool {
    let file = node.file.replace('\\', "/");
    let basename = file.rsplit('/').next().unwrap_or("");
    let label = node.label.trim();
    if matches!(node.kind.as_str(), "module" | "file" | "group") || !basename.contains('.') {
        return true;
    }
    if matches!(
        label,
        "str"
            | "int"
            | "float"
            | "bool"
            | "bytes"
            | "list"
            | "dict"
            | "set"
            | "tuple"
            | "None"
            | "True"
            | "False"
            | "Any"
            | "Optional"
            | "List"
            | "Dict"
            | "String"
            | "Object"
            | "Promise"
            | "Mock"
            | "MagicMock"
    ) {
        return true;
    }
    if basename.to_lowercase().ends_with(".json")
        && matches!(
            label.to_lowercase().as_str(),
            "name"
                | "type"
                | "properties"
                | "items"
                | "value"
                | "version"
                | "dependencies"
                | "description"
                | "id"
        )
    {
        return true;
    }
    false
}

pub(super) fn import_cycles(snapshot: &GraphSnapshot) -> Vec<Vec<String>> {
    let nodes: BTreeMap<_, _> = snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut graph = BTreeMap::<&str, BTreeSet<&str>>::new();
    let mut reverse = BTreeMap::<&str, BTreeSet<&str>>::new();
    for edge in &snapshot.edges {
        if !edge.directed
            || !matches!(
                edge.relation.as_str(),
                "imports" | "imports_from" | "re_exports"
            )
        {
            continue;
        }
        let a = nodes[edge.source.as_str()].file.as_str();
        let b = nodes[edge.target.as_str()].file.as_str();
        if a.is_empty() || b.is_empty() {
            continue;
        }
        graph.entry(a).or_default().insert(b);
        graph.entry(b).or_default();
        reverse.entry(b).or_default().insert(a);
        reverse.entry(a).or_default();
    }
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for &seed in graph.keys() {
        let mut pending = vec![(seed, false)];
        while let Some((node, done)) = pending.pop() {
            if done {
                order.push(node);
                continue;
            }
            if !visited.insert(node) {
                continue;
            }
            pending.push((node, true));
            pending.extend(graph[node].iter().rev().map(|&next| (next, false)));
        }
    }
    visited.clear();
    let mut cycles = Vec::new();
    for &seed in order.iter().rev() {
        if visited.contains(seed) {
            continue;
        }
        let mut pending = vec![seed];
        let mut group = Vec::new();
        while let Some(node) = pending.pop() {
            if !visited.insert(node) {
                continue;
            }
            group.push(node.to_owned());
            pending.extend(reverse[node].iter().copied());
        }
        if group.len() > 1 || graph[seed].contains(seed) {
            group.sort();
            cycles.push(group);
        }
    }
    cycles.sort();
    cycles
}

pub(super) fn structural_relation(relation: &str) -> bool {
    matches!(
        relation,
        "imports"
            | "imports_from"
            | "re_exports"
            | "contains"
            | "method"
            | "member_of"
            | "defines"
            | "declares"
    )
}

pub(super) fn insight_node(node: &crate::model::Node) -> bool {
    node.kind != "rationale"
        && attributes(&node.metadata)
            .get("file_type")
            .and_then(serde_json::Value::as_str)
            != Some("rationale")
        && !is_noise(node)
}

pub(super) fn source_category(file: &str) -> &'static str {
    match file
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "py" | "rs" | "js" | "jsx" | "ts" | "tsx" | "go" | "java" | "c" | "h" | "cpp" | "hpp"
        | "cs" | "swift" | "rb" | "php" | "ex" | "exs" | "kt" | "scala" => "code",
        "pdf" => "paper",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image",
        _ => "document",
    }
}

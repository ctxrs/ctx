use super::*;

pub(crate) fn diagnose(args: &DiagnoseArgs, db: Option<&Path>, json_output: bool) -> Result<()> {
    let source = SourceArgs {
        snapshot: args.snapshot.clone(),
        analysis: AnalysisArgs {
            community_algorithm: analysis::CommunityAlgorithm::default(),
            community_starts: 1,
            community_seed: 42,
            community_local_max_passes: 100,
            resolution: 1.0,
            max_community_size: None,
            min_cohesion: None,
            exclude_hubs: None,
            include_noise: false,
        },
    };
    let (graph, _) = load(&source, db)?;
    let mut ordered = BTreeMap::<(&str, &str), usize>::new();
    let mut pairs = BTreeMap::<(&str, &str), Vec<&Edge>>::new();
    let mut parallel = BTreeMap::<(bool, &str, &str), usize>::new();
    let mut records = BTreeSet::new();
    let mut duplicate_records = 0;
    for edge in &graph.edges {
        let (a, b) = (edge.source.as_str(), edge.target.as_str());
        let pair = if a <= b { (a, b) } else { (b, a) };
        *ordered.entry((a, b)).or_default() += 1;
        pairs.entry(pair).or_default().push(edge);
        let endpoints = if edge.directed { (a, b) } else { pair };
        *parallel
            .entry((edge.directed, endpoints.0, endpoints.1))
            .or_default() += 1;
        // IDs identify distinct records; compare the remaining stored facts.
        let mut record = edge.clone();
        record.id.clear();
        if !record.directed && a > b {
            std::mem::swap(&mut record.source, &mut record.target);
        }
        if !records.insert(serde_json::to_string(&record)?) {
            duplicate_records += 1;
        }
    }
    let mixed = pairs
        .values()
        .filter(|edges| edges.iter().any(|e| e.directed) && edges.iter().any(|e| !e.directed))
        .count();
    let relation_variants = pairs
        .values()
        .filter(|edges| {
            edges
                .iter()
                .map(|e| &e.relation)
                .collect::<BTreeSet<_>>()
                .len()
                > 1
        })
        .count();
    let mut risks: Vec<_> = pairs.iter().filter(|(_, edges)| edges.len() > 1).collect();
    risks.sort_by(|(a, ae), (b, be)| be.len().cmp(&ae.len()).then(a.cmp(b)));
    let examples: Vec<_> = risks
        .iter()
        .take(args.max_examples as usize)
        .map(|(pair, edges)| {
            let mut samples = edges.to_vec();
            samples.sort_by(|a, b| a.id.cmp(&b.id));
            samples.truncate(5);
            json!({"endpoints":[pair.0,pair.1],"edge_count":edges.len(),
            "undirected_collapse_loss":edges.len()-1,"samples":samples,
            "samples_truncated":samples.len()<edges.len()})
        })
        .collect();
    print(
        &json!({
            "generation":graph.generation,"node_count":graph.nodes.len(),"edge_count":graph.edges.len(),
            "unresolved_reference_count":graph.metadata.get("graf_unresolved_references").and_then(Value::as_array).map(Vec::len),
            "directed_edges":graph.edges.iter().filter(|e| e.directed).count(),
            "undirected_edges":graph.edges.iter().filter(|e| !e.directed).count(),
            "self_loop_edges":graph.edges.iter().filter(|e| e.source==e.target).count(),
            "parallel_edge_groups":parallel.values().filter(|&&n| n>1).count(),
            "parallel_extra_edges":parallel.values().map(|n| n-1).sum::<usize>(),
            "mixed_endpoint_groups":mixed,"relation_variant_groups":relation_variants,
            "duplicate_record_edges":duplicate_records,
            "ordered_unique_endpoint_pairs":ordered.len(),
            "ordered_same_endpoint_collapse_loss":graph.edges.len()-ordered.len(),
            "undirected_unique_endpoint_pairs":pairs.len(),
            "undirected_same_endpoint_collapse_loss":graph.edges.len()-pairs.len(),
            "collapse_risk_groups":risks.len(),"examples":examples,"examples_truncated":examples.len()<risks.len(),
            "methodology":{
                "scope":"Validated stored graph; malformed or dangling input records are rejected during loading. Unresolved references, including external imports, are stored separately and are not dangling edges. Their count is null when no authoritative reference inventory is present. No graph changes are made.",
                "parallel":"Same directed source/target, or same unordered undirected endpoints, with direction kinds kept separate.",
                "collapse":"Hypothetical losses retaining one edge per ordered or unordered endpoint pair, ignoring relation and direction kind. No collapse is performed.",
                "duplicates":"Equal stored edge facts excluding ID; undirected endpoint order is normalized.",
                "examples":"Unordered endpoint groups, largest first then endpoint IDs, with up to five edge samples each."
            }
        }),
        json_output,
    )
}

pub(crate) fn benchmark(args: &BenchmarkArgs, db: Option<&Path>, json_output: bool) -> Result<()> {
    ensure!(
        args.query.len() <= 32,
        "benchmark accepts at most 32 explicit queries"
    );
    ensure!(
        args.query.len() * args.iterations as usize <= 10_000,
        "benchmark accepts at most 10000 measured calls in total"
    );
    ensure!(
        args.query
            .iter()
            .all(|q| !q.trim().is_empty() && q.len() <= 4096),
        "each benchmark query must be nonempty and at most 4096 bytes"
    );
    let path = local_database(db)?;
    regular(&path)?;
    let store = Store::open_read_only(&path)?;
    let stats = store.stats()?;
    let options = QueryOptions {
        depth: args.depth,
        limit: args.limit as usize,
        direction: args.direction.into(),
        relation: args.relation.clone(),
    };
    let mut results = Vec::new();
    for query in &args.query {
        let warmup = store.query(query, &options)?;
        ensure!(
            warmup.generation == stats.generation,
            "graph generation changed during benchmark; retry"
        );
        let mut samples = Vec::new();
        let mut nodes = 0;
        let mut edges = 0;
        let mut unresolved = 0;
        let mut truncated = false;
        for _ in 0..args.iterations {
            let start = Instant::now();
            let result = store.query(query, &options)?;
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            ensure!(
                result.generation == stats.generation,
                "graph generation changed during benchmark; retry"
            );
            nodes += result.nodes.len();
            edges += result.edges.len();
            unresolved += result.unresolved.len();
            truncated |= result.truncated;
        }
        samples.sort_by(f64::total_cmp);
        let n = samples.len();
        let median = if n.is_multiple_of(2) {
            (samples[n / 2 - 1] + samples[n / 2]) / 2.0
        } else {
            samples[n / 2]
        };
        results.push(json!({"query":query,"iterations":args.iterations,"median_ms":median,
            "p95_ms":samples[(95*n).div_ceil(100)-1],"min_ms":samples[0],"max_ms":samples[n-1],
            "result":{"nodes":warmup.nodes.len(),"edges":warmup.edges.len(),"unresolved":warmup.unresolved.len(),"truncated":warmup.truncated},
            "measured_totals":{"nodes":nodes,"edges":edges,"unresolved":unresolved,"any_truncated":truncated}}));
    }
    print(
        &json!({"schema_version":1,"ctx_version":env!("CARGO_PKG_VERSION"),"graf_version":"0.6.0","generation":stats.generation,
        "graph":{"kind":stats.kind,"nodes":stats.nodes,"edges":stats.edges},"options":options,
        "iterations_per_query":args.iterations,"warmup_calls_per_query":1,
        "measured_calls":args.iterations as usize*args.query.len(),"queries":results,
        "methodology":"One read-only SQLite connection; one unmeasured warm-up per query. Timings include SQL search/traversal and result construction, excluding database open, warm-up, and output serialization. Median averages middle values; p95 uses nearest rank. Cache and host load affect results; no external-system comparison."}),
        json_output,
    )
}

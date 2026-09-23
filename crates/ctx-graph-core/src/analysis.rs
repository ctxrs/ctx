//! Deterministic, offline structural analysis. Communities describe connectivity,
//! not architectural intent; call edges are recorded evidence, not execution traces.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use network_partitions::{clustering::Clustering, leiden::leiden_view, network::CsrNetworkView};
use rand::{SeedableRng, rngs::SmallRng};
use serde::{Deserialize, Serialize};

use crate::model::{Edge, GraphSnapshot};

/// Community engine. Louvain is available for compatibility with earlier defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum CommunityAlgorithm {
    #[default]
    Leiden,
    Louvain,
}

fn one_start() -> u32 {
    1
}

fn is_one_start(starts: &u32) -> bool {
    *starts == 1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisOptions {
    pub damping: f64,
    pub tolerance: f64,
    pub max_iterations: usize,
    /// Total Louvain sweeps or Leiden outer iterations, including split retries.
    pub community_max_passes: usize,
    pub community_algorithm: CommunityAlgorithm,
    /// Leiden starts: 1 preserves early stopping; 4 spends fixed allocations
    /// within a shared outer budget of 4..=100, including optional split retries.
    #[serde(skip_serializing_if = "is_one_start")]
    pub community_starts: u32,
    /// Fixed RNG seed for Leiden; ignored by deterministic Louvain.
    pub community_seed: u64,
    /// Leiden only: at most this many times the level's node count are processed
    /// per local-moving call. Positive; not a global sweep or wall-clock budget.
    pub community_local_max_passes: u32,
    /// Positive modularity resolution; larger values favor smaller groups.
    pub resolution: f64,
    /// Optional split trigger, not a guaranteed cap. Unsplit groups are reported.
    pub max_community_size: Option<usize>,
    /// Optional pair-cohesion split trigger in [0, 1]; singletons are exempt.
    pub min_cohesion: Option<f64>,
    /// Remove nodes strictly above this full-graph degree percentile from
    /// partitioning, then reattach by majority neighboring community.
    pub exclude_hubs_percentile: Option<f64>,
    /// Filter source-less, file/container and common builtin/JSON noise from
    /// hub rankings and labels only. Complete metrics/topology remain available.
    pub filter_noise: bool,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            damping: 0.85,
            tolerance: 1e-10,
            max_iterations: 200,
            community_max_passes: 100,
            community_algorithm: CommunityAlgorithm::default(),
            community_starts: 1,
            community_seed: 42,
            community_local_max_passes: 100,
            resolution: 1.0,
            max_community_size: None,
            min_cohesion: None,
            exclude_hubs_percentile: None,
            filter_noise: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub id: String,
    /// Edge incidences: parallel edges count separately, every self edge counts twice.
    pub degree: usize,
    /// Directed incoming arcs plus both orientations of undirected edges.
    pub in_degree: usize,
    /// Directed outgoing arcs plus both orientations of undirected edges.
    pub out_degree: usize,
    pub weighted_degree: f64,
    pub pagerank: f64,
    pub community: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Community {
    pub id: usize,
    pub nodes: Vec<String>,
    /// Fraction of distinct positive-weight, non-self pairs present.
    pub cohesion: f64,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDependency {
    pub source_file: String,
    pub target_file: String,
    pub relation: String,
    pub directed: bool,
    /// Exact underlying records, including source locations and confidence.
    pub evidence: Vec<Edge>,
}

/// Fixed output bounds; analysis still considers every eligible graph record.
pub const SURPRISE_LIMIT: usize = 5;
pub const QUESTION_LIMIT: usize = 7;
/// Refinement temperature for the total-strength-two normalized projection.
const LEIDEN_RANDOMNESS: f64 = 0.001;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurpriseSignal {
    pub code: String,
    pub points: u32,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurprisingConnection {
    pub score: u32,
    pub signals: Vec<SurpriseSignal>,
    pub edge: Edge,
    pub source_file: String,
    pub target_file: String,
    pub source_community: usize,
    pub target_community: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedQuestion {
    pub kind: String,
    pub question: String,
    pub why: String,
    pub node_ids: Vec<String>,
    pub community_ids: Vec<usize>,
    /// At most three original edge records; evidence_count reports the full
    /// number supporting this question. Node-only questions reference node_ids.
    pub edge_evidence: Vec<Edge>,
    pub evidence_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub schema_version: u32,
    pub generation: u64,
    pub nodes: Vec<NodeMetrics>,
    /// Eligible nodes ordered by degree descending, then ID. Metrics remain complete.
    pub hubs: Vec<String>,
    pub excluded_hubs: Vec<String>,
    pub noise_filtered_hubs: Vec<String>,
    pub communities: Vec<Community>,
    pub pagerank_iterations: usize,
    pub pagerank_converged: bool,
    pub community_algorithm: String,
    /// Requested starts per Leiden partition invocation, not executed trials.
    #[serde(default = "one_start", skip_serializing_if = "is_one_start")]
    pub community_starts: u32,
    pub community_modularity: f64,
    pub community_resolution: f64,
    pub community_passes: usize,
    pub community_converged: bool,
    /// False when the engine cannot certify convergence/cap exhaustion.
    pub community_convergence_known: bool,
    /// `louvain_sweeps` or `leiden_iterations`; counts include split retries.
    pub community_pass_unit: String,
    pub community_split_attempts: usize,
    /// Final community IDs still outside requested optional split thresholds.
    pub unsatisfied_community_constraints: Vec<usize>,
    pub file_dependencies: Vec<FileDependency>,
    pub call_edges: Vec<Edge>,
    pub confidence_counts: BTreeMap<String, usize>,
    pub isolates: Vec<String>,
    pub cross_community_edges: Vec<Edge>,
    pub surprises: Vec<SurprisingConnection>,
    pub surprise_candidates: usize,
    pub suggested_questions: Vec<SuggestedQuestion>,
    pub suggested_question_candidates: usize,
    /// Strongly connected file sets under recorded directed import relations.
    pub import_cycles: Vec<Vec<String>>,
    pub methodology: String,
}

pub(crate) fn validate(snapshot: &GraphSnapshot) -> Result<()> {
    crate::snapshot::validate_graph(&snapshot.nodes, &snapshot.edges)
}

/// Unwrap composition provenance without assuming anything about ID prefixes.
pub(crate) fn attributes(mut value: &serde_json::Value) -> &serde_json::Value {
    while value
        .get("project")
        .is_some_and(serde_json::Value::is_string)
        && value
            .get("original_id")
            .is_some_and(serde_json::Value::is_string)
        && value.get("original_metadata").is_some()
    {
        value = &value["original_metadata"];
    }
    value
}

/// Imported identities are a separate namespace from recomputed communities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreservedCommunity {
    /// Composition names from outermost to innermost; empty for a direct import.
    pub project: Vec<String>,
    /// Original integer or string identity, without coercing one into the other.
    pub id: serde_json::Value,
    /// All distinct recorded names; conflicting names remain visible.
    pub names: Vec<String>,
    pub nodes: Vec<String>,
}

/// Read stored memberships without clustering, modifying the graph, or guessing
/// identity from a node-ID prefix. Missing/null/unsupported IDs are unassigned.
pub fn preserved_communities(snapshot: &GraphSnapshot) -> Vec<PreservedCommunity> {
    let mut groups = BTreeMap::<(Vec<String>, String), PreservedCommunity>::new();
    for node in &snapshot.nodes {
        let mut value = &node.metadata;
        let mut project = Vec::new();
        while let (Some(name), Some(_), Some(original)) = (
            value.get("project").and_then(serde_json::Value::as_str),
            value.get("original_id").and_then(serde_json::Value::as_str),
            value.get("original_metadata"),
        ) {
            project.push(name.to_owned());
            value = original;
        }
        let Some(id) = value
            .get("community")
            .filter(|id| id.is_i64() || id.is_u64() || id.as_str().is_some_and(|s| !s.is_empty()))
        else {
            continue;
        };
        let group = groups
            .entry((project.clone(), id.to_string()))
            .or_insert_with(|| PreservedCommunity {
                project,
                id: id.clone(),
                names: Vec::new(),
                nodes: Vec::new(),
            });
        group.nodes.push(node.id.clone());
        if let Some(name) = value
            .get("community_name")
            .and_then(serde_json::Value::as_str)
            && !name.is_empty()
        {
            group.names.push(name.to_owned());
        }
    }
    groups
        .into_values()
        .map(|mut group| {
            group.nodes.sort();
            group.names.sort();
            group.names.dedup();
            group
        })
        .collect()
}

/// A missing weight is 1; explicit weights must be finite and nonnegative.
/// Confidence is never silently interpreted as weight.
fn weight(edge: &Edge) -> Result<f64> {
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

pub fn analyze(snapshot: &GraphSnapshot, options: &AnalysisOptions) -> Result<AnalysisReport> {
    validate(snapshot)?;
    ensure!(
        matches!(options.community_starts, 1 | 4),
        "community starts must be 1 or 4"
    );
    ensure!(
        options.community_starts == 1 || options.community_algorithm == CommunityAlgorithm::Leiden,
        "four community starts require Leiden"
    );
    ensure!(
        options.community_starts == 1 || (4..=100).contains(&options.community_max_passes),
        "four community starts require a total community pass budget in 4..=100"
    );
    ensure!(
        options.damping.is_finite() && (0.0..1.0).contains(&options.damping),
        "damping must be in [0, 1)"
    );
    ensure!(
        options.tolerance.is_finite() && options.tolerance > 0.0,
        "tolerance must be positive and finite"
    );
    ensure!(
        options.max_iterations > 0 && options.community_max_passes > 0,
        "iteration limits must be positive"
    );
    ensure!(
        options.resolution.is_finite() && options.resolution > 0.0,
        "community resolution must be positive and finite"
    );
    ensure!(
        options.max_community_size != Some(0),
        "maximum community size must be positive"
    );
    if let Some(cohesion) = options.min_cohesion {
        ensure!(
            cohesion.is_finite() && (0.0..=1.0).contains(&cohesion),
            "minimum cohesion must be finite and in [0, 1]"
        );
    }
    if let Some(percentile) = options.exclude_hubs_percentile {
        ensure!(
            percentile.is_finite() && (0.0..=100.0).contains(&percentile),
            "hub percentile must be in [0, 100]"
        );
    }
    ensure!(
        options.community_algorithm != CommunityAlgorithm::Leiden
            || options.community_local_max_passes > 0,
        "Leiden local pass limit must be positive"
    );
    let mut nodes: Vec<_> = snapshot.nodes.iter().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let indices: BTreeMap<_, _> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    let count = nodes.len();
    let mut metrics: Vec<_> = nodes
        .iter()
        .map(|n| NodeMetrics {
            id: n.id.clone(),
            degree: 0,
            in_degree: 0,
            out_degree: 0,
            weighted_degree: 0.0,
            pagerank: 0.0,
            community: 0,
        })
        .collect();
    let mut arcs = vec![BTreeMap::<usize, f64>::new(); count];
    let mut adjacency = vec![BTreeMap::<usize, f64>::new(); count];
    let mut edges: Vec<_> = snapshot.edges.iter().collect();
    edges.sort_by(|a, b| a.id.cmp(&b.id));
    let mut dependencies = BTreeMap::<(String, String, String, bool), Vec<Edge>>::new();
    let mut call_edges = Vec::new();
    for edge in edges {
        let a = indices[edge.source.as_str()];
        let b = indices[edge.target.as_str()];
        let w = weight(edge)?;
        for i in [a, b] {
            metrics[i].degree += 1;
            metrics[i].weighted_degree += w;
        }
        metrics[a].out_degree += 1;
        metrics[b].in_degree += 1;
        *arcs[a].entry(b).or_default() += w;
        if !edge.directed {
            metrics[b].out_degree += 1;
            metrics[a].in_degree += 1;
            *arcs[b].entry(a).or_default() += w;
        }
        // Symmetric projection: sum parallel/directed weights, retain self loops twice.
        *adjacency[a].entry(b).or_default() += w;
        *adjacency[b].entry(a).or_default() += w;
        if edge.relation == "calls" {
            call_edges.push(edge.clone());
        }
        if !nodes[a].file.is_empty() && !nodes[b].file.is_empty() && nodes[a].file != nodes[b].file
        {
            let (mut source, mut target) = (nodes[a].file.clone(), nodes[b].file.clone());
            if !edge.directed && source > target {
                std::mem::swap(&mut source, &mut target);
            }
            dependencies
                .entry((source, target, edge.relation.clone(), edge.directed))
                .or_default()
                .push(edge.clone());
        }
    }
    let strengths: Vec<f64> = adjacency.iter().map(|row| row.values().sum()).collect();
    let outgoing: Vec<f64> = arcs.iter().map(|row| row.values().sum()).collect();
    ensure!(
        strengths.iter().chain(&outgoing).all(|v| v.is_finite())
            && strengths.iter().sum::<f64>().is_finite(),
        "sum of edge weights exceeds finite range"
    );
    let (ranks, iterations, rank_converged) = pagerank(&arcs, &outgoing, options);
    let mut degrees: Vec<_> = metrics.iter().map(|n| n.degree).collect();
    degrees.sort_unstable();
    let threshold = options.exclude_hubs_percentile.and_then(|p| {
        if degrees.is_empty() {
            None
        } else {
            Some(
                degrees[((degrees.len() as f64 * p / 100.0) as usize)
                    .saturating_sub(1)
                    .min(degrees.len() - 1)],
            )
        }
    });
    let excluded: Vec<_> = metrics
        .iter()
        .map(|n| threshold.is_some_and(|t| n.degree > t))
        .collect();
    let excluded_hubs = metrics
        .iter()
        .enumerate()
        .filter(|(i, _)| excluded[*i])
        .map(|(_, n)| n.id.clone())
        .collect();
    let noise: Vec<_> = nodes
        .iter()
        .map(|node| options.filter_noise && is_noise(node))
        .collect();
    let noise_filtered_hubs = nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| noise[*i])
        .map(|(_, n)| n.id.clone())
        .collect();
    let filtered = excluded.iter().any(|v| *v).then(|| {
        adjacency
            .iter()
            .enumerate()
            .map(|(i, row)| {
                row.iter()
                    .filter(|(j, _)| !excluded[i] && !excluded[**j])
                    .map(|(&j, &w)| (j, w))
                    .collect::<BTreeMap<usize, f64>>()
            })
            .collect::<Vec<_>>()
    });
    let filtered_strengths = filtered.as_ref().map(|rows| {
        rows.iter()
            .map(|row| row.values().sum())
            .collect::<Vec<f64>>()
    });
    let (mut membership, _, mut passes, mut community_converged) = partition(
        filtered.as_deref().unwrap_or(&adjacency),
        filtered_strengths.as_deref().unwrap_or(&strengths),
        options.community_max_passes,
        options.resolution,
        options,
    )?;
    let mut assigned: Vec<_> = excluded.iter().map(|v| !v).collect();
    // IDs are sorted. Majority votes count distinct positive-weight neighbors,
    // not parallel-edge multiplicity; ties choose the lowest partition ID.
    for i in 0..count {
        if !excluded[i] {
            continue;
        }
        let mut votes = BTreeMap::<usize, usize>::new();
        for (&j, &w) in &adjacency[i] {
            if j != i && assigned[j] && w > 0.0 {
                *votes.entry(membership[j]).or_default() += 1;
            }
        }
        if let Some((&group, _)) = votes
            .iter()
            .max_by(|(a, x), (b, y)| x.cmp(y).then_with(|| b.cmp(a)))
        {
            membership[i] = group;
        }
        assigned[i] = true;
    }
    let (community_split_attempts, split_passes, split_converged) = repartition(
        &adjacency,
        &mut membership,
        options,
        options.community_max_passes - passes,
    )?;
    passes += split_passes;
    community_converged &= split_converged;
    let community_convergence_known = options.community_algorithm == CommunityAlgorithm::Louvain
        || strengths.iter().all(|w| *w == 0.0);
    if !community_convergence_known {
        community_converged = false;
    }
    let modularity = partition_modularity(&adjacency, &strengths, &membership, options.resolution);
    let labels: BTreeMap<_, _> = nodes
        .iter()
        .map(|n| (n.id.as_str(), n.label.as_str()))
        .collect();
    let mut groups = BTreeMap::<usize, Vec<String>>::new();
    for (i, metric) in metrics.iter_mut().enumerate() {
        metric.pagerank = ranks[i];
        groups
            .entry(membership[i])
            .or_default()
            .push(metric.id.clone());
    }
    // Canonical IDs by minimum member ID, independent of input order.
    let mut groups: Vec<_> = groups.into_values().collect();
    groups.sort_by(|a, b| a[0].cmp(&b[0]));
    let groups: Vec<_> = groups
        .into_iter()
        .enumerate()
        .map(|(id, nodes)| {
            let members: BTreeSet<_> = nodes.iter().map(|n| indices[n.as_str()]).collect();
            let cohesion = pair_cohesion(&adjacency, &members);
            let hub = nodes
                .iter()
                .filter(|n| !noise[indices[n.as_str()]] && !excluded[indices[n.as_str()]])
                .min_by(|a, b| {
                    metrics[indices[b.as_str()]]
                        .degree
                        .cmp(&metrics[indices[a.as_str()]].degree)
                        .then(a.cmp(b))
                });
            let label = hub
                .map(|id| labels[id.as_str()].trim())
                .filter(|label| !label.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Community {id}"));
            Community {
                id,
                nodes,
                cohesion,
                label,
            }
        })
        .collect();
    let unsatisfied_community_constraints = groups
        .iter()
        .filter(|c| needs_split(c.nodes.len(), c.cohesion, options))
        .map(|c| c.id)
        .collect();
    for group in &groups {
        for id in &group.nodes {
            metrics[indices[id.as_str()]].community = group.id;
        }
    }
    let mut hubs: Vec<_> = metrics
        .iter()
        .enumerate()
        .filter(|(i, _)| !excluded[*i] && !noise[*i])
        .map(|(_, n)| n)
        .collect();
    hubs.sort_by(|a, b| b.degree.cmp(&a.degree).then(a.id.cmp(&b.id)));
    let hubs: Vec<String> = hubs.into_iter().map(|n| n.id.clone()).collect();
    let mut confidence_counts = BTreeMap::new();
    let mut cross_community_edges = Vec::new();
    let mut ordered_edges: Vec<_> = snapshot.edges.iter().collect();
    ordered_edges.sort_by(|a, b| a.id.cmp(&b.id));
    for edge in ordered_edges {
        *confidence_counts
            .entry(edge.confidence.clone())
            .or_default() += 1;
        if metrics[indices[edge.source.as_str()]].community
            != metrics[indices[edge.target.as_str()]].community
        {
            cross_community_edges.push(edge.clone());
        }
    }
    let isolates = metrics
        .iter()
        .filter(|n| n.degree == 0)
        .map(|n| n.id.clone())
        .collect();
    let import_cycles = import_cycles(snapshot);
    let (surprises, surprise_candidates, suggested_questions, suggested_question_candidates) =
        structural_insights(snapshot, &metrics, &groups, &hubs);
    let mut community_algorithm = match options.community_algorithm {
        CommunityAlgorithm::Louvain => format!(
            "deterministic multilevel Louvain with connectivity splitting (resolution {})",
            options.resolution
        ),
        CommunityAlgorithm::Leiden => format!(
            "native Leiden (network_partitions 0.3.0; seed {}; resolution {}; local pass limit {}) with final connectivity splitting",
            options.community_seed, options.resolution, options.community_local_max_passes
        ),
    };
    if options.community_starts == 4 {
        community_algorithm.push_str(&format!(
            "; four fixed singleton starts; shared outer-call budget {}; earliest best modularity candidate",
            options.community_max_passes
        ));
    }
    let community_iteration_methodology = if options.community_starts == 4 {
        "Its pass count is outer iterations. Four singleton starts share the total outer-call budget in fixed allocations; unchanged partitions do not stop these allocations. Each partition invocation retains the earliest highest-modularity candidate across its starts. A positive-strength initial search exhausts the shared budget, leaving no optional size/cohesion retries; unmet thresholds are reported. More modularity search does not guarantee better semantic communities."
    } else {
        "Its pass count is outer iterations; unchanged consecutive partitions stop iteration but do not certify convergence."
    };
    Ok(AnalysisReport {
        surprises,
        surprise_candidates,
        suggested_questions,
        suggested_question_candidates,
        confidence_counts,
        isolates,
        cross_community_edges,
        import_cycles,
        excluded_hubs,
        noise_filtered_hubs,
        schema_version: snapshot.schema_version,
        generation: snapshot.generation,
        nodes: metrics,
        hubs,
        communities: groups,
        pagerank_iterations: iterations,
        pagerank_converged: rank_converged,
        community_algorithm,
        community_starts: options.community_starts,
        community_convergence_known,
        community_pass_unit: match options.community_algorithm {
            CommunityAlgorithm::Louvain => "louvain_sweeps",
            CommunityAlgorithm::Leiden => "leiden_iterations",
        }
        .into(),
        community_modularity: modularity,
        community_resolution: options.resolution,
        community_passes: passes,
        community_converged,
        community_split_attempts,
        unsatisfied_community_constraints,
        file_dependencies: dependencies
            .into_iter()
            .map(
                |((source_file, target_file, relation, directed), evidence)| FileDependency {
                    source_file,
                    target_file,
                    relation,
                    directed,
                    evidence,
                },
            )
            .collect(),
        call_edges,
        methodology: format!(
            "Weights: nonnegative metadata.weight, default 1; confidence is separate. PageRank: directed arcs, undirected edges in both orientations, uniform teleport and dangling redistribution; absolute L1 convergence. Degree counts incidences, including parallel edges and two per self loop. Communities: selected native Leiden or deterministic multilevel Louvain on the weighted symmetric projection with positive resolution and retained self loops. Leiden uses seeded stochastic refinement and aggregation, retains the highest modularity candidate observed across its bounded warm-start iterations, and uses a dimensionless randomness value after total-strength normalization; its local pass limit bounds node processing per call at each level, not total runtime. {community_iteration_methodology} The core does not report cap exhaustion, so nontrivial Leiden runs report community_converged=false and community_convergence_known=false. Connectivity splitting protects capped partitions. Louvain counts local sweeps. Neither engine supplies semantic naming. Optional size/cohesion thresholds retry induced subgraphs at max(resolution, 1) after hub reattachment, sharing the selected engine's total pass budget; unresolved thresholds are reported without arbitrary forced splits. Optional hub exclusion removes above-percentile nodes before partitioning and reattaches by distinct positive-neighbor majority with deterministic ties; reported modularity uses the full graph after reattachment. Noise filtering affects hub rankings and labels only. File dependencies group recorded cross-file relations; calls are static evidence, not runtime order or proof of execution. Surprises retain the top 5 eligible edges: recorded confidence AMBIGUOUS=3/INFERRED=2/EXTRACTED=1/other=0, cross-source=1, different source directory=2, different source category=2, cross-community=1, degree <=2 to degree >=5=1; score ties use edge ID. Up to 7 question templates rotate across ambiguity, cross-community incidence, inferred hub edges, weak nodes and low cohesion; no betweenness or semantic inference is claimed."
        ),
    })
}

fn pagerank(
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

fn local_move(
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

/// Canonical, connected components within a partition, using positive edges.
fn connected_membership(adjacency: &[BTreeMap<usize, f64>], membership: &[usize]) -> Vec<usize> {
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

fn partition(
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

fn communities(
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

fn partition_modularity(
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

fn pair_cohesion(adjacency: &[BTreeMap<usize, f64>], members: &BTreeSet<usize>) -> f64 {
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

fn needs_split(size: usize, cohesion: f64, options: &AnalysisOptions) -> bool {
    size > 1
        && (options.max_community_size.is_some_and(|limit| size > limit)
            || options
                .min_cohesion
                .is_some_and(|minimum| cohesion < minimum))
}

/// Retry only requested groups on their induced graphs. Each accepted split
/// strictly increases the partition count; all retries share the remaining
/// sweep budget. Thresholds never force arbitrary shards of an inseparable group.
fn repartition(
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

fn is_noise(node: &crate::model::Node) -> bool {
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

fn import_cycles(snapshot: &GraphSnapshot) -> Vec<Vec<String>> {
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

fn structural_relation(relation: &str) -> bool {
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

fn insight_node(node: &crate::model::Node) -> bool {
    node.kind != "rationale"
        && attributes(&node.metadata)
            .get("file_type")
            .and_then(serde_json::Value::as_str)
            != Some("rationale")
        && !is_noise(node)
}

fn source_category(file: &str) -> &'static str {
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

pub(crate) fn cohesion_question(id: usize, label: &str) -> String {
    format!(
        "Does the recorded connectivity of community {id} ({label}) support this grouping, or warrant a closer source review?"
    )
}

fn structural_insights(
    snapshot: &GraphSnapshot,
    metrics: &[NodeMetrics],
    communities: &[Community],
    hubs: &[String],
) -> (
    Vec<SurprisingConnection>,
    usize,
    Vec<SuggestedQuestion>,
    usize,
) {
    let nodes: BTreeMap<_, _> = snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let metrics: BTreeMap<_, _> = metrics.iter().map(|n| (n.id.as_str(), n)).collect();
    let eligible: BTreeSet<_> = snapshot
        .nodes
        .iter()
        .filter(|n| insight_node(n))
        .map(|n| n.id.as_str())
        .collect();
    let mut edges: Vec<_> = snapshot.edges.iter().collect();
    edges.sort_by(|a, b| a.id.cmp(&b.id));
    let mut incidents = BTreeMap::<&str, Vec<&Edge>>::new();
    let mut surprises = Vec::<SurprisingConnection>::new();
    let mut candidate_count = 0;
    let mut ambiguous = Vec::new();
    let mut ambiguous_count = 0;
    for edge in edges {
        if !eligible.contains(edge.source.as_str()) || !eligible.contains(edge.target.as_str()) {
            continue;
        }
        let source = nodes[edge.source.as_str()];
        let target = nodes[edge.target.as_str()];
        let a = metrics[edge.source.as_str()];
        let b = metrics[edge.target.as_str()];
        incidents.entry(&edge.source).or_default().push(edge);
        if edge.source != edge.target {
            incidents.entry(&edge.target).or_default().push(edge);
        }
        if edge.confidence == "AMBIGUOUS" {
            ambiguous_count += 1;
        }
        if edge.confidence == "AMBIGUOUS" && ambiguous.len() < QUESTION_LIMIT {
            ambiguous.push(SuggestedQuestion {
                kind:"ambiguous_edge".into(),
                question:format!("What source evidence confirms or rejects the recorded {} relation between {} and {}?",edge.relation,source.label,target.label),
                why:"The edge is explicitly tagged AMBIGUOUS; this question does not assert that the relation is correct.".into(),
                node_ids:vec![source.id.clone(),target.id.clone()],community_ids:vec![a.community,b.community],edge_evidence:vec![edge.clone()],evidence_count:1,
            });
        }
        let source_file = source.file.replace('\\', "/");
        let target_file = target.file.replace('\\', "/");
        if structural_relation(&edge.relation)
            || (source_file == target_file && a.community == b.community)
        {
            continue;
        }
        candidate_count += 1;
        let confidence = match edge.confidence.as_str() {
            "AMBIGUOUS" => 3,
            "INFERRED" => 2,
            "EXTRACTED" => 1,
            _ => 0,
        };
        let mut signals = vec![SurpriseSignal {
            code: "confidence".into(),
            points: confidence,
            detail: format!("Recorded confidence: {}", edge.confidence),
        }];
        let mut signal = |code: &str, points, detail: String| {
            signals.push(SurpriseSignal {
                code: code.into(),
                points,
                detail,
            })
        };
        if source_file != target_file {
            signal(
                "cross_source",
                1,
                "Endpoints have different recorded source files".into(),
            );
        }
        let source_dir = source_file.rsplit_once('/').map_or("", |(dir, _)| dir);
        let target_dir = target_file.rsplit_once('/').map_or("", |(dir, _)| dir);
        if source_dir != target_dir {
            signal(
                "cross_directory",
                2,
                format!("Different recorded source directories: {source_dir} / {target_dir}"),
            );
        }
        let source_kind = source_category(&source_file);
        let target_kind = source_category(&target_file);
        if source_kind != target_kind {
            signal(
                "cross_category",
                2,
                format!("Source extension categories: {source_kind} / {target_kind}"),
            );
        }
        if a.community != b.community {
            signal(
                "cross_community",
                1,
                format!(
                    "Endpoints belong to structural communities {} and {}",
                    a.community, b.community
                ),
            );
        }
        if a.degree.min(b.degree) <= 2 && a.degree.max(b.degree) >= 5 {
            signal(
                "peripheral_hub",
                1,
                format!(
                    "Endpoint incidence degrees are {} and {}",
                    a.degree, b.degree
                ),
            );
        }
        surprises.push(SurprisingConnection {
            score: signals.iter().map(|s| s.points).sum(),
            signals,
            edge: edge.clone(),
            source_file: source.file.clone(),
            target_file: target.file.clone(),
            source_community: a.community,
            target_community: b.community,
        });
        surprises.sort_by(|a, b| b.score.cmp(&a.score).then(a.edge.id.cmp(&b.edge.id)));
        surprises.truncate(SURPRISE_LIMIT);
    }
    // Cross-community incidence is linear in graph size; never claim all-pairs
    // betweenness or runtime importance from this bounded structural signal.
    let mut bridge_nodes = Vec::new();
    for (&id, incident) in &incidents {
        let community = metrics[id].community;
        let bridge_edges: Vec<_> = incident
            .iter()
            .copied()
            .filter(|e| {
                !structural_relation(&e.relation)
                    && metrics[e.source.as_str()].community != metrics[e.target.as_str()].community
            })
            .collect();
        let others: BTreeSet<_> = bridge_edges
            .iter()
            .flat_map(|e| {
                [
                    metrics[e.source.as_str()].community,
                    metrics[e.target.as_str()].community,
                ]
            })
            .filter(|c| *c != community)
            .collect();
        if !others.is_empty() {
            bridge_nodes.push((id, others, bridge_edges));
        }
    }
    bridge_nodes.sort_by(|(a, ac, ae), (b, bc, be)| {
        bc.len()
            .cmp(&ac.len())
            .then(be.len().cmp(&ae.len()))
            .then(a.cmp(b))
    });
    let bridge_count = bridge_nodes.len();
    let bridges = bridge_nodes.into_iter().take(QUESTION_LIMIT).map(|(id,others,evidence)|SuggestedQuestion {
        kind:"bridge_node".into(),question:format!("Which recorded relations explain how {} connects to other structural communities?",nodes[id].label),
        why:format!("{} nonstructural edges reach {} other communities; ranked by distinct community count then edge count, not betweenness.",evidence.len(),others.len()),
        node_ids:vec![id.to_owned()],community_ids:std::iter::once(metrics[id].community).chain(others).take(3).collect(),edge_evidence:evidence.iter().take(3).map(|e|(*e).clone()).collect(),evidence_count:evidence.len(),
    }).collect::<Vec<_>>();
    let mut inferred = Vec::new();
    for id in hubs
        .iter()
        .filter(|id| eligible.contains(id.as_str()))
        .take(5)
    {
        let evidence: Vec<_> = incidents
            .get(id.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|e| e.confidence == "INFERRED")
            .collect();
        if evidence.len() < 2 {
            continue;
        }
        inferred.push(SuggestedQuestion {kind:"verify_inferred".into(),question:format!("Which of the {} recorded INFERRED edges involving {} can be checked against their sources?",evidence.len(),nodes[id.as_str()].label),why:format!("The node ranks among the top five eligible hubs and has incidence degree {}; inference is recorded metadata, not verified fact.",metrics[id.as_str()].degree),node_ids:vec![id.clone()],community_ids:vec![metrics[id.as_str()].community],edge_evidence:evidence.iter().take(3).map(|e|(*e).clone()).collect(),evidence_count:evidence.len()});
    }
    let weak: Vec<_> = eligible
        .iter()
        .copied()
        .filter(|id| metrics[id].degree <= 1)
        .collect();
    let isolated = if weak.is_empty() {
        Vec::new()
    } else {
        vec![SuggestedQuestion {
            kind: "isolated_nodes".into(),
            question: format!(
                "Are any relationships involving {} absent from this graph?",
                weak.iter()
                    .take(3)
                    .map(|id| nodes[id].label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            why: format!(
                "{} eligible nodes have recorded degree at most one. This does not prove missing functionality; rationale and source-less/container noise are excluded.",
                weak.len()
            ),
            node_ids: weak.iter().take(3).map(|id| (*id).to_owned()).collect(),
            community_ids: Vec::new(),
            edge_evidence: Vec::new(),
            evidence_count: weak.len(),
        }]
    };
    let mut low: Vec<_> = communities
        .iter()
        .filter(|c| {
            c.cohesion < 0.15
                && c.nodes
                    .iter()
                    .filter(|id| eligible.contains(id.as_str()))
                    .count()
                    >= 5
        })
        .collect();
    low.sort_by(|a, b| a.cohesion.total_cmp(&b.cohesion).then(a.id.cmp(&b.id)));
    let low_count = low.len();
    let low = low.into_iter().take(QUESTION_LIMIT).map(|c|SuggestedQuestion {kind:"low_cohesion".into(),question:cohesion_question(c.id, &c.label),why:format!("Recorded pair cohesion is {:.6} across {} nodes; threshold is below 0.15 with at least five eligible non-noise members. This is not a recommendation to restructure the code.",c.cohesion,c.nodes.len()),node_ids:c.nodes.iter().filter(|id|eligible.contains(id.as_str())).take(3).cloned().collect(),community_ids:vec![c.id],edge_evidence:Vec::new(),evidence_count:c.nodes.len()}).collect::<Vec<_>>();
    // Rotate across signal kinds so many ambiguous edges cannot crowd out every
    // other useful question. Within kinds, ordering is stable and evidence-based.
    let question_count =
        ambiguous_count + bridge_count + inferred.len() + isolated.len() + low_count;
    let categories = [ambiguous, bridges, inferred, isolated, low];
    let mut questions = Vec::new();
    for row in 0..QUESTION_LIMIT {
        for category in &categories {
            if let Some(question) = category.get(row) {
                questions.push(question.clone());
            }
            if questions.len() == QUESTION_LIMIT {
                return (surprises, candidate_count, questions, question_count);
            }
        }
    }
    (surprises, candidate_count, questions, question_count)
}

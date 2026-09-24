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

// Canonical, connected components within a partition, using positive edges.

// Retry only requested groups on their induced graphs. Each accepted split
// strictly increases the partition count; all retries share the remaining
// sweep budget. Thresholds never force arbitrary shards of an inseparable group.

pub(crate) fn cohesion_question(id: usize, label: &str) -> String {
    format!(
        "Does the recorded connectivity of community {id} ({label}) support this grouping, or warrant a closer source review?"
    )
}

mod one_start;
use one_start::*;
mod structural_insights;
use structural_insights::*;

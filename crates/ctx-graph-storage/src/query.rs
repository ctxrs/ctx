use crate::{
    model::*,
    store::{StorageLayout, generation, normalized_storage, storage_layout},
};
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
    time::{Duration, Instant},
};

const MAX_SEEDS: usize = 20;
const MAX_EXAMINED: usize = 5_000;
// Ranking and endpoint lookup inspect the catalog; traversal admits neighbors.
// Separate budgets keep a common word or a late endpoint discoverable.
const MAX_RANK_POSTINGS: usize = 250_000;
const MAX_RANK_BYTES: usize = 64 * 1024 * 1024;
// FTS prefix expansion happens before node filters or Rust row budgets. Limit
// its global logical input, including prose, separately from endpoint fields.
// These are source-byte/node caps, not SQLite allocation or wall-clock limits.
const MAX_ENDPOINT_FTS_NODES: usize = 50_000;
const MAX_ENDPOINT_FTS_BYTES: usize = 8 * 1024 * 1024;

// The handler belongs to this request, not the Store: stats and writes must
// not inherit a query's exhausted budget, including after an error.
struct QueryBudget<'a>(&'a Connection);

impl Drop for QueryBudget<'_> {
    fn drop(&mut self) {
        // Store connections are owned handles. The only possible error here
        // concerns externally borrowed handles, which Store never constructs.
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

pub(crate) fn query(conn: &Connection, text: &str, options: &QueryOptions) -> Result<GraphResult> {
    budgeted(conn, || query_snapshot(conn, text, options))
}
pub(crate) fn neighbors(
    conn: &Connection,
    symbol: &str,
    options: &QueryOptions,
) -> Result<GraphResult> {
    budgeted(conn, || neighbors_snapshot(conn, symbol, options))
}
pub(crate) fn path(
    conn: &Connection,
    source: &str,
    target: &str,
    options: &QueryOptions,
) -> Result<PathResult> {
    budgeted(conn, || path_snapshot(conn, source, target, options))
}

// Resolve once before bounded relationship streams, never across transactions
// or write phases. Payloads and ordering continue to use public string IDs.

// Fetch at most `budget` edge rows across indexed directional streams. The
// stored orientation is never changed, even when traversal walks backwards.

// Storage v5 omits the duplicate endpoint-only B-trees. Its relation indexes
// retain `(endpoint, relation, id)`, so walk one bounded relation stream at a
// time and merge by the public edge/reference ID. This preserves the previous
// lexical LIMIT semantics without sorting an entire high-degree endpoint.

/// Exploration order. Shortest-path searches always require breadth first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Traversal {
    #[default]
    Bfs,
    Dfs,
}

/// Optional query controls; the original QueryOptions and Store methods retain
/// their behavior. Files and kinds constrain every returned node. Contexts
/// constrain relations; entries within a filter are ORed, filters are ANDed.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SearchOptions {
    pub graph: QueryOptions,
    pub traversal: Traversal,
    pub contexts: Vec<String>,
    pub files: Vec<String>,
    pub kinds: Vec<String>,
    /// Approximate budget for GraphResult JSON: ceil(UTF-8 bytes / 4). This is
    /// not a model tokenizer count, and excludes the SearchResult wrapper.
    pub token_budget: Option<usize>,
    pub induced_edges: bool,
    pub infer_context: bool,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchResult {
    pub graph: GraphResult,
    pub seeds: Vec<String>,
    pub estimated_tokens: usize,
    pub contexts: Vec<String>,
    pub truncation_reasons: Vec<String>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PathSearchResult {
    pub found: bool,
    pub result: SearchResult,
}

/// Recorded dependency relations followed backwards by default. Containment
/// only supplies initial seeds; it is never part of impact propagation.
pub const DEFAULT_IMPACT_RELATIONS: &[&str] = &[
    "calls",
    "indirect_call",
    "declared_member",
    "declared_callee",
    "references",
    "imports",
    "imports_from",
    "dynamic_import",
    "re_exports",
    "inherits",
    "extends",
    "implements",
    "uses",
    "mixes_in",
    "embeds",
    "requires",
    "type",
    "uses_type",
    "type_of",
    "field_type",
    "parameter_type",
    "return_type",
    "generic_arg",
    "documents",
];

const MEMBERSHIP_RELATIONS: &[&str] = &["contains", "method", "defines"];

/// Impact always traverses incoming dependencies, including either direction
/// of genuinely undirected edges. Other SearchOptions filters/budgets apply.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ImpactOptions {
    pub search: SearchOptions,
    /// Empty uses DEFAULT_IMPACT_RELATIONS. search.graph.relation, if set,
    /// selects a single relation instead, and must agree with this list.
    pub relations: Vec<String>,
}

const MAX_SEARCH_BYTES: usize = 8 * 1024 * 1024;

struct OutputBudget {
    bytes: usize,
    limit: Option<usize>,
}

pub(crate) fn query_extended(
    conn: &Connection,
    text: &str,
    options: &SearchOptions,
    exact_only: bool,
) -> Result<SearchResult> {
    catalog_budgeted(conn, || {
        validate_search(options)?;
        let text = validate_text(text)?;
        let tx = conn.unchecked_transaction()?;
        let mut output = new_search(&tx, contexts(text, options))?;
        let seeds = if exact_only {
            vec![unique_filtered(&tx, text, options)?]
        } else {
            rank_seeds(&tx, text, options)?
        };
        explore(&tx, seeds, options, &mut output)?;
        tx.commit()?;
        finish_search(output)
    })
}

#[derive(Debug)]
enum Step {
    Expand(String, u32),
    Follow(String, u32, Edge),
}

pub(crate) fn path_extended(
    conn: &Connection,
    source: &str,
    target: &str,
    options: &SearchOptions,
) -> Result<PathSearchResult> {
    catalog_budgeted(conn, || {
        validate_search(options)?;
        ensure!(
            options.traversal == Traversal::Bfs,
            "shortest path requires bfs traversal"
        );
        let tx = conn.unchecked_transaction()?;
        let mut output = new_search(&tx, contexts("", options))?;
        let start = resolve_endpoint_in(&tx, validate_text(source)?, options)?;
        let end = resolve_endpoint_in(&tx, validate_text(target)?, options)?;
        output.seeds = vec![start.id.clone()];
        if end.id != start.id {
            output.seeds.push(end.id.clone());
        }
        let mut queue = VecDeque::from([(start.id.clone(), 0)]);
        let mut visited = BTreeSet::from([start.id.clone()]);
        let mut parents = BTreeMap::new();
        let mut examined = 0;
        let mut found = start.id == end.id;
        'search: while !found {
            let Some((id, depth)) = queue.pop_front() else {
                break;
            };
            if examined == MAX_EXAMINED {
                truncate(&mut output, "work_limit");
                break;
            }
            let edges = adjacency(&tx, &id, &options.graph, MAX_EXAMINED - examined)?;
            examined += edges.len();
            if examined == MAX_EXAMINED {
                truncate(&mut output, "work_limit");
            }
            for edge in edges {
                if !context_matches(&edge, &output.contexts) {
                    continue;
                }
                let next = opposite(&edge, &id).to_owned();
                if visited.contains(&next) {
                    continue;
                }
                let candidate = node(&tx, &next)?
                    .ok_or_else(|| anyhow::anyhow!("edge points to missing node {next}"))?;
                if !node_matches(&candidate, options) {
                    continue;
                }
                if depth >= options.graph.depth {
                    truncate(&mut output, "depth_limit");
                    continue;
                }
                if visited.len() == options.graph.limit {
                    truncate(&mut output, "node_limit");
                    continue;
                }
                visited.insert(next.clone());
                parents.insert(next.clone(), (id.clone(), edge));
                if next == end.id {
                    found = true;
                    break 'search;
                }
                queue.push_back((next, depth + 1));
            }
        }
        if found {
            let mut id = end.id.clone();
            let mut ids = vec![id.clone()];
            while id != start.id {
                let (parent, edge) = parents.remove(&id).expect("path has predecessor");
                output.graph.edges.push(edge);
                ids.push(parent.clone());
                id = parent;
            }
            output.graph.edges.reverse();
            ids.reverse();
            for id in ids {
                output
                    .graph
                    .nodes
                    .push(node(&tx, &id)?.expect("path node exists"));
            }
        } else {
            output.graph.nodes.push(start);
            if output.graph.nodes[0].id != end.id {
                if options.graph.limit > 1 {
                    output.graph.nodes.push(end);
                } else {
                    truncate(&mut output, "node_limit");
                    output.seeds.truncate(1);
                }
            }
        }
        // A partial path must never masquerade as a complete found path.
        let mut budget = OutputBudget::new(&output.graph, options)?;
        if options.induced_edges {
            close_edges(&tx, options, &mut output, &mut budget, &mut examined, &[])?;
        }
        tx.commit()?;
        Ok(PathSearchResult {
            found,
            result: finish_search(output)?,
        })
    })
}

#[cfg(test)]
mod tests;

mod crate_neighbors_resolved;
mod outputbudget_new;
mod querybudget_install;

mod budgeted;
use budgeted::*;
mod enriched_search_index;
use enriched_search_index::*;
mod rank_seeds;
use rank_seeds::*;

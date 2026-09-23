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
impl<'a> QueryBudget<'a> {
    fn install(conn: &'a Connection) -> Result<Self> {
        Self::with_steps(conn, 2_000)
    }
    fn with_steps(conn: &'a Connection, max_callbacks: usize) -> Result<Self> {
        let start = Instant::now();
        let mut callbacks = 0;
        conn.progress_handler(
            1_000,
            Some(move || {
                callbacks += 1;
                callbacks >= max_callbacks || start.elapsed() >= Duration::from_secs(2)
            }),
        )?;
        Ok(Self(conn))
    }
}
impl Drop for QueryBudget<'_> {
    fn drop(&mut self) {
        // Store connections are owned handles. The only possible error here
        // concerns externally borrowed handles, which Store never constructs.
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}
fn budgeted<T>(conn: &Connection, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let _budget = QueryBudget::install(conn)?;
    query_errors(work())
}
fn catalog_budgeted<T>(conn: &Connection, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let _budget = QueryBudget::with_steps(conn, 20_000)?;
    query_errors(work())
}
fn query_errors<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| {
        if matches!(error.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::OperationInterrupted) {
            error.context("query exceeded its SQLite work/time budget; use a more specific symbol, fewer search terms, or a smaller depth/limit")
        } else { error }
    })
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

fn validate(options: &QueryOptions) -> Result<()> {
    ensure!(options.depth <= 6, "depth must be between 0 and 6");
    ensure!(
        (1..=500).contains(&options.limit),
        "limit must be between 1 and 500"
    );
    Ok(())
}

fn empty(conn: &Connection) -> Result<GraphResult> {
    let generation = generation(conn)?;
    // Generation pins the read snapshot before selecting physical-format SQL.
    // A Store handle may observe an upgrade in its next transaction.
    storage_layout(conn)?;
    Ok(GraphResult {
        schema_version: SCHEMA_VERSION,
        generation,
        nodes: Vec::new(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        truncated: false,
    })
}

// Resolve once before bounded relationship streams, never across transactions
// or write phases. Payloads and ordering continue to use public string IDs.
fn node_identity(
    conn: &Connection,
    layout: StorageLayout,
    id: &str,
) -> Result<rusqlite::types::Value> {
    Ok(match layout {
        StorageLayout::Legacy => id.to_owned().into(),
        StorageLayout::Compact => conn
            .query_row("SELECT nkey FROM nodes WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })?
            .into(),
    })
}

fn node(conn: &Connection, id: &str) -> Result<Option<Node>> {
    let json: Option<String> = conn
        .query_row("SELECT payload FROM nodes WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    json.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}

fn exact(conn: &Connection, text: &str, include_file: bool) -> Result<Vec<Node>> {
    if let Some(node) = node(conn, text)? {
        return Ok(vec![node]);
    }
    let mut matches = BTreeMap::new();
    let columns: &[&str] = if include_file {
        &["label", "qualified_name", "file"]
    } else {
        &["label", "qualified_name"]
    };
    for column in columns {
        // Each index range is limited before merging, including common labels.
        let sql = format!("SELECT payload FROM nodes WHERE {column}=?1 ORDER BY id LIMIT ?2");
        let mut stmt = conn.prepare(&sql)?;
        for json in stmt.query_map(params![text, (MAX_SEEDS + 1) as i64], |r| {
            r.get::<_, String>(0)
        })? {
            let n: Node = serde_json::from_str(&json?)?;
            matches.insert(n.id.clone(), n);
        }
    }
    Ok(matches.into_values().take(MAX_SEEDS + 1).collect())
}

fn unique(conn: &Connection, text: &str) -> Result<Node> {
    let nodes = exact(conn, text, false)?;
    match nodes.len() {
        0 => bail!("no symbol matches {text:?}"),
        1 => Ok(nodes.into_iter().next().unwrap()),
        _ => {
            let ids: Vec<_> = nodes.iter().map(|n| n.id.as_str()).collect();
            bail!(
                "ambiguous symbol {text:?}; use an exact ID: {}{}",
                ids.join(", "),
                if nodes.len() > MAX_SEEDS {
                    " (additional matches may exist)"
                } else {
                    ""
                }
            )
        }
    }
}

fn query_snapshot(conn: &Connection, text: &str, options: &QueryOptions) -> Result<GraphResult> {
    validate(options)?;
    let text = text.trim();
    ensure!(!text.is_empty(), "query cannot be empty");
    ensure!(text.len() <= 1024, "query exceeds 1024 bytes");
    let tx = conn.unchecked_transaction()?;
    // Reading generation pins the snapshot before any seeds are selected.
    let mut result = empty(&tx)?;
    let mut seeds = exact(&tx, text, true)?;
    if seeds.is_empty() {
        let tokens: Vec<_> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .take(9)
            .collect();
        ensure!(!tokens.is_empty(), "query must contain a searchable word");
        ensure!(
            tokens.len() <= 8,
            "query must contain at most 8 search terms"
        );
        // Literal prefix tokens only: callers cannot inject FTS operators.
        // Rowid order can stop at the candidate cap; no global relevance sort.
        let expression = tokens
            .iter()
            .map(|t| format!("\"{t}\"*"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut stmt = tx.prepare("SELECT n.payload FROM node_search s JOIN nodes n ON n.rowid=s.rowid WHERE node_search MATCH ?1 ORDER BY s.rowid LIMIT ?2")?;
        seeds = stmt
            .query_map(params![expression, (MAX_SEEDS + 1) as i64], |r| {
                r.get::<_, String>(0)
            })?
            .map(|json| Ok(serde_json::from_str(&json?)?))
            .collect::<Result<Vec<_>>>()?;
    }
    if seeds.len() > MAX_SEEDS {
        result.truncated = true;
        seeds.truncate(MAX_SEEDS);
    }
    traverse(&tx, seeds, options, &mut result)?;
    tx.commit()?;
    Ok(result)
}

fn neighbors_snapshot(
    conn: &Connection,
    symbol: &str,
    options: &QueryOptions,
) -> Result<GraphResult> {
    validate(options)?;
    let tx = conn.unchecked_transaction()?;
    let mut result = empty(&tx)?;
    let seed = unique(&tx, symbol)?;
    traverse(&tx, vec![seed], options, &mut result)?;
    tx.commit()?;
    Ok(result)
}

/// Fetch at most `budget` edge rows across indexed directional streams. The
/// stored orientation is never changed, even when traversal walks backwards.
fn adjacency(
    conn: &Connection,
    id: &str,
    options: &QueryOptions,
    budget: usize,
) -> Result<Vec<Edge>> {
    adjacency_filtered(conn, id, options, budget, &[])
}

fn adjacency_filtered(
    conn: &Connection,
    id: &str,
    options: &QueryOptions,
    budget: usize,
    relations: &[String],
) -> Result<Vec<Edge>> {
    let layout = storage_layout(conn)?;
    let identity = node_identity(conn, layout, id)?;
    let mut edges = Vec::new();
    let selected_relations = relation_selection(options.relation.as_deref(), relations);
    let streams = if options.direction == Direction::Incoming {
        [false, true]
    } else {
        [true, false]
    };
    for outgoing in streams {
        let remaining = budget - edges.len();
        if remaining == 0 {
            break;
        }
        let undirected_only = matches!(
            (options.direction, outgoing),
            (Direction::Incoming, true) | (Direction::Outgoing, false)
        );
        // Do not filter self-loops in SQL: even rejected rows could make a
        // LIMIT scan an entire hub. Duplicates consume the budget and are
        // removed only after bounded retrieval.
        edges.extend(edge_relation_rows(
            conn,
            layout,
            &identity,
            outgoing,
            undirected_only,
            selected_relations.as_deref(),
            remaining,
        )?);
    }
    Ok(edges)
}

fn relation_selection(explicit: Option<&str>, relations: &[String]) -> Option<Vec<String>> {
    match explicit {
        Some(relation) => Some(
            if relations.is_empty() || relations.iter().any(|candidate| candidate == relation) {
                vec![relation.to_owned()]
            } else {
                Vec::new()
            },
        ),
        None if relations.is_empty() => None,
        None => Some(
            relations
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        ),
    }
}

// Storage v5 omits the duplicate endpoint-only B-trees. Its relation indexes
// retain `(endpoint, relation, id)`, so walk one bounded relation stream at a
// time and merge by the public edge/reference ID. This preserves the previous
// lexical LIMIT semantics without sorting an entire high-degree endpoint.
fn stream_relations(
    conn: &Connection,
    stream: &str,
    identity: &rusqlite::types::Value,
) -> Result<Vec<String>> {
    let mut relations = Vec::new();
    let mut previous: Option<String> = None;
    loop {
        let relation = if let Some(previous) = &previous {
            conn.query_row(
                &format!("SELECT relation FROM {stream} AND relation>?2 ORDER BY relation LIMIT 1"),
                params![identity.clone(), previous],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        } else {
            conn.query_row(
                &format!("SELECT relation FROM {stream} ORDER BY relation LIMIT 1"),
                [identity.clone()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        };
        let Some(relation) = relation else {
            break;
        };
        previous = Some(relation.clone());
        relations.push(relation);
    }
    Ok(relations)
}

fn edge_relation_rows(
    conn: &Connection,
    layout: StorageLayout,
    identity: &rusqlite::types::Value,
    outgoing: bool,
    undirected_only: bool,
    selected_relations: Option<&[String]>,
    limit: usize,
) -> Result<Vec<Edge>> {
    if limit == 0 || selected_relations.is_some_and(<[String]>::is_empty) {
        return Ok(Vec::new());
    }
    let endpoint = if outgoing { "source" } else { "target" };
    let column = match (layout, outgoing) {
        (StorageLayout::Legacy, true) => "source",
        (StorageLayout::Legacy, false) => "target",
        (StorageLayout::Compact, true) => "source_key",
        (StorageLayout::Compact, false) => "target_key",
    };
    let index = format!(
        "edges_{endpoint}{}_relation",
        if undirected_only { "_direction" } else { "" }
    );
    let stream = format!(
        "edges INDEXED BY {index} WHERE {column}=?1{}",
        if undirected_only {
            " AND directed=0"
        } else {
            ""
        }
    );
    let relations = match selected_relations {
        Some(relations) => relations.to_vec(),
        None => stream_relations(conn, &stream, identity)?,
    };
    let mut first = conn.prepare(&format!(
        "SELECT id,payload FROM {stream} AND relation=?2 ORDER BY id LIMIT 1"
    ))?;
    let mut next = conn.prepare(&format!(
        "SELECT id,payload FROM {stream} AND relation=?2 AND id>?3 ORDER BY id LIMIT 1"
    ))?;
    let mut pending = BinaryHeap::new();
    for relation in relations {
        if let Some((id, payload)) = first
            .query_row(params![identity.clone(), relation], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .optional()?
        {
            pending.push(Reverse((id, relation, payload)));
        }
    }
    let mut edges = Vec::new();
    while edges.len() < limit {
        let Some(Reverse((id, relation, payload))) = pending.pop() else {
            break;
        };
        edges.push(serde_json::from_str(&payload)?);
        if let Some((next_id, next_payload)) = next
            .query_row(params![identity.clone(), relation, id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .optional()?
        {
            pending.push(Reverse((next_id, relation, next_payload)));
        }
    }
    Ok(edges)
}

fn unresolved_relation_rows(
    conn: &Connection,
    layout: StorageLayout,
    identity: &rusqlite::types::Value,
    relation: Option<&str>,
    limit: usize,
) -> Result<Vec<(Reference, String)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let stream = match layout {
        StorageLayout::Legacy => {
            "refs INDEXED BY refs_unresolved_relation WHERE source=?1 AND resolved_target IS NULL"
        }
        StorageLayout::Compact => {
            "refs INDEXED BY refs_unresolved_relation WHERE source_key=?1 AND resolved_target_key IS NULL"
        }
    };
    let relations = match relation {
        Some(relation) => vec![relation.to_owned()],
        None => stream_relations(conn, stream, identity)?,
    };
    let mut first = conn.prepare(&format!(
        "SELECT id,payload,resolution_reason FROM {stream} AND relation=?2 ORDER BY id LIMIT 1"
    ))?;
    let mut next = conn.prepare(&format!(
        "SELECT id,payload,resolution_reason FROM {stream} AND relation=?2 AND id>?3 ORDER BY id LIMIT 1"
    ))?;
    let mut pending = BinaryHeap::new();
    for relation in relations {
        if let Some((id, payload, reason)) = first
            .query_row(params![identity.clone(), relation], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .optional()?
        {
            pending.push(Reverse((id, relation, payload, reason)));
        }
    }
    let mut references = Vec::new();
    while references.len() < limit {
        let Some(Reverse((id, relation, payload, reason))) = pending.pop() else {
            break;
        };
        references.push((serde_json::from_str(&payload)?, reason));
        if let Some((next_id, next_payload, next_reason)) = next
            .query_row(params![identity.clone(), relation, id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .optional()?
        {
            pending.push(Reverse((next_id, relation, next_payload, next_reason)));
        }
    }
    Ok(references)
}

fn opposite<'a>(edge: &'a Edge, id: &str) -> &'a str {
    if edge.source == id {
        &edge.target
    } else {
        &edge.source
    }
}

fn unresolved(
    conn: &Connection,
    id: &str,
    options: &QueryOptions,
    result: &mut GraphResult,
) -> Result<()> {
    if options.direction == Direction::Incoming {
        return Ok(());
    }
    let remaining = options.limit - result.unresolved.len();
    let layout = storage_layout(conn)?;
    let identity = node_identity(conn, layout, id)?;
    let rows = unresolved_relation_rows(
        conn,
        layout,
        &identity,
        options.relation.as_deref(),
        remaining + 1,
    )?;
    for (seen, (reference, reason)) in rows.into_iter().enumerate() {
        if seen == remaining {
            result.truncated = true;
            break;
        }
        result.unresolved.push(UnresolvedReference {
            source: reference.source,
            label: reference.label,
            relation: reference.relation,
            file: reference.file,
            line: reference.line,
            reason,
        });
    }
    Ok(())
}

fn traverse(
    conn: &Connection,
    seeds: Vec<Node>,
    options: &QueryOptions,
    result: &mut GraphResult,
) -> Result<()> {
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    let mut edge_ids = BTreeSet::new();
    for n in seeds {
        if result.nodes.len() == options.limit {
            result.truncated = true;
            break;
        }
        if visited.insert(n.id.clone()) {
            queue.push_back((n.id.clone(), 0));
            result.nodes.push(n);
        }
    }
    let mut examined = 0;
    while let Some((id, depth)) = queue.pop_front() {
        unresolved(conn, &id, options, result)?;
        if depth >= options.depth {
            continue;
        }
        if examined == MAX_EXAMINED {
            result.truncated = true;
            break;
        }
        let edges = adjacency(conn, &id, options, MAX_EXAMINED - examined)?;
        examined += edges.len();
        if examined == MAX_EXAMINED {
            result.truncated = true;
        }
        for edge in edges {
            if edge_ids.contains(&edge.id) {
                continue;
            }
            let next = opposite(&edge, &id);
            if !visited.contains(next) {
                if result.nodes.len() == options.limit {
                    result.truncated = true;
                    continue;
                }
                let n = node(conn, next)?
                    .ok_or_else(|| anyhow::anyhow!("edge points to missing node {next}"))?;
                visited.insert(next.to_owned());
                queue.push_back((next.to_owned(), depth + 1));
                result.nodes.push(n);
            }
            edge_ids.insert(edge.id.clone());
            result.edges.push(edge);
        }
    }
    Ok(())
}

fn path_snapshot(
    conn: &Connection,
    source: &str,
    target: &str,
    options: &QueryOptions,
) -> Result<PathResult> {
    validate(options)?;
    let tx = conn.unchecked_transaction()?;
    let mut result = empty(&tx)?;
    let start = unique(&tx, source)?;
    let end = unique(&tx, target)?;
    let mut visited = BTreeSet::from([start.id.clone()]);
    let mut queue = VecDeque::from([(start.id.clone(), 0)]);
    let mut parents: BTreeMap<String, (String, Edge)> = BTreeMap::new();
    let mut examined = 0;
    let mut found = start.id == end.id;
    'search: while !found {
        let Some((id, depth)) = queue.pop_front() else {
            break;
        };
        if examined == MAX_EXAMINED {
            result.truncated = true;
            break;
        }
        let edges = adjacency(&tx, &id, options, MAX_EXAMINED - examined)?;
        examined += edges.len();
        if examined == MAX_EXAMINED {
            result.truncated = true;
        }
        for edge in edges {
            let next = opposite(&edge, &id).to_owned();
            if visited.contains(&next) {
                continue;
            }
            if depth >= options.depth || visited.len() >= options.limit {
                result.truncated = true;
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
            let (previous, edge) = parents.remove(&id).expect("visited path has a predecessor");
            result.edges.push(edge);
            ids.push(previous.clone());
            id = previous;
        }
        result.edges.reverse();
        ids.reverse();
        for id in ids {
            result
                .nodes
                .push(node(&tx, &id)?.expect("path node exists in this snapshot"));
        }
    } else {
        // Include the resolved endpoints for an actionable unsuccessful result,
        // without implying they are connected or exceeding the node limit.
        result.nodes.push(start);
        if options.limit > 1 && result.nodes[0].id != end.id {
            result.nodes.push(end);
        }
    }
    tx.commit()?;
    Ok(PathResult {
        found,
        graph: result,
    })
}

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

const MAX_SEARCH_BYTES: usize = 8 * 1024 * 1024;

fn validate_search(options: &SearchOptions) -> Result<()> {
    validate(&options.graph)?;
    for filters in [&options.contexts, &options.files, &options.kinds] {
        ensure!(filters.len() <= 32, "at most 32 values per filter");
        ensure!(
            filters
                .iter()
                .all(|s| !s.trim().is_empty() && s.len() <= 1024),
            "filters must be nonempty and at most 1024 bytes each"
        );
    }
    ensure!(
        options
            .token_budget
            .is_none_or(|n| (1..=1_000_000).contains(&n)),
        "token budget must be between 1 and 1000000"
    );
    ensure!(
        options
            .graph
            .relation
            .as_ref()
            .is_none_or(|s| !s.is_empty() && s.len() <= 1024),
        "relation must be nonempty and at most 1024 bytes"
    );
    Ok(())
}

fn validate_text(text: &str) -> Result<&str> {
    let text = text.trim();
    ensure!(
        !text.is_empty() && text.len() <= 1024,
        "query/endpoint must be nonempty and at most 1024 bytes"
    );
    Ok(text)
}

fn normalize(text: &str) -> String {
    use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
    text.nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

fn context_alias(text: &str) -> String {
    let text = normalize(text.trim());
    match text.as_str() {
        "calls" | "called" | "caller" | "callers" | "invoke" | "invokes" | "invoked"
        | "invocation" => "call".into(),
        "imports" | "imported" | "module" | "modules" => "import".into(),
        "exports" | "exported" => "export".into(),
        "fields" | "property" | "properties" | "member" | "members" => "field".into(),
        "param" | "params" | "parameter" | "parameters" | "argument" | "arguments" | "arg"
        | "args" => "parameter_type".into(),
        "return" | "returns" | "returned" => "return_type".into(),
        "generic" | "generics" | "template" | "templates" => "generic_arg".into(),
        "annotation" | "annotations" | "decorator" | "decorators" => "attribute".into(),
        "references" | "referenced" => "reference".into(),
        _ => text,
    }
}

fn contexts(text: &str, options: &SearchOptions) -> Vec<String> {
    let mut values: BTreeSet<_> = options.contexts.iter().map(|s| context_alias(s)).collect();
    if values.is_empty() && options.infer_context {
        for token in text.split(|c: char| !c.is_alphanumeric()) {
            let context = context_alias(token);
            if matches!(
                context.as_str(),
                "call"
                    | "import"
                    | "export"
                    | "field"
                    | "parameter_type"
                    | "return_type"
                    | "generic_arg"
                    | "attribute"
                    | "reference"
            ) {
                values.insert(context);
            }
        }
    }
    values.into_iter().collect()
}

fn attributes(mut value: &serde_json::Value) -> &serde_json::Value {
    while let Some(original) = value.get("original_metadata") {
        value = original;
    }
    value
}

fn context_matches(edge: &Edge, contexts: &[String]) -> bool {
    if contexts.is_empty() {
        return true;
    }
    let attrs = attributes(&edge.metadata);
    match attrs.get("context") {
        Some(serde_json::Value::String(value)) => contexts.contains(&context_alias(value)),
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|v| contexts.contains(&context_alias(v))),
        Some(_) => false,
        // Native facts often have a relation but no redundant context attribute.
        None => contexts.contains(&context_alias(&edge.relation)),
    }
}

fn node_matches(node: &Node, options: &SearchOptions) -> bool {
    (options.files.is_empty() || options.files.contains(&node.file))
        && (options.kinds.is_empty() || options.kinds.contains(&node.kind))
}

fn filter_sql(
    conn: &Connection,
    options: &SearchOptions,
    values: &mut Vec<rusqlite::types::Value>,
) -> Result<String> {
    let mut sql = String::new();
    let kind = if normalized_storage(conn)? {
        "n.kind"
    } else {
        "json_extract(n.payload,'$.kind')"
    };
    for (column, filters) in [("n.file", &options.files), (kind, &options.kinds)] {
        if !filters.is_empty() {
            sql.push_str(&format!(
                " AND {column} IN ({})",
                vec!["?"; filters.len()].join(",")
            ));
            values.extend(filters.iter().cloned().map(Into::into));
        }
    }
    Ok(sql)
}

fn enriched_search_index(conn: &Connection) -> Result<bool> {
    let has_version: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('metadata') WHERE name='search_version')",
        [],
        |row| row.get(0),
    )?;
    if !has_version {
        return Ok(false);
    }
    Ok(conn.query_row(
        // Only v5 promises the explicit NFKD/accent-folded companion. Older
        // unicode61 postings miss compound diacritics; future formats need review.
        "SELECT search_version = 5 FROM metadata WHERE singleton=1",
        [],
        |row| row.get(0),
    )?)
}

fn within_endpoint_fts_bounds(conn: &Connection) -> Result<bool> {
    // nodes.search is the complete input maintained in node_search by writes.
    // Do not apply endpoint filters: prefix setup visits global postings, and
    // named prose/attributes can dwarf the ID/label/qualified-name fields.
    // Read lengths only and stop at the first exceeded cap. A large SQLite
    // value or historical FTS segments can still cost I/O outside these caps.
    let mut stmt = conn.prepare("SELECT length(CAST(search AS BLOB)) FROM nodes LIMIT ?")?;
    let mut rows = stmt.query([(MAX_ENDPOINT_FTS_NODES + 1) as i64])?;
    let mut remaining = MAX_ENDPOINT_FTS_BYTES;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let bytes = usize::try_from(row.get::<_, i64>(0)?)?;
        if count == MAX_ENDPOINT_FTS_NODES || bytes > remaining {
            return Ok(false);
        }
        count += 1;
        remaining -= bytes;
    }
    Ok(true)
}

fn within_endpoint_bounds(
    conn: &Connection,
    filters: &str,
    values: &[rusqlite::types::Value],
) -> Result<bool> {
    let record_bytes = "length(CAST(n.id AS BLOB)) + length(CAST(n.label AS BLOB)) + COALESCE(length(CAST(n.qualified_name AS BLOB)), 0)";
    let sql = format!(
        "SELECT COUNT(*) <= {MAX_RANK_POSTINGS}
                AND COALESCE(SUM({record_bytes}), 0) <= {MAX_RANK_BYTES}
                AND COALESCE(MAX({record_bytes}), 0) <= {MAX_SEARCH_BYTES}
         FROM nodes n WHERE 1=1{filters}"
    );
    let mut stmt = conn.prepare(&sql)?;
    Ok(
        stmt.query_row(rusqlite::params_from_iter(values.iter().cloned()), |row| {
            row.get(0)
        })?,
    )
}

fn endpoint_tier(
    id: &str,
    label: &str,
    qualified: Option<&str>,
    term: &str,
    callable: &str,
    qualified_tail: bool,
) -> Option<usize> {
    let normalized = [
        normalize(id),
        normalize(label),
        normalize(qualified.unwrap_or("")),
    ];
    if normalized.iter().any(|value| value == term) || normalized[1] == callable {
        Some(0)
    } else if qualified_tail
        && normalized[2].strip_suffix(term).is_some_and(|prefix| {
            prefix.is_empty()
                || prefix
                    .chars()
                    .next_back()
                    .is_some_and(|c| c == '.' || c.is_whitespace())
        })
    {
        Some(1)
    } else if normalized.iter().any(|value| value.starts_with(term)) {
        Some(2)
    } else if normalized.iter().any(|value| value.contains(term)) {
        Some(3)
    } else {
        None
    }
}

fn scan_endpoint_rows(
    conn: &Connection,
    sql: &str,
    values: &[rusqlite::types::Value],
    term: &str,
    callable: &str,
    qualified_tail: bool,
) -> Result<[Vec<String>; 4]> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(values.iter().cloned()))?;
    let mut tiers: [Vec<String>; 4] = Default::default();
    let start = Instant::now();
    let mut bytes = 0;
    let mut examined = 0;
    while let Some(row) = rows.next()? {
        ensure!(
            examined < MAX_RANK_POSTINGS && start.elapsed() < Duration::from_secs(2),
            "endpoint lookup exceeded its work/time budget; use an exact ID or a smaller file/kind scope"
        );
        examined += 1;
        let id: &str = row.get_ref(0)?.as_str()?;
        let label: &str = row.get_ref(1)?.as_str()?;
        let qualified: Option<&str> = row.get_ref(2)?.as_str_or_null()?;
        // Normalization runs outside SQLite's VM guard. Bound its input too,
        // before allocating strings for arbitrarily long imported identifiers.
        let record_bytes = id.len() + label.len() + qualified.map_or(0, str::len);
        bytes += record_bytes;
        ensure!(
            record_bytes <= MAX_SEARCH_BYTES && bytes <= MAX_RANK_BYTES,
            "endpoint lookup exceeded its normalization byte budget; use an exact ID or a smaller file/kind scope"
        );
        if let Some(tier) = endpoint_tier(id, label, qualified, term, callable, qualified_tail)
            && tiers[tier].len() < 2
        {
            tiers[tier].push(id.to_owned());
        }
        if tiers[0].len() == 2 {
            break;
        }
    }
    Ok(tiers)
}

fn exact_filtered(
    conn: &Connection,
    text: &str,
    options: &SearchOptions,
    include_file: bool,
) -> Result<Vec<Node>> {
    if let Some(node) = node(conn, text)? {
        return Ok(if node_matches(&node, options) {
            vec![node]
        } else {
            Vec::new()
        });
    }
    let mut matches = BTreeMap::new();
    let columns: &[&str] = if include_file {
        &["label", "qualified_name", "file"]
    } else {
        &["label", "qualified_name"]
    };
    for column in columns {
        let mut values: Vec<rusqlite::types::Value> = vec![text.to_owned().into()];
        let filters = filter_sql(conn, options, &mut values)?;
        let sql = format!(
            "SELECT n.payload FROM nodes n WHERE n.{column}=?{filters} ORDER BY n.id LIMIT {}",
            MAX_SEEDS + 1
        );
        let mut stmt = conn.prepare(&sql)?;
        for row in stmt.query_map(rusqlite::params_from_iter(values), |r| {
            r.get::<_, String>(0)
        })? {
            let node: Node = serde_json::from_str(&row?)?;
            matches.insert(node.id.clone(), node);
        }
    }
    if !matches.is_empty() {
        return Ok(matches.into_values().take(MAX_SEEDS + 1).collect());
    }
    // A literal ID/label containing :: wins. Otherwise this is a strict file
    // scope: an absent match must not drift to a similarly named foreign file.
    if let Some((file, symbol)) = text.split_once("::") {
        ensure!(
            !file.is_empty() && !symbol.is_empty(),
            "scoped endpoint requires file::symbol"
        );
        // Normalize only the scope, after raw whole-input matches. Exact symbol
        // spelling must win for ./ and in-root absolute paths before accent
        // folding; rebuilding the whole input could select an unrelated ID.
        let file = source_path(conn, file)?;
        for column in ["id", "label", "qualified_name"] {
            let mut values: Vec<rusqlite::types::Value> =
                vec![file.to_owned().into(), symbol.to_owned().into()];
            let filters = filter_sql(conn, options, &mut values)?;
            let sql = format!(
                "SELECT n.payload FROM nodes n WHERE n.file=? AND n.{column}=?{filters} ORDER BY n.id LIMIT {}",
                MAX_SEEDS + 1
            );
            let mut stmt = conn.prepare(&sql)?;
            for row in stmt.query_map(rusqlite::params_from_iter(values), |r| {
                r.get::<_, String>(0)
            })? {
                let node: Node = serde_json::from_str(&row?)?;
                matches.insert(node.id.clone(), node);
            }
        }
    }
    Ok(matches.into_values().take(MAX_SEEDS + 1).collect())
}

fn unique_filtered(conn: &Connection, text: &str, options: &SearchOptions) -> Result<Node> {
    let nodes = exact_filtered(conn, validate_text(text)?, options, false)?;
    require_unique(nodes, text)
}

fn require_unique(nodes: Vec<Node>, text: &str) -> Result<Node> {
    match nodes.len() {
        0 => bail!("no symbol matches {text:?} within the requested scope"),
        1 => Ok(nodes.into_iter().next().unwrap()),
        _ => bail!(
            "ambiguous symbol {text:?}; use an exact ID or file::symbol: {}{}",
            nodes
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if nodes.len() > MAX_SEEDS {
                " (additional matches may exist)"
            } else {
                ""
            }
        ),
    }
}

fn resolve_relation(
    conn: &Connection,
    id: &str,
    text: &str,
    options: &QueryOptions,
) -> Result<String> {
    let layout = storage_layout(conn)?;
    let identity = node_identity(conn, layout, id)?;
    let mut streams = Vec::new();
    let columns = match layout {
        StorageLayout::Legacy => ["source", "target"],
        StorageLayout::Compact => ["source_key", "target_key"],
    };
    for (column, outgoing) in [(columns[0], true), (columns[1], false)] {
        let undirected_only = matches!(
            (options.direction, outgoing),
            (Direction::Incoming, true) | (Direction::Outgoing, false)
        );
        streams.push(format!(
            "edges INDEXED BY edges_{}{}_relation WHERE {column}=?1{}",
            if outgoing { "source" } else { "target" },
            if undirected_only { "_direction" } else { "" },
            if undirected_only {
                " AND directed=0"
            } else {
                ""
            }
        ));
    }
    if options.direction != Direction::Incoming {
        streams.push(
            match layout {
                StorageLayout::Legacy => "refs INDEXED BY refs_unresolved_relation WHERE source=?1 AND resolved_target IS NULL",
                StorageLayout::Compact => {
                    "refs INDEXED BY refs_unresolved_relation WHERE source_key=?1 AND resolved_target_key IS NULL"
                }
            }
            .into(),
        );
    }
    // Exact relation probes use directional relation indexes even for huge hubs.
    for stream in &streams {
        if conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {stream} AND relation=?2)"),
            params![identity, text],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(text.to_owned());
        }
    }
    let term = normalize(text.trim());
    ensure!(
        !term.is_empty(),
        "relation has no searchable normalized spelling"
    );
    let mut tiers: [BTreeSet<String>; 3] = Default::default();
    let start = Instant::now();
    let mut examined = 0;
    let mut bytes = 0;
    for stream in &streams {
        // Seek to the next distinct indexed name. SELECT DISTINCT would still
        // walk every edge at a high-degree hub before discovering a second name.
        let mut first = conn.prepare(&format!(
            "SELECT relation FROM {stream} ORDER BY relation LIMIT 1"
        ))?;
        let mut next = conn.prepare(&format!(
            "SELECT relation FROM {stream} AND relation>?2 ORDER BY relation LIMIT 1"
        ))?;
        let mut relation: Option<String> = first.query_row([&identity], |r| r.get(0)).optional()?;
        while let Some(name) = relation {
            ensure!(
                examined < MAX_EXAMINED && start.elapsed() < Duration::from_secs(2),
                "relation lookup exceeded its work/time budget; use an exact relation"
            );
            examined += 1;
            bytes += name.len();
            ensure!(
                bytes <= MAX_SEARCH_BYTES,
                "relation lookup exceeded its byte budget; use an exact relation"
            );
            let folded = normalize(&name);
            let tier = if folded == term {
                Some(0)
            } else if folded.starts_with(&term) {
                Some(1)
            } else if folded.contains(&term) {
                Some(2)
            } else {
                None
            };
            if let Some(tier) = tier {
                // Keep the lexically first two distinct names for a stable error.
                tiers[tier].insert(name.clone());
                if tiers[tier].len() > 2 {
                    tiers[tier].pop_last();
                }
            }
            relation = next
                .query_row(params![identity, name], |r| r.get(0))
                .optional()?;
        }
    }
    let matches = tiers
        .into_iter()
        .find(|names| !names.is_empty())
        .unwrap_or_default();
    ensure!(
        matches.len() <= 1,
        "ambiguous relation {text:?}; use an exact relation: {}",
        matches.iter().cloned().collect::<Vec<_>>().join(", ")
    );
    // An unmatched filter must stay restrictive, never become all relations.
    Ok(matches
        .into_iter()
        .next()
        .unwrap_or_else(|| text.to_owned()))
}

fn source_path(conn: &Connection, text: &str) -> Result<String> {
    use std::path::{Component, Path};
    let root: Option<String> =
        conn.query_row("SELECT root FROM metadata WHERE singleton=1", [], |r| {
            r.get(0)
        })?;
    let original = Path::new(text);
    let path = if original.is_absolute() {
        root.as_deref()
            .and_then(|root| original.strip_prefix(root).ok())
            .unwrap_or(original)
    } else {
        original
    };
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::Normal(value) => parts.push(value.to_string_lossy()),
            Component::CurDir => {}
            _ => return Ok(text.to_owned()),
        }
    }
    Ok(parts.join("/"))
}

fn file_root(node: &Node, file: &str) -> bool {
    matches!(node.kind.as_str(), "file" | "module")
        || (node.line == Some(1)
            && std::path::Path::new(file)
                .file_name()
                .and_then(|s| s.to_str())
                == Some(node.label.as_str()))
}

fn file_nodes(
    conn: &Connection,
    file: &str,
    options: &SearchOptions,
    limit: usize,
) -> Result<Vec<Node>> {
    let basename = std::path::Path::new(file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(file);
    let mut values: Vec<rusqlite::types::Value> = vec![file.to_owned().into()];
    let filters = filter_sql(conn, options, &mut values)?;
    values.push(basename.to_owned().into());
    values.push((limit as i64).into());
    let root_order = if normalized_storage(conn)? {
        "n.kind IN ('file','module') OR (n.line=1 AND n.label=?)"
    } else {
        "json_extract(n.payload,'$.kind') IN ('file','module') OR (json_extract(n.payload,'$.line')=1 AND n.label=?)"
    };
    let sql = format!(
        "SELECT n.payload FROM nodes n WHERE n.file=?{filters}
        ORDER BY CASE WHEN {root_order} THEN 0 ELSE 1 END,n.id LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(rusqlite::params_from_iter(values), |r| {
        r.get::<_, String>(0)
    })?
    .map(|row| Ok(serde_json::from_str(&row?)?))
    .collect()
}

fn resolve_endpoint_in(conn: &Connection, text: &str, options: &SearchOptions) -> Result<Node> {
    let exact = exact_filtered(conn, text, options, false)?;
    if !exact.is_empty() || node(conn, text)?.is_some() {
        return require_unique(exact, text);
    }
    let scope = text.split_once("::");
    if scope.is_none() {
        let file = source_path(conn, text)?;
        let mut candidates = file_nodes(conn, &file, options, 2)?;
        if !candidates.is_empty() {
            if file_root(&candidates[0], &file)
                && candidates.get(1).is_none_or(|n| !file_root(n, &file))
            {
                return Ok(candidates.remove(0));
            }
            return require_unique(candidates, text);
        }
    }
    let (file, term) = scope.map_or((None, text), |(file, term)| (Some(file), term));
    let term = normalize(term);
    // A combining-mark-only query must not become an empty prefix of all nodes.
    ensure!(
        !term.is_empty(),
        "endpoint has no searchable normalized spelling"
    );
    let callable = term
        .strip_suffix("()")
        .map_or_else(|| format!("{term}()"), str::to_owned);
    // A complete scoped Owner.member spelling is stronger than a partial
    // match in a longer name or descendant, but never outranks full equality.
    let qualified_tail = file.is_some()
        && term.contains('.')
        && term.split('.').all(|component| !component.is_empty());
    let mut values = Vec::new();
    let mut filters = filter_sql(conn, options, &mut values)?;
    if let Some(file) = file {
        filters.push_str(" AND n.file=?");
        values.push(source_path(conn, file)?.into());
    }
    // V5 stores NFKC spellings plus an explicit NFKD/accent-folded companion,
    // making ASCII exact/prefix candidates complete. First bound the global
    // FTS input, then keep the independent filtered endpoint-field budget.
    let fts_safe = term.len() >= 3
        && term.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && enriched_search_index(conn)?
        && within_endpoint_fts_bounds(conn)?
        && within_endpoint_bounds(conn, &filters, &values)?;
    let mut tiers = if fts_safe {
        let expression = format!("\"{term}\"*");
        let mut fts_values = vec![expression.into()];
        fts_values.extend(values.iter().cloned());
        let fts_sql = format!(
            "SELECT n.id,n.label,n.qualified_name
             FROM node_search CROSS JOIN nodes n ON n.rowid=node_search.rowid
             WHERE node_search MATCH ?{filters} ORDER BY n.id"
        );
        let fts_tiers = scan_endpoint_rows(
            conn,
            &fts_sql,
            &fts_values,
            &term,
            &callable,
            qualified_tail,
        )?;
        if fts_tiers[0].is_empty() && fts_tiers[2].is_empty() {
            // Tokenization cannot prove arbitrary literal substrings, so keep
            // the complete endpoint scan when no exact/prefix tier was found.
            Default::default()
        } else {
            fts_tiers
        }
    } else {
        Default::default()
    };
    if tiers[0].is_empty() && tiers[2].is_empty() {
        let sql = format!(
            "SELECT n.id,n.label,n.qualified_name FROM nodes n WHERE 1=1{filters}
            ORDER BY n.id LIMIT {}",
            MAX_RANK_POSTINGS + 1
        );
        tiers = scan_endpoint_rows(conn, &sql, &values, &term, &callable, qualified_tail)?;
    }
    let ids = tiers
        .into_iter()
        .find(|ids| !ids.is_empty())
        .unwrap_or_default();
    let nodes = ids
        .iter()
        .map(|id| node(conn, id)?.ok_or_else(|| anyhow::anyhow!("missing endpoint {id}")))
        .collect::<Result<Vec<_>>>()?;
    require_unique(nodes, text)
}

fn impact_seeds(
    conn: &Connection,
    text: &str,
    options: &SearchOptions,
    output: &mut SearchResult,
) -> Result<(Vec<Node>, usize)> {
    let exact = exact_filtered(conn, text, options, false)?;
    let mut seeds = if !exact.is_empty() || node(conn, text)?.is_some() {
        vec![require_unique(exact, text)?]
    } else if !text.contains("::") {
        file_nodes(
            conn,
            &source_path(conn, text)?,
            options,
            options.graph.limit + 1,
        )?
    } else {
        Vec::new()
    };
    if seeds.is_empty() {
        seeds.push(resolve_endpoint_in(conn, text, options)?);
    }
    if seeds.len() == 1 && !seeds[0].file.is_empty() && file_root(&seeds[0], &seeds[0].file) {
        let root = seeds[0].id.clone();
        for member in file_nodes(conn, &seeds[0].file, options, options.graph.limit + 1)? {
            if member.id != root {
                seeds.push(member);
            }
        }
    }
    let mut examined = seeds.len();
    if seeds.len() > options.graph.limit {
        truncate(output, "seed_limit");
        seeds.truncate(options.graph.limit);
    }
    let mut seen: BTreeSet<_> = seeds.iter().map(|n| n.id.clone()).collect();
    let mut cursor = 0;
    let layout = storage_layout(conn)?;
    // Only descendants of the original seeds are added here. Dependencies
    // discovered by the later reverse walk never expand their own members.
    let membership_relations = [
        "contains".to_owned(),
        "defines".to_owned(),
        "method".to_owned(),
    ];
    'members: while cursor < seeds.len() && examined < MAX_EXAMINED {
        let identity = node_identity(conn, layout, &seeds[cursor].id)?;
        let rows = edge_relation_rows(
            conn,
            layout,
            &identity,
            true,
            false,
            Some(&membership_relations),
            MAX_EXAMINED - examined,
        )?;
        cursor += 1;
        for edge in rows {
            examined += 1;
            let id = edge.target;
            if seen.contains(&id) {
                continue;
            }
            let member = node(conn, &id)?
                .ok_or_else(|| anyhow::anyhow!("membership points to missing node {id}"))?;
            if !node_matches(&member, options) {
                continue;
            }
            if seeds.len() == options.graph.limit {
                truncate(output, "seed_limit");
                break 'members;
            }
            seen.insert(id);
            seeds.push(member);
        }
    }
    if examined == MAX_EXAMINED {
        truncate(output, "work_limit");
    }
    Ok((seeds, examined))
}

fn search_terms(conn: &Connection, text: &str) -> Result<Vec<String>> {
    use unicode_normalization::UnicodeNormalization;
    // Compatibility composition agrees with the enriched write-side index,
    // without decomposing Hangul or dropping Greek accents before MATCH.
    let mut spelling: String = text.nfkc().collect();
    if spelling != text {
        let has_version: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('metadata') WHERE name='search_version')",
            [],
            |r| r.get(0),
        )?;
        let version: i64 = if has_version {
            conn.query_row(
                "SELECT search_version FROM metadata WHERE singleton=1",
                [],
                |r| r.get(0),
            )?
        } else {
            0
        };
        // Read-only queries must still find literal postings in old indexes.
        // Only an explicit write adds compatibility-normalized search text.
        if version < 2 {
            spelling = text.to_owned();
        }
    }
    let mut all = BTreeSet::new();
    for token in spelling
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
    {
        // Ranking's accent stripping is deliberately separate from FTS spelling.
        let token = token.to_owned();
        let chars: Vec<_> = token.chars().collect();
        if chars.len() > 2 && chars.iter().all(|c| crate::store::cjk(*c)) {
            for pair in chars.windows(2) {
                all.insert(pair.iter().collect::<String>());
            }
        } else {
            all.insert(token);
        }
    }
    let mut terms: Vec<_> = all
        .iter()
        .filter(|s| {
            !matches!(
                normalize(s).as_str(),
                "a" | "an"
                    | "and"
                    | "are"
                    | "does"
                    | "for"
                    | "how"
                    | "in"
                    | "is"
                    | "of"
                    | "or"
                    | "the"
                    | "to"
                    | "what"
                    | "where"
                    | "which"
                    | "who"
                    | "why"
            )
        })
        .cloned()
        .collect();
    if terms.is_empty() {
        terms.extend(all);
    }
    let intent = |s: &str| {
        matches!(
            normalize(s).as_str(),
            "call"
                | "calls"
                | "called"
                | "caller"
                | "callers"
                | "invoke"
                | "invokes"
                | "use"
                | "uses"
                | "used"
                | "using"
                | "import"
                | "imports"
                | "export"
                | "exports"
                | "extend"
                | "extends"
                | "implement"
                | "implements"
                | "depend"
                | "depends"
                | "reference"
                | "references"
        )
    };
    if terms.iter().any(|t| !intent(t)) {
        terms.retain(|t| !intent(t));
    }
    ensure!(
        !terms.is_empty() && terms.len() <= 8,
        "query must contain 1 to 8 distinct searchable terms after removing question words"
    );
    Ok(terms)
}

fn rank_seeds(conn: &Connection, text: &str, options: &SearchOptions) -> Result<Vec<Node>> {
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

fn truncate(output: &mut SearchResult, reason: &str) {
    output.graph.truncated = true;
    if !output.truncation_reasons.iter().any(|r| r == reason) {
        output.truncation_reasons.push(reason.into());
    }
}

struct OutputBudget {
    bytes: usize,
    limit: Option<usize>,
}
impl OutputBudget {
    fn new(graph: &GraphResult, options: &SearchOptions) -> Result<Self> {
        let budget = Self {
            bytes: serde_json::to_vec(graph)?.len() + 1,
            limit: options.token_budget.map(|n| n * 4),
        };
        ensure!(
            budget.limit.is_none_or(|limit| budget.bytes <= limit),
            "token budget cannot hold the complete result; increase the budget"
        );
        Ok(budget)
    }
    fn cost(value: &impl serde::Serialize) -> Result<usize> {
        Ok(serde_json::to_vec(value)?.len() + 1)
    }
    fn take(&mut self, bytes: usize, output: &mut SearchResult) -> bool {
        if self
            .limit
            .is_some_and(|limit| bytes > limit.saturating_sub(self.bytes))
        {
            truncate(output, "token_budget");
            false
        } else {
            self.bytes += bytes;
            true
        }
    }
}

fn new_search(conn: &Connection, contexts: Vec<String>) -> Result<SearchResult> {
    Ok(SearchResult {
        graph: empty(conn)?,
        seeds: Vec::new(),
        estimated_tokens: 0,
        contexts,
        truncation_reasons: Vec::new(),
    })
}
fn finish_search(mut output: SearchResult) -> Result<SearchResult> {
    output.estimated_tokens = serde_json::to_vec(&output.graph)?.len().div_ceil(4);
    Ok(output)
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

fn explore(
    conn: &Connection,
    seeds: Vec<Node>,
    options: &SearchOptions,
    output: &mut SearchResult,
) -> Result<()> {
    explore_filtered(conn, seeds, options, output, &[], 0, MAX_SEEDS)
}

fn explore_filtered(
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

fn close_edges(
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

fn collect_unresolved(
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
mod tests {
    use super::*;

    #[test]
    fn endpoint_fts_requires_bounded_global_prose_and_known_projection() -> Result<()> {
        use crate::store::Store;
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        // Same endpoint and scope: small/just-under-cap input uses FTS, while
        // excess prose must bypass it even when the prose belongs to another file.
        for (version, repetitions, prose_file, fast) in [
            (5, 0, "near.rs", true),
            (5, 838_840, "near.rs", true),
            (5, 840_000, "near.rs", false),
            (5, 840_000, "far.rs", false),
            (6, 0, "near.rs", false),
        ] {
            let make_node = |id: &str, label: &str, file: &str, metadata| Node {
                id: id.into(),
                label: label.into(),
                kind: "function".into(),
                file: file.into(),
                line: None,
                end_line: None,
                qualified_name: None,
                binding_key: None,
                metadata,
            };
            let dir = tempfile::tempdir()?;
            let mut store = Store::create(&dir.path().join("graph.db"))?;
            store.import_graph(ImportedGraph {
                nodes: vec![
                    make_node("a", "Coder", "near.rs", serde_json::Value::Null),
                    make_node(
                        "b",
                        "Other",
                        prose_file,
                        serde_json::json!({"description": "code ".repeat(repetitions)}),
                    ),
                ],
                edges: vec![],
                metadata: serde_json::Value::Null,
            })?;
            store
                .conn
                .execute("UPDATE metadata SET search_version=?", [version])?;
            let source_bytes: i64 = store.conn.query_row(
                "SELECT sum(length(CAST(search AS BLOB))) FROM nodes",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(source_bytes <= 8 * 1024 * 1024, repetitions < 840_000);
            let accessed = Arc::new(AtomicBool::new(false));
            let observed = accessed.clone();
            store
                .conn
                .authorizer(Some(move |context: AuthContext<'_>| {
                    if matches!(
                        context.action,
                        AuthAction::Read {
                            table_name: "node_search",
                            ..
                        }
                    ) {
                        observed.store(true, Ordering::Relaxed);
                        if !fast {
                            return Authorization::Deny;
                        }
                    }
                    Authorization::Allow
                }))?;
            assert_eq!(
                store
                    .resolve_endpoint("near.rs::cod", &SearchOptions::default())?
                    .id,
                "a"
            );
            assert_eq!(accessed.load(Ordering::Relaxed), fast);
            assert_eq!(
                store.resolve_endpoint("a", &SearchOptions::default())?.id,
                "a"
            );
            assert_eq!(store.stats()?.nodes, 2);
        }
        Ok(())
    }

    #[test]
    fn endpoint_fts_global_node_cap_includes_every_record() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE nodes(search TEXT NOT NULL);
             WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<50000)
             INSERT INTO nodes SELECT 'x' FROM n;",
        )?;
        assert!(within_endpoint_fts_bounds(&conn)?);
        conn.execute("INSERT INTO nodes VALUES('x')", [])?;
        assert!(!within_endpoint_fts_bounds(&conn)?);
        Ok(())
    }

    #[test]
    fn common_term_search_does_not_invoke_corpus_ranking() -> Result<()> {
        use crate::store::Store;
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

        let dir = tempfile::tempdir()?;
        let mut store = Store::create(&dir.path().join("graph.db"))?;
        store.import_graph(ImportedGraph {
            nodes: (0..1_024)
                .map(|i| Node {
                    id: format!("n{i:04}"),
                    label: format!("Common{i:04}"),
                    kind: "function".into(),
                    file: "file.rs".into(),
                    line: None,
                    end_line: None,
                    qualified_name: None,
                    binding_key: None,
                    metadata: serde_json::Value::Null,
                })
                .collect(),
            edges: vec![],
            metadata: serde_json::Value::Null,
        })?;
        // A result cap cannot catch BM25's hidden posting-list scan. Reject
        // both explicit BM25 and FTS5's implicit rank column at preparation.
        store
            .conn
            .authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Function { function_name }
                    if function_name.eq_ignore_ascii_case("bm25") =>
                {
                    Authorization::Deny
                }
                AuthAction::Read {
                    table_name: "node_search",
                    column_name: "rank",
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))?;
        assert!(store.conn.prepare(
            "SELECT bm25(node_search) FROM node_search WHERE node_search MATCH 'Common*' LIMIT 1"
        ).is_err());
        assert!(
            store
                .conn
                .prepare("SELECT rank FROM node_search WHERE node_search MATCH 'Common*' LIMIT 1")
                .is_err()
        );
        let result = store.query_extended(
            "Common",
            &SearchOptions {
                graph: QueryOptions {
                    depth: 0,
                    limit: 2,
                    ..QueryOptions::default()
                },
                ..SearchOptions::default()
            },
        )?;
        assert_eq!(
            result
                .graph
                .nodes
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>(),
            ["n0000", "n0001"]
        );
        assert!(result.truncation_reasons.iter().any(|s| s == "node_limit"));
        Ok(())
    }

    #[test]
    fn interrupted_query_removes_its_handler() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        let sql = "WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<1000000) SELECT sum(n) FROM numbers";
        let error = budgeted(&conn, || {
            Ok(conn.query_row(sql, [], |row| row.get::<_, i64>(0))?)
        })
        .unwrap_err();
        assert!(error.to_string().contains("work/time budget"));
        // The identical operation succeeds after the request guard drops.
        let sum: i64 = conn.query_row(sql, [], |row| row.get(0))?;
        assert_eq!(sum, 500000500000);
        Ok(())
    }
}

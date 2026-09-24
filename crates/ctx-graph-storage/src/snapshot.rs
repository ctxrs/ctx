//! Full Graf snapshot interchange and explicit cross-project composition.
//!
//! Graf JSON is a serialized `GraphSnapshot`, not a native index backup: files,
//! reference-resolution state and indexing ownership are not recreated. Import
//! retains the source header and metadata as provenance; the destination owns
//! its kind and generation. Composition never guesses cross-project links.
use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::model::{Edge, GraphSnapshot, ImportedGraph, Node, SCHEMA_VERSION};

const NODE_FIELDS: &[&str] = &[
    "id",
    "label",
    "kind",
    "file",
    "line",
    "end_line",
    "qualified_name",
    "binding_key",
    "metadata",
];
const EDGE_FIELDS: &[&str] = &[
    "id",
    "source",
    "target",
    "relation",
    "directed",
    "file",
    "line",
    "confidence",
    "metadata",
];

pub(crate) fn graf_header(value: &Value) -> Result<()> {
    known_fields(
        value,
        &["schema_version", "generation", "kind", "root", "metadata"],
    )?;
    let mut header = value.clone();
    header["nodes"] = json!([]);
    header["edges"] = json!([]);
    let snapshot: GraphSnapshot = serde_json::from_value(header).context("invalid Graf header")?;
    validate_snapshot(&snapshot)
}

pub(crate) fn graf_node(value: &Value) -> Result<Node> {
    known_fields(value, NODE_FIELDS)?;
    serde_json::from_value(value.clone()).context("invalid Graf node")
}

pub(crate) fn graf_edge(value: &Value) -> Result<Edge> {
    known_fields(value, EDGE_FIELDS)?;
    serde_json::from_value(value.clone()).context("invalid Graf edge")
}

/// Read Graf JSON with the same regular-file, 256 MiB and duplicate-key checks
/// as Graphify input. Unknown fields fail instead of being silently discarded.
pub fn read(path: &Path) -> Result<ImportedGraph> {
    let value = crate::import::read_json(path)?;
    known_fields(
        &value,
        &[
            "schema_version",
            "generation",
            "kind",
            "root",
            "nodes",
            "edges",
            "metadata",
        ],
    )?;
    for node in value
        .get("nodes")
        .and_then(Value::as_array)
        .context("nodes must be an array")?
    {
        known_fields(node, NODE_FIELDS)?;
    }
    for edge in value
        .get("edges")
        .and_then(Value::as_array)
        .context("edges must be an array")?
    {
        known_fields(edge, EDGE_FIELDS)?;
    }
    let snapshot: GraphSnapshot = serde_json::from_value(value).context("invalid Graf snapshot")?;
    validate_snapshot(&snapshot)?;
    Ok(ImportedGraph {
        metadata: json!({"graf_snapshot": {
            "schema_version": snapshot.schema_version, "generation": snapshot.generation,
            "kind": snapshot.kind, "root": snapshot.root, "metadata": snapshot.metadata,
        }}),
        nodes: snapshot.nodes,
        edges: snapshot.edges,
    })
}

/// Compose named snapshots without collapsing nodes, multiedges or groups.
/// Names must be nonempty and unique. IDs use a JSON-encoded `(project, id)`
/// tuple, so delimiters, Unicode and IDs resembling namespaces cannot collide.
/// Entity metadata is wrapped, never overwritten; source graph headers and
/// metadata are recorded in `metadata.projects`. Membership endpoints are
/// rewritten like every other edge; original group records remain provenance.
pub fn merge(snapshots: Vec<(String, GraphSnapshot)>) -> Result<ImportedGraph> {
    ensure!(
        !snapshots.is_empty(),
        "merge requires at least one snapshot"
    );
    let mut names = HashSet::new();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut projects = Vec::new();
    for (name, snapshot) in snapshots {
        ensure!(!name.trim().is_empty(), "project name cannot be empty");
        ensure!(names.insert(name.clone()), "duplicate project name: {name}");
        validate_snapshot(&snapshot).with_context(|| format!("project {name}"))?;
        projects.push(json!({
            "name": name, "schema_version": snapshot.schema_version,
            "generation": snapshot.generation, "kind": snapshot.kind,
            "root": snapshot.root, "metadata": snapshot.metadata,
        }));
        for mut node in snapshot.nodes {
            node.metadata = json!({"project": name, "original_id": node.id, "original_metadata": node.metadata});
            node.id = namespaced(&name, &node.id);
            nodes.push(node);
        }
        for mut edge in snapshot.edges {
            edge.metadata = json!({"project": name, "original_id": edge.id, "original_metadata": edge.metadata});
            edge.id = namespaced(&name, &edge.id);
            edge.source = namespaced(&name, &edge.source);
            edge.target = namespaced(&name, &edge.target);
            edges.push(edge);
        }
    }
    Ok(ImportedGraph {
        nodes,
        edges,
        metadata: json!({"projects": projects}),
    })
}

fn namespaced(project: &str, id: &str) -> String {
    // Serializing strings and a tuple cannot fail.
    format!("graf:project:{}", json!([project, id]))
}

fn known_fields(value: &Value, allowed: &[&str]) -> Result<()> {
    let object = value.as_object().context("expected an object")?;
    for key in object.keys() {
        ensure!(
            allowed.contains(&key.as_str()),
            "unknown Graf snapshot field: {key}"
        );
    }
    Ok(())
}

fn validate_snapshot(snapshot: &GraphSnapshot) -> Result<()> {
    ensure!(
        snapshot.schema_version == SCHEMA_VERSION,
        "unsupported Graf snapshot schema version {}; expected {SCHEMA_VERSION}",
        snapshot.schema_version
    );
    ensure!(
        matches!(snapshot.kind.as_str(), "empty" | "native" | "imported"),
        "invalid Graf snapshot kind"
    );
    validate_graph(&snapshot.nodes, &snapshot.edges)
}

pub fn validate_graph(nodes: &[Node], edges: &[Edge]) -> Result<()> {
    let mut ids = HashSet::with_capacity(nodes.len());
    for node in nodes {
        ensure!(!node.id.is_empty(), "node ID cannot be empty");
        ensure!(
            ids.insert(node.id.as_str()),
            "duplicate node ID: {}",
            node.id
        );
    }
    let mut edge_ids = HashSet::with_capacity(edges.len());
    for edge in edges {
        ensure!(!edge.id.is_empty(), "edge ID cannot be empty");
        ensure!(
            edge_ids.insert(edge.id.as_str()),
            "duplicate edge ID: {}",
            edge.id
        );
        ensure!(
            ids.contains(edge.source.as_str()) && ids.contains(edge.target.as_str()),
            "edge {} references an unknown node ID",
            edge.id
        );
    }
    Ok(())
}

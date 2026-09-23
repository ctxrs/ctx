//! Portable offline exports. Snapshot JSON is the canonical lossless format.
//! Visual/report formats show recorded data; they do not infer execution semantics.
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    analysis::{self, AnalysisOptions, AnalysisReport},
    memory::{LearningNode, LearningOverlay},
    model::{GraphSnapshot, Node},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExportFormat {
    SnapshotJson,
    GraphifyJson,
    GraphMl,
    Cypher,
    Mermaid,
    Svg,
    Html,
    Markdown,
    Canvas,
    CallflowHtml,
    TreeHtml,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportOptions {
    pub analysis: AnalysisOptions,
    /// Display labels keyed by BLAKE3 of sorted JSON member IDs, matching the
    /// Graf label file. Stale signatures are ignored; topology is never changed.
    pub community_labels: BTreeMap<String, String>,
    /// Explicit, read-only work-memory annotations for this exact snapshot.
    /// Supported by HTML, Markdown and wiki exports only; never changes topology.
    pub learning: Option<LearningOverlay>,
    /// Maximum detailed/aggregate nodes per interactive page. Topology layout
    /// uses only the bounded drawn graph; search retains the complete snapshot.
    pub node_limit: usize,
    /// Maximum drawn edges per interactive page. Full data stays downloadable.
    /// Neighbor traversal pages retain every recorded relation, showing at most
    /// min(50, node_limit, edge_limit) records at once regardless of view filters.
    pub edge_limit: usize,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            analysis: AnalysisOptions::default(),
            community_labels: BTreeMap::new(),
            learning: None,
            node_limit: 300,
            edge_limit: 1000,
        }
    }
}

fn export_analysis(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<AnalysisReport> {
    let mut report = analysis::analyze(snapshot, &options.analysis)?;
    if options.community_labels.is_empty() {
        return Ok(report);
    }
    for community in &mut report.communities {
        // Analysis already orders member IDs canonically; sort explicitly so
        // signature identity remains independent of presentation order.
        let mut members = community.nodes.clone();
        members.sort();
        let signature = blake3::hash(&serde_json::to_vec(&members)?)
            .to_hex()
            .to_string();
        if let Some(label) = options
            .community_labels
            .get(&signature)
            .filter(|label| !label.trim().is_empty())
        {
            community.label = label.clone();
        }
    }
    for question in &mut report.suggested_questions {
        if question.kind == "low_cohesion"
            && let Some(community) = question
                .community_ids
                .first()
                .and_then(|id| report.communities.get(*id))
        {
            question.question = analysis::cohesion_question(community.id, &community.label);
        }
    }
    Ok(report)
}

/// Render one complete artifact in memory without filesystem or network effects.
pub fn render(snapshot: &GraphSnapshot, format: ExportFormat) -> Result<String> {
    render_with_options(snapshot, format, &ExportOptions::default())
}

pub fn render_with_options(
    snapshot: &GraphSnapshot,
    format: ExportFormat,
    options: &ExportOptions,
) -> Result<String> {
    analysis::validate(snapshot)?;
    ensure!(
        options.learning.is_none() || matches!(format, ExportFormat::Html | ExportFormat::Markdown),
        "learning annotations are supported only by HTML, Markdown and wiki exports"
    );
    validate_learning(snapshot, options)?;
    ensure!(
        options.node_limit > 0 && options.edge_limit > 0,
        "viewer node and edge limits must be positive"
    );
    match format {
        ExportFormat::SnapshotJson => Ok(serde_json::to_string_pretty(snapshot)? + "\n"),
        ExportFormat::GraphifyJson => graphify(snapshot, options),
        ExportFormat::GraphMl => graphml(snapshot, options),
        ExportFormat::Cypher => cypher(snapshot),
        ExportFormat::Mermaid => Ok(mermaid(snapshot)),
        ExportFormat::Svg => svg(snapshot, options),
        ExportFormat::Html => html(snapshot, options),
        ExportFormat::Markdown => markdown(snapshot, options),
        ExportFormat::Canvas => canvas(snapshot, None, options),
        ExportFormat::CallflowHtml => callflow_html(snapshot, options),
        ExportFormat::TreeHtml => tree_html(snapshot),
    }
}

fn validate_learning(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<()> {
    if let Some(learning) = &options.learning {
        ensure!(
            learning.schema_version == 1,
            "unsupported learning overlay schema"
        );
        let mut hasher = blake3::Hasher::new();
        serde_json::to_writer(&mut hasher, snapshot)?;
        let hash = hasher.finalize().to_hex().to_string();
        ensure!(
            learning.snapshot_hash.as_deref() == Some(hash.as_str()),
            "learning overlay does not match this exact snapshot; regenerate it"
        );
        ensure!(
            learning.nodes.values().all(|node| node.score.is_finite()),
            "learning scores must be finite"
        );
    }
    Ok(())
}

/// Display membership comes only from actual incidence edges. Ordinary group
/// relations and every original node/edge remain in the snapshot and analysis.
fn display_groups(snapshot: &GraphSnapshot) -> Vec<Value> {
    let mut groups: BTreeMap<_, (BTreeSet<&str>, Vec<&str>)> = snapshot
        .nodes
        .iter()
        .filter(|node| matches!(node.kind.as_str(), "group" | "hyperedge"))
        .map(|node| (node.id.as_str(), (BTreeSet::new(), Vec::new())))
        .collect();
    for edge in &snapshot.edges {
        if edge.relation != "member_of" || edge.source == edge.target {
            continue;
        }
        if let Some((members, incidences)) = groups.get_mut(edge.target.as_str()) {
            members.insert(&edge.source);
            incidences.push(&edge.id);
        }
        if !edge.directed
            && let Some((members, incidences)) = groups.get_mut(edge.source.as_str())
        {
            members.insert(&edge.target);
            incidences.push(&edge.id);
        }
    }
    groups.into_iter().map(|(id, (members, incidences))| {
        json!({"id": id, "members": members, "incidences": incidences})
    }).collect()
}

fn location(node: &Node) -> String {
    match (node.line, node.end_line) {
        (Some(a), Some(b)) => format!("L{a}-L{b}"),
        (Some(a), _) => format!("L{a}"),
        _ => String::new(),
    }
}

fn graphify(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
    let report = export_analysis(snapshot, options)?;
    let communities: BTreeMap<_, _> = report
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.community))
        .collect();
    let community_labels: BTreeMap<_, _> = report
        .communities
        .iter()
        .map(|c| (c.id, c.label.as_str()))
        .collect();
    let (group_nodes, group_edges, groups) = active_groups(snapshot);
    let nodes: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|n| !group_nodes.contains(n.id.as_str()))
        .map(|n| {
            let mut object = analysis::attributes(&n.metadata)
                .as_object()
                .cloned()
                .unwrap_or_default();
            for alias in ["name", "source", "path"] {
                object.remove(alias);
            }
            object.insert("id".into(), json!(n.id));
            object.insert("label".into(), json!(n.label));
            object.insert("file_type".into(), json!(n.kind));
            object.insert("source_file".into(), json!(n.file));
            object.insert("source_location".into(), json!(location(n)));
            object.remove("qualified_name");
            if let Some(name) = &n.qualified_name {
                object.insert("qualified_name".into(), json!(name));
            }
            object
                .entry("community")
                .or_insert_with(|| json!(communities[n.id.as_str()]));
            if object.get("community").and_then(Value::as_u64)
                == Some(communities[n.id.as_str()] as u64)
            {
                object
                    .entry("community_name")
                    .or_insert_with(|| json!(community_labels[&communities[n.id.as_str()]]));
            }
            object.insert("_graf".into(), json!(n));
            Value::Object(object)
        })
        .collect();
    let links: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|e| !group_edges.contains(e.id.as_str()))
        .map(|e| {
            let mut object = analysis::attributes(&e.metadata)
                .as_object()
                .cloned()
                .unwrap_or_default();
            for alias in ["from", "to", "type", "_src", "_tgt"] {
                object.remove(alias);
            }
            for (key, value) in [
                ("source", json!(e.source)),
                ("target", json!(e.target)),
                ("id", json!(e.id)),
                ("key", json!(e.id)),
                ("relation", json!(e.relation)),
                ("directed", json!(e.directed)),
                ("confidence", json!(e.confidence)),
            ] {
                object.insert(key.into(), value);
            }
            // Strict readers require text if source_file is present, rather than null.
            object.remove("source_file");
            if let Some(file) = &e.file {
                object.insert("source_file".into(), json!(file));
            }
            object.insert(
                "source_location".into(),
                json!(e.line.map(|line| format!("L{line}"))),
            );
            if e.directed {
                object.insert("_src".into(), json!(e.source));
                object.insert("_tgt".into(), json!(e.target));
            }
            object.insert("_graf".into(), json!(e));
            Value::Object(object)
        })
        .collect();
    // NetworkX has one graph-wide direction. Mixed direction is retained explicitly
    // by each edge's flag and the same _src/_tgt convention Graphify consumes.
    let mut graph = snapshot
        .metadata
        .get("graph")
        .unwrap_or(&snapshot.metadata)
        .as_object()
        .cloned()
        .unwrap_or_default();
    graph.remove("groups");
    graph.remove("hyperedges");
    let value = json!({"directed":false,"multigraph":true,"graph":graph,"nodes":nodes,"links":links,"hyperedges":groups,"_graf":header(snapshot)});
    Ok(serde_json::to_string_pretty(&value)? + "\n")
}

/// Reconstruct only recognizable, exclusively generated group incidence records.
/// If a group acquired ordinary relations, keep its node/edges representation so
/// those relations never lose their endpoints or become duplicate memberships.
fn active_groups(snapshot: &GraphSnapshot) -> (BTreeSet<&str>, BTreeSet<&str>, Vec<Value>) {
    let candidates: BTreeSet<_> = snapshot
        .nodes
        .iter()
        .filter(|n| {
            n.kind == "group"
                && analysis::attributes(&n.metadata)
                    .get("graphify_group")
                    .is_some_and(Value::is_object)
        })
        .map(|n| n.id.as_str())
        .collect();
    let mut incidents = BTreeMap::<&str, Vec<&crate::model::Edge>>::new();
    for edge in &snapshot.edges {
        incidents.entry(&edge.source).or_default().push(edge);
        if edge.source != edge.target {
            incidents.entry(&edge.target).or_default().push(edge);
        }
    }
    let mut removed_nodes = BTreeSet::new();
    let mut removed_edges = BTreeSet::new();
    let mut groups = Vec::new();
    let mut used_ids = BTreeSet::new();
    for node in &snapshot.nodes {
        if !candidates.contains(node.id.as_str()) {
            continue;
        }
        let edges = incidents
            .get(node.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if edges.is_empty()
            || !edges.iter().all(|e| {
                let metadata = analysis::attributes(&e.metadata);
                e.target == node.id
                    && !candidates.contains(e.source.as_str())
                    && e.relation == "member_of"
                    && !e.directed
                    && metadata
                        .get("graphify_group_ordinal")
                        .is_some_and(Value::is_u64)
                    && metadata.get("member_index").is_some_and(Value::is_u64)
            })
        {
            continue;
        }
        let mut edges = edges.to_vec();
        edges.sort_by(|a, b| {
            analysis::attributes(&a.metadata)["member_index"]
                .as_u64()
                .cmp(&analysis::attributes(&b.metadata)["member_index"].as_u64())
                .then(a.id.cmp(&b.id))
        });
        let original = &analysis::attributes(&node.metadata)["graphify_group"];
        let mut record = original.as_object().cloned().unwrap_or_default();
        let members: Vec<_> = edges.iter().map(|e| json!(e.source)).collect();
        record.remove("members");
        record.remove("node_ids");
        record.insert("nodes".into(), json!(members));
        // Independent projects may reuse group IDs. Current materialized node IDs
        // are collision-free, while the original complete record stays provenance.
        let preferred = if node.metadata.get("project").is_some() {
            json!(node.id)
        } else {
            record.get("id").cloned().unwrap_or_else(|| json!(node.id))
        };
        let mut id = preferred.clone();
        while !used_ids.insert(id.to_string()) {
            id = json!(format!("graf-group:{}:{}", groups.len(), id));
        }
        record.insert("id".into(), id);
        record.insert(
            "_graf_group".into(),
            json!({"node":node,"incidences":edges}),
        );
        groups.push(Value::Object(record));
        removed_nodes.insert(node.id.as_str());
        removed_edges.extend(edges.iter().map(|e| e.id.as_str()));
    }
    (removed_nodes, removed_edges, groups)
}

fn header(snapshot: &GraphSnapshot) -> Value {
    json!({"schema_version":snapshot.schema_version,"generation":snapshot.generation,"kind":snapshot.kind,"root":snapshot.root,"metadata":snapshot.metadata})
}

/// XML 1.0 cannot represent some control characters. Return an error instead of
/// silently corrupting the source; JSON formats can carry these characters.
fn xml(text: &str) -> Result<String> {
    ensure!(text.chars().all(|c| matches!(c, '\t'|'\n'|'\r'|'\u{20}'..='\u{d7ff}'|'\u{e000}'..='\u{fffd}'|'\u{10000}'..='\u{10ffff}')), "XML cannot represent a control character; use snapshot JSON for lossless export");
    Ok(text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn graphml(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
    let report = export_analysis(snapshot, options)?;
    let communities: BTreeMap<_, _> = report
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.community))
        .collect();
    let mut output = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\">\n",
    );
    for (key, scope, kind) in [
        ("label", "node", "string"),
        ("kind", "node", "string"),
        ("source_file", "node", "string"),
        ("community", "node", "int"),
        ("relation", "edge", "string"),
        ("confidence", "edge", "string"),
        ("graf_json", "all", "string"),
    ] {
        writeln!(
            output,
            "<key id=\"{key}\" for=\"{scope}\" attr.name=\"{key}\" attr.type=\"{kind}\"/>"
        )?;
    }
    output.push_str("<graph id=\"g\" edgedefault=\"directed\">\n");
    writeln!(
        output,
        "<data key=\"graf_json\">{}</data>",
        xml(&serde_json::to_string(&header(snapshot))?)?
    )?;
    let indices: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    for (i, node) in snapshot.nodes.iter().enumerate() {
        writeln!(output, "<node id=\"n{i}\">")?;
        for (key, value) in [
            ("label", node.label.clone()),
            ("kind", node.kind.clone()),
            ("source_file", node.file.clone()),
            ("community", communities[node.id.as_str()].to_string()),
            ("graf_json", serde_json::to_string(node)?),
        ] {
            writeln!(output, "<data key=\"{key}\">{}</data>", xml(&value)?)?;
        }
        output.push_str("</node>\n");
    }
    for (i, edge) in snapshot.edges.iter().enumerate() {
        writeln!(
            output,
            "<edge id=\"e{i}\" source=\"n{}\" target=\"n{}\" directed=\"{}\">",
            indices[edge.source.as_str()],
            indices[edge.target.as_str()],
            edge.directed
        )?;
        for (key, value) in [
            ("relation", edge.relation.clone()),
            ("confidence", edge.confidence.clone()),
            ("graf_json", serde_json::to_string(edge)?),
        ] {
            writeln!(output, "<data key=\"{key}\">{}</data>", xml(&value)?)?;
        }
        output.push_str("</edge>\n");
    }
    output.push_str("</graph>\n</graphml>\n");
    Ok(output)
}

fn cypher_string(text: &str) -> String {
    let mut out = String::from("'");
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Return complete, ordered Cypher statements without trailing semicolons.
/// Semicolons inside user strings remain data; callers must not split statements.
/// This only generates text and never connects to a database.
pub fn cypher_statements(snapshot: &GraphSnapshot) -> Result<Vec<String>> {
    analysis::validate(snapshot)?;
    // Scope MERGE to the artifact, preventing collisions with unrelated imported
    // snapshots. Property JSON retains nested metadata that Cypher cannot store.
    let scope = blake3::hash(&serde_json::to_vec(snapshot)?)
        .to_hex()
        .to_string();
    let scope = cypher_string(&scope);
    let mut statements = vec![format!(
        "MERGE (g:GrafSnapshot {{scope:{scope}}}) SET g.graf_json={}",
        cypher_string(&serde_json::to_string(&header(snapshot))?)
    )];
    for node in &snapshot.nodes {
        statements.push(format!(
            "MERGE (n:GrafNode {{scope:{scope}, id:{}}}) SET n.label={}, n.kind={}, n.source_file={}, n.graf_json={}",
            cypher_string(&node.id),
            cypher_string(&node.label),
            cypher_string(&node.kind),
            cypher_string(&node.file),
            cypher_string(&serde_json::to_string(node)?)
        ));
    }
    for edge in &snapshot.edges {
        statements.push(format!(
            "MATCH (a:GrafNode {{scope:{scope}, id:{}}}), (b:GrafNode {{scope:{scope}, id:{}}}) MERGE (a)-[r:GRAF_EDGE {{id:{}}}]->(b) SET r.relation={}, r.directed={}, r.graf_json={}",
            cypher_string(&edge.source),
            cypher_string(&edge.target),
            cypher_string(&edge.id),
            cypher_string(&edge.relation),
            edge.directed,
            cypher_string(&serde_json::to_string(edge)?)
        ));
    }
    Ok(statements)
}

fn cypher(snapshot: &GraphSnapshot) -> Result<String> {
    let mut output = String::from(
        "// Offline Neo4j / FalkorDB Cypher. Execute statements in order.\n// Undirected edges use one stored arrow with directed=false; query them without direction.\n",
    );
    for statement in cypher_statements(snapshot)? {
        writeln!(output, "{statement};")?;
    }
    Ok(output)
}

fn mermaid_text(text: &str) -> String {
    // Decimal entities are Mermaid's own label-escaping syntax. Encode all
    // punctuation, including directive delimiters, HTML and quotes.
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c.to_string()
            } else {
                format!("#{};", c as u32)
            }
        })
        .collect()
}

fn mermaid(snapshot: &GraphSnapshot) -> String {
    let indices: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    let mut output = String::from("flowchart LR\n");
    for (i, node) in snapshot.nodes.iter().enumerate() {
        let _ = writeln!(output, "  n{i}[\"{}\"]", mermaid_text(&node.label));
    }
    for edge in &snapshot.edges {
        let _ = writeln!(
            output,
            "  n{} {}|\"{}\"| n{}",
            indices[edge.source.as_str()],
            if edge.directed { "-->" } else { "---" },
            mermaid_text(&edge.relation),
            indices[edge.target.as_str()]
        );
    }
    output
}

fn positions(snapshot: &GraphSnapshot) -> (Vec<(f64, f64)>, f64) {
    let size = (snapshot.nodes.len() as f64 * 22.0).max(640.0);
    let radius = size / 2.0 - 100.0;
    let points = (0..snapshot.nodes.len())
        .map(|i| {
            let angle = i as f64 * std::f64::consts::TAU / snapshot.nodes.len() as f64;
            (
                size / 2.0 + radius * angle.cos(),
                size / 2.0 + radius * angle.sin(),
            )
        })
        .collect();
    (points, size)
}

fn svg(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
    let report = export_analysis(snapshot, options)?;
    let metrics: BTreeMap<_, _> = report.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let (points, size) = positions(snapshot);
    let indices: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    let mut output = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size} {size}\" role=\"img\" aria-label=\"Graf graph\">\n<title>Graf graph</title><desc>Recorded relations; arrows show direction. Full records are in metadata.</desc>\n<metadata>{}</metadata>\n<defs><marker id=\"arrow\" viewBox=\"0 0 10 10\" refX=\"10\" refY=\"5\" markerWidth=\"6\" markerHeight=\"6\" orient=\"auto-start-reverse\"><path d=\"M0 0 L10 5 L0 10 z\" fill=\"#64748b\"/></marker></defs>\n",
        xml(&serde_json::to_string(snapshot)?)?
    );
    let mut pairs = BTreeMap::<(usize, usize), usize>::new();
    for edge in &snapshot.edges {
        let a = indices[edge.source.as_str()];
        let b = indices[edge.target.as_str()];
        let (x, y) = points[a];
        let (u, v) = points[b];
        let key = (a.min(b), a.max(b));
        let ordinal = pairs.entry(key).or_default();
        *ordinal += 1;
        let bend = 22.0 * *ordinal as f64;
        let d = if a == b {
            format!(
                "M{x} {y} C{} {} {} {} {} {}",
                x + bend,
                y - bend * 2.0,
                x - bend,
                y - bend * 2.0,
                x - 2.0,
                y - 10.0
            )
        } else {
            let len = ((u - x).powi(2) + (v - y).powi(2)).sqrt();
            let sign = if a < b { 1.0 } else { -1.0 };
            format!(
                "M{x} {y} Q{} {} {} {}",
                (x + u) / 2.0 - (v - y) / len * bend * sign,
                (y + v) / 2.0 + (u - x) / len * bend * sign,
                u - (u - x) / len * 12.0,
                v - (v - y) / len * 12.0
            )
        };
        writeln!(
            output,
            "<path d=\"{d}\" fill=\"none\" stroke=\"#64748b\"{}><title>{}</title></path>",
            if edge.directed {
                " marker-end=\"url(#arrow)\""
            } else {
                ""
            },
            xml(&format!(
                "{}: {} — {} [{}]",
                edge.id, edge.source, edge.target, edge.relation
            ))?
        )?;
    }
    for (node, (x, y)) in snapshot.nodes.iter().zip(points) {
        writeln!(
            output,
            "<g><title>{}</title><circle cx=\"{x}\" cy=\"{y}\" r=\"10\" fill=\"hsl({} 60% 45%)\"/><text x=\"{}\" y=\"{}\" font-family=\"sans-serif\" font-size=\"12\">{}</text></g>",
            xml(&format!("{} {}", node.id, node.file))?,
            metrics[node.id.as_str()].community as f64 * 137.508 % 360.0,
            x + 14.0,
            y + 4.0,
            xml(&node.label)?
        )?;
    }
    output.push_str("</svg>\n");
    Ok(output)
}

fn html(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
    let report = export_analysis(snapshot, options)?;
    let data = serde_json::to_string(&json!({"snapshot":snapshot,"analysis":report,"groups":display_groups(snapshot),"learning":options.learning,"viewer":{"node_limit":options.node_limit,"edge_limit":options.edge_limit}}))?
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    Ok(include_str!("assets/viewer.html").replace("__GRAF_DATA__", &data))
}

/// Encode Markdown metacharacters as character references, keeping Unicode text.
/// Source paths are text, never interpolated as links or Obsidian wikilinks.
fn md(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_punctuation() || c.is_control() || c == '\u{2028}' || c == '\u{2029}' {
                format!("&#{};", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn markdown(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
    let report = export_analysis(snapshot, options)?;
    report_markdown(snapshot, &report, options.learning.as_ref())
}

fn learning_node_markdown(node: &LearningNode) -> String {
    format!(
        "Status: {} · score {:.9} · useful {} · negative {} · verified useful {} · unverified {}\n\n{}\n",
        md(&node.status),
        node.score,
        node.useful,
        node.negative,
        node.verified_useful,
        node.unverified,
        md(&node.reason)
    )
}

fn learning_markdown(snapshot: &GraphSnapshot, learning: &LearningOverlay) -> Result<String> {
    let mut out = format!(
        "\n## Work-memory lessons\n\nExplicit observations generated at Unix time {} for this exact snapshot. Cited-source verification does not certify other files or answer correctness.\n\n",
        learning.generated_unix_secs
    );
    for node in &snapshot.nodes {
        if let Some(annotation) = learning.nodes.get(&node.id) {
            writeln!(
                out,
                "### {} ({})\n\n{}",
                md(&node.label),
                md(&node.id),
                learning_node_markdown(annotation)
            )?;
        }
    }
    out.push_str("\n### Recorded lessons and event provenance\n\n");
    // Preserve the shared lesson categories, but render all other content as
    // indented code. Exact IDs stay readable without activating HTML or links.
    // Normalize standalone CR too, so every logical line receives indentation.
    let lessons = learning.lessons.replace("\r\n", "\n").replace('\r', "\n");
    for line in lessons.lines() {
        if matches!(
            line,
            "## preferred"
                | "## tentative"
                | "## contested"
                | "## dead_end"
                | "## corrected"
                | "## Source verification"
                | "## Event provenance"
        ) {
            writeln!(out, "\n{line}\n")?;
        } else {
            writeln!(out, "    {line}")?;
        }
    }
    Ok(out)
}

fn report_markdown(
    snapshot: &GraphSnapshot,
    report: &AnalysisReport,
    learning: Option<&LearningOverlay>,
) -> Result<String> {
    let nodes: BTreeMap<_, _> = snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut output = format!(
        "# Graf graph report\n\nGeneration {} · {} nodes · {} edges\n\n{}\n\nPageRank converged: {} ({} iterations). Community convergence: {} ({} passes). Modularity: {:.6}.\n\n",
        snapshot.generation,
        snapshot.nodes.len(),
        snapshot.edges.len(),
        report.methodology,
        report.pagerank_converged,
        report.pagerank_iterations,
        report.community_converged,
        report.community_passes,
        report.community_modularity
    );
    writeln!(
        output,
        "Community resolution: {}. Optional repartition attempts: {}. Communities still outside requested size/cohesion thresholds: {}. Thresholds trigger source-topology repartitioning, not guaranteed caps.\n",
        report.community_resolution,
        report.community_split_attempts,
        if report.unsatisfied_community_constraints.is_empty() {
            "none".into()
        } else {
            report
                .unsatisfied_community_constraints
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        }
    )?;
    output.push_str("## Hubs and ranking\n\n| Node | Degree | PageRank | Community | Source |\n| --- | ---: | ---: | ---: | --- |\n");
    let metrics: BTreeMap<_, _> = report.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for id in &report.hubs {
        let node = nodes[id.as_str()];
        let metric = metrics[id.as_str()];
        writeln!(
            output,
            "| {} ({}) | {} | {:.8} | {} | {} {} |",
            md(&node.label),
            md(id),
            metric.degree,
            metric.pagerank,
            metric.community,
            md(&node.file),
            md(&location(node))
        )?;
    }
    writeln!(
        output,
        "\nHub ranking omits {} above-percentile nodes and {} noise candidates (sets may overlap); complete node metrics remain in analysis output. Labels quote eligible member names.\n",
        report.excluded_hubs.len(),
        report.noise_filtered_hubs.len()
    )?;
    output.push_str("\n## Structural communities\n\n");
    for community in &report.communities {
        writeln!(
            output,
            "- Community {} — {}: {}",
            community.id,
            md(&community.label),
            community
                .nodes
                .iter()
                .map(|id| md(&nodes[id.as_str()].label))
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    output.push_str("\n## Confidence audit\n\n");
    for (confidence, count) in &report.confidence_counts {
        writeln!(output, "- {}: {} edges", md(confidence), count)?;
    }
    output.push_str("\n## Gaps and uncertainty\n\nAbsence of an edge is not proof of missing functionality. These are structural observations.\n\n");
    for id in &report.isolates {
        writeln!(
            output,
            "- Isolated node: {} ({})",
            md(&nodes[id.as_str()].label),
            md(id)
        )?;
    }
    for community in &report.communities {
        writeln!(
            output,
            "- Community {}: {} nodes, pair cohesion {:.3}",
            community.id,
            community.nodes.len(),
            community.cohesion
        )?;
    }
    for edge in &snapshot.edges {
        if edge.confidence != "EXTRACTED" {
            writeln!(
                output,
                "- {} edge {}: {} → {}",
                md(&edge.confidence),
                md(&edge.id),
                md(&edge.source),
                md(&edge.target)
            )?;
        }
    }
    writeln!(
        output,
        "\n## Ranked structural connections\n\nShowing {} of {} eligible connections (limit {}). Scores are documented structural signals, not proof of architectural significance.\n",
        report.surprises.len(),
        report.surprise_candidates,
        analysis::SURPRISE_LIMIT
    )?;
    for surprise in &report.surprises {
        let edge = &surprise.edge;
        writeln!(
            output,
            "- Score {}: {} {} {} — {} (edge {}; confidence {}; source evidence: {} {}).",
            surprise.score,
            md(&nodes[edge.source.as_str()].label),
            if edge.directed { "→" } else { "↔" },
            md(&nodes[edge.target.as_str()].label),
            md(&edge.relation),
            md(&edge.id),
            md(&edge.confidence),
            md(edge.file.as_deref().unwrap_or("unavailable")),
            edge.line.map(|n| format!("L{n}")).unwrap_or_default()
        )?;
        writeln!(
            output,
            "  Endpoint sources: {} / {}. Signals: {}.",
            md(&surprise.source_file),
            md(&surprise.target_file),
            surprise
                .signals
                .iter()
                .map(|signal| format!(
                    "{} +{} ({})",
                    md(&signal.code),
                    signal.points,
                    md(&signal.detail)
                ))
                .collect::<Vec<_>>()
                .join("; ")
        )?;
    }
    writeln!(
        output,
        "\n## Suggested questions\n\nShowing {} of {} template candidates (limit {}), rotating across signal types. Questions ask for verification; they do not assert missing functionality or recommend architecture changes.\n",
        report.suggested_questions.len(),
        report.suggested_question_candidates,
        analysis::QUESTION_LIMIT
    )?;
    for question in &report.suggested_questions {
        writeln!(
            output,
            "- [{}] {} {}",
            md(&question.kind),
            md(&question.question),
            md(&question.why)
        )?;
        writeln!(
            output,
            "  Supporting records: {}; shown nodes: {}; communities: {}.",
            question.evidence_count,
            question
                .node_ids
                .iter()
                .map(|id| format!(
                    "{} ({} {})",
                    md(id),
                    md(&nodes[id.as_str()].file),
                    md(&location(nodes[id.as_str()]))
                ))
                .collect::<Vec<_>>()
                .join(", "),
            question
                .community_ids
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )?;
        for edge in &question.edge_evidence {
            writeln!(
                output,
                "  Edge {}: {} {} {} — {} {} [{}].",
                md(&edge.id),
                md(&edge.source),
                if edge.directed { "→" } else { "↔" },
                md(&edge.target),
                md(edge.file.as_deref().unwrap_or("unavailable")),
                edge.line.map(|n| format!("L{n}")).unwrap_or_default(),
                md(&edge.confidence)
            )?;
        }
    }
    output.push_str("\n## Cross-community connections\n\nThese edges cross structural partitions; no semantic surprise is inferred.\n\n");
    for edge in &report.cross_community_edges {
        writeln!(
            output,
            "- {} {} {} — {} (edge {})",
            md(&edge.source),
            if edge.directed { "→" } else { "↔" },
            md(&edge.target),
            md(&edge.relation),
            md(&edge.id)
        )?;
    }
    output.push_str("\n## Import cycles\n\nStrongly connected file sets under recorded directed import relations; member order is alphabetical, not an execution sequence.\n\n");
    if report.import_cycles.is_empty() {
        output.push_str("No recorded import cycles.\n");
    }
    for cycle in &report.import_cycles {
        writeln!(
            output,
            "- {}",
            cycle
                .iter()
                .map(|file| md(file))
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    output.push_str("\n## Architecture: recorded cross-file relations\n\nThese groups describe graph structure, not inferred responsibilities.\n\n");
    for dependency in &report.file_dependencies {
        writeln!(
            output,
            "- {} {} {} — {} ({} records)",
            md(&dependency.source_file),
            if dependency.directed { "→" } else { "↔" },
            md(&dependency.target_file),
            md(&dependency.relation),
            dependency.evidence.len()
        )?;
    }
    output.push_str("\n## Callflow evidence\n\nOnly recorded calls are listed; order, reachability at runtime and architectural meaning are not inferred.\n\n");
    for edge in &report.call_edges {
        writeln!(
            output,
            "- {} {} {} — {} {} (edge {}; confidence {})",
            md(&nodes[edge.source.as_str()].label),
            if edge.directed { "→" } else { "↔" },
            md(&nodes[edge.target.as_str()].label),
            md(edge.file.as_deref().unwrap_or("source unavailable")),
            edge.line.map(|n| format!("L{n}")).unwrap_or_default(),
            md(&edge.id),
            md(&edge.confidence)
        )?;
    }
    output.push_str("\n## All relation evidence\n\n| Edge | Source ID | Relation | Target ID | Direction | Location | Confidence |\n| --- | --- | --- | --- | --- | --- | --- |\n");
    for edge in &snapshot.edges {
        writeln!(
            output,
            "| {} | {} | {} | {} | {} | {} {} | {} |",
            md(&edge.id),
            md(&edge.source),
            md(&edge.relation),
            md(&edge.target),
            if edge.directed {
                "directed"
            } else {
                "undirected"
            },
            md(edge.file.as_deref().unwrap_or("unavailable")),
            edge.line.map(|n| format!("L{n}")).unwrap_or_default(),
            md(&edge.confidence)
        )?;
    }
    if let Some(learning) = learning {
        output.push_str(&learning_markdown(snapshot, learning)?);
    }
    Ok(output)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultReport {
    pub directory: PathBuf,
    pub files: usize,
}

/// Write a new generated folder inside an existing directory. Never update or
/// replace a user's note/config, even when rerun or when labels collide.
pub fn write_vault(snapshot: &GraphSnapshot, vault: &Path) -> Result<VaultReport> {
    write_vault_with_options(snapshot, vault, &ExportOptions::default())
}

pub fn write_vault_with_options(
    snapshot: &GraphSnapshot,
    vault: &Path,
    options: &ExportOptions,
) -> Result<VaultReport> {
    analysis::validate(snapshot)?;
    validate_learning(snapshot, options)?;
    ensure!(
        vault.is_dir(),
        "vault destination must be an existing directory"
    );
    let report = export_analysis(snapshot, options)?;
    let directory = tempfile::Builder::new()
        .prefix("graf-export-")
        .tempdir_in(vault)?;
    let folder = directory
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let community_tags: BTreeMap<_, _> = report
        .communities
        .iter()
        .map(|c| (c.id, format!("graf/{folder}/community-{}", c.id)))
        .collect();
    let node_communities: BTreeMap<_, _> = report
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.community))
        .collect();
    let mut files = 0;
    let mut write = |name: &str, content: &str| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.path().join(name))?;
        file.write_all(content.as_bytes())?;
        files += 1;
        Ok(())
    };
    write(
        "report.md",
        &report_markdown(snapshot, &report, options.learning.as_ref())?,
    )?;
    write("snapshot.json", &serde_json::to_string_pretty(snapshot)?)?;
    let filenames: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), format!("node-{i}.md")))
        .collect();
    let mut index = String::from(
        "# Graf wiki\n\n[Graph report](report.md) · [Lossless snapshot](snapshot.json)\n\n",
    );
    let mut incidents = BTreeMap::<&str, Vec<&crate::model::Edge>>::new();
    for edge in &snapshot.edges {
        incidents.entry(&edge.source).or_default().push(edge);
        if edge.target != edge.source {
            incidents.entry(&edge.target).or_default().push(edge);
        }
    }
    for node in &snapshot.nodes {
        let filename = &filenames[node.id.as_str()];
        writeln!(index, "- [{}]({filename})", md(&node.label))?;
        let mut note = format!(
            "---\ntags:\n  - {}\n---\n\n# {}\n\nID: {}\n\nKind: {}\n\nSource: {} {}\n\n## Relations\n\n",
            community_tags[&node_communities[node.id.as_str()]],
            md(&node.label),
            md(&node.id),
            md(&node.kind),
            md(&node.file),
            md(&location(node))
        );
        for edge in incidents.get(node.id.as_str()).into_iter().flatten() {
            let peer = if edge.source == node.id {
                &edge.target
            } else {
                &edge.source
            };
            writeln!(
                note,
                "- [{}]({}) — {} ({}, {}; evidence: {} {})",
                md(peer),
                filenames[peer.as_str()],
                md(&edge.relation),
                if edge.directed {
                    if edge.source == node.id {
                        "outgoing"
                    } else {
                        "incoming"
                    }
                } else {
                    "undirected"
                },
                md(&edge.id),
                md(edge.file.as_deref().unwrap_or("unavailable")),
                edge.line.map(|n| format!("L{n}")).unwrap_or_default()
            )?;
        }
        if let Some(annotation) = options
            .learning
            .as_ref()
            .and_then(|learning| learning.nodes.get(&node.id))
        {
            writeln!(
                note,
                "\n## Work-memory observation\n\n{}",
                learning_node_markdown(annotation)
            )?;
        }
        // Entity encoding prevents fence-breaking content, HTML and plugin directives.
        writeln!(note, "\n## Record\n\n{}", md(&serde_json::to_string(node)?))?;
        write(filename, &note)?;
    }
    index.push_str("\n## Communities\n\n");
    let node_map: BTreeMap<_, _> = snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for community in &report.communities {
        let filename = format!("community-{}.md", community.id);
        writeln!(
            index,
            "- [Community {} — {}]({filename})",
            community.id,
            md(&community.label)
        )?;
        let mut note = format!(
            "---\ntags:\n  - {}\n---\n\n# Community {} — {}\n\n{} nodes · pair cohesion {:.3}. Membership describes connectivity, not inferred responsibility.\n\n",
            community_tags[&community.id],
            community.id,
            md(&community.label),
            community.nodes.len(),
            community.cohesion
        );
        for id in &community.nodes {
            writeln!(
                note,
                "- [{}]({}) — {} {}",
                md(&node_map[id.as_str()].label),
                filenames[id.as_str()],
                md(&node_map[id.as_str()].file),
                md(&location(node_map[id.as_str()]))
            )?;
        }
        write(&filename, &note)?;
    }
    // Obsidian Canvas file references are vault-root-relative, not relative to
    // the .canvas file. Only generated names enter these links.
    let canvas_files = filenames
        .iter()
        .map(|(&id, name)| (id, format!("{folder}/{name}")))
        .collect();
    write(
        "graph.canvas",
        &canvas(snapshot, Some(&canvas_files), options)?,
    )?;
    // Configuration is an opt-in snippet in this fresh output folder. Queries
    // use generated ASCII tags shared by the actual node/community notes.
    let colors = [0x2563eb, 0x16a34a, 0xd97706, 0xdc2626, 0x9333ea, 0x0891b2];
    let color_groups: Vec<_> = report
        .communities
        .iter()
        .map(|c| {
            json!({
                "query":format!("tag:#{}",community_tags[&c.id]),
                "color":{"a":1,"rgb":colors[c.id % colors.len()]}
            })
        })
        .collect();
    write(
        "graph-colors.json",
        &serde_json::to_string_pretty(&json!({"colorGroups":color_groups}))?,
    )?;
    let mut color_instructions = String::from(
        "# Community graph colors\n\nThis folder's notes have generated community tags. In Obsidian's Graph view, add the queries below under Groups and select their colors.\n\nAlternatively, close Obsidian and manually merge the `colorGroups` entries from [graph-colors.json](graph-colors.json) into your vault's `.obsidian/graph.json`, preserving existing groups and settings. This export does not change that configuration. Queries match only this export folder's tags; labels and source paths are never used as search syntax.\n\n| Community | Query | Color |\n| --- | --- | --- |\n",
    );
    for community in &report.communities {
        writeln!(
            color_instructions,
            "| {} — {} | `tag:#{}` | `#{:06x}` |",
            community.id,
            md(&community.label),
            community_tags[&community.id],
            colors[community.id % colors.len()]
        )?;
    }
    write("graph-colors.md", &color_instructions)?;
    index.push_str(
        "\n[Open graph canvas](graph.canvas) · [Set community graph colors](graph-colors.md)\n",
    );
    write("index.md", &index)?;
    Ok(VaultReport {
        directory: directory.keep(),
        files,
    })
}

fn canvas(
    snapshot: &GraphSnapshot,
    filenames: Option<&BTreeMap<&str, String>>,
    options: &ExportOptions,
) -> Result<String> {
    let report = export_analysis(snapshot, options)?;
    let nodes: BTreeMap<_, _> = snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let indices: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), format!("n{i}")))
        .collect();
    let mut cards = Vec::new();
    let mut groups = Vec::new();
    let mut y = 0usize;
    for community in &report.communities {
        let rows = community.nodes.len().div_ceil(4);
        groups.push(json!({"id":format!("c{}",community.id),"type":"group","x":0,"y":y,"width":1360,"height":rows*240+70,"label":format!("Community {}: {}",community.id,community.label),"color":(community.id%6+1).to_string()}));
        for (i, id) in community.nodes.iter().enumerate() {
            let node = nodes[id.as_str()];
            let mut card = json!({"id":indices[id.as_str()],"x":30+(i%4)*330,"y":y+50+(i/4)*240,"width":300,"height":210});
            if let Some(filenames) = filenames {
                card["type"] = json!("file");
                card["file"] = json!(filenames[id.as_str()]);
            } else {
                card["type"] = json!("text");
                card["text"] = json!(format!(
                    "# {}\n\n{}\n\nSource: {} {}\n\nID: {}",
                    md(&node.label),
                    md(&node.kind),
                    md(&node.file),
                    md(&location(node)),
                    md(&node.id)
                ));
            }
            cards.push(card);
        }
        y += rows * 240 + 130;
    }
    groups.extend(cards);
    let edges: Vec<_> = snapshot.edges.iter().enumerate().map(|(i,e)| json!({"id":format!("e{i}"),"fromNode":indices[e.source.as_str()],"toNode":indices[e.target.as_str()],"fromSide":"right","toSide":"left","fromEnd":"none","toEnd":if e.directed {"arrow"} else {"none"},"label":e.relation,"_graf":e})).collect();
    Ok(serde_json::to_string_pretty(
        &json!({"nodes":groups,"edges":edges,"_graf_snapshot":snapshot}),
    )? + "\n")
}

fn html_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn html_report(title: &str, body: &str) -> String {
    // Replace body last: user text resembling a template marker stays literal.
    include_str!("assets/report.html")
        .replace("__TITLE__", title)
        .replace("__BODY__", body)
}

fn callflow_html(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
    let nodes: BTreeMap<_, _> = snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let edges: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|e| {
            matches!(
                e.relation.as_str(),
                "calls" | "calls_method" | "uses" | "imports" | "imports_from" | "re_exports"
            )
        })
        .cloned()
        .collect();
    let ids: BTreeSet<_> = edges
        .iter()
        .flat_map(|e| [e.source.as_str(), e.target.as_str()])
        .collect();
    let selected = GraphSnapshot {
        nodes: ids.iter().map(|id| nodes[id].clone()).collect(),
        edges: edges.clone(),
        ..snapshot.clone()
    };
    let mut output = format!(
        "<p>Generation {}. Recorded calls, calls_method, uses, imports, imports_from and re_exports. Arrows retain recorded direction; this is not a runtime sequence or inferred architecture.</p><p>Every row includes the recorded source location and confidence. Unknown sources remain unknown.</p><h2>Relation overview</h2>{}<h2>Source evidence</h2>",
        snapshot.generation,
        svg(&selected, options)?
    );
    let mut sections = BTreeMap::<&str, Vec<&crate::model::Edge>>::new();
    for edge in &edges {
        sections
            .entry(nodes[edge.source.as_str()].file.as_str())
            .or_default()
            .push(edge);
    }
    if sections.is_empty() {
        output.push_str("<p>No selected relations in this snapshot.</p>");
    }
    for (file, edges) in sections {
        writeln!(
            output,
            "<details data-branch open><summary>{}</summary><table><thead><tr><th>Source</th><th>Relation</th><th>Target</th><th>Evidence</th><th>Confidence</th></tr></thead><tbody>",
            html_text(if file.is_empty() {
                "Source unavailable"
            } else {
                file
            })
        )?;
        for edge in edges {
            writeln!(
                output,
                "<tr data-search><td>{}</td><td>{} {}</td><td>{}</td><td>{} {} · {}</td><td>{}</td></tr>",
                html_text(&nodes[edge.source.as_str()].label),
                html_text(&edge.relation),
                if edge.directed { "→" } else { "↔" },
                html_text(&nodes[edge.target.as_str()].label),
                html_text(edge.file.as_deref().unwrap_or("Source unavailable")),
                edge.line.map(|n| format!("L{n}")).unwrap_or_default(),
                html_text(&edge.id),
                html_text(&edge.confidence)
            )?;
        }
        output.push_str("</tbody></table></details>");
    }
    Ok(html_report("Graf architecture and callflow", &output))
}

fn tree_html(snapshot: &GraphSnapshot) -> Result<String> {
    // Folder paths are display keys only; never filesystem operations or URLs.
    let mut files = BTreeMap::<String, Vec<&Node>>::new();
    for node in &snapshot.nodes {
        files
            .entry(if node.file.is_empty() {
                "(source unavailable)".into()
            } else {
                node.file.replace('\\', "/")
            })
            .or_default()
            .push(node);
    }
    let mut output = format!(
        "<p>Generation {} · {} nodes. A source-file tree of recorded entities, not a call hierarchy. Expand a symbol for its complete record.</p>",
        snapshot.generation,
        snapshot.nodes.len()
    );
    let mut open: Vec<String> = Vec::new();
    for (file, mut nodes) in files {
        let parts: Vec<_> = file.split('/').collect();
        let folders = &parts[..parts.len() - 1];
        let shared = open
            .iter()
            .zip(folders)
            .take_while(|(a, b)| a.as_str() == **b)
            .count();
        for _ in shared..open.len() {
            output.push_str("</details>");
        }
        open.truncate(shared);
        for folder in &folders[shared..] {
            writeln!(
                output,
                "<details data-branch><summary>{}</summary>",
                html_text(if folder.is_empty() { "/" } else { folder })
            )?;
            open.push((*folder).into());
        }
        writeln!(
            output,
            "<details data-branch><summary>{} ({} entities)</summary>",
            html_text(parts.last().unwrap()),
            nodes.len()
        )?;
        nodes.sort_by(|a, b| {
            a.line
                .cmp(&b.line)
                .then(a.label.cmp(&b.label))
                .then(a.id.cmp(&b.id))
        });
        for node in nodes {
            writeln!(
                output,
                "<details data-search><summary>{} · {} · {}</summary><pre>{}</pre></details>",
                html_text(&node.label),
                html_text(&node.kind),
                html_text(&location(node)),
                html_text(&serde_json::to_string_pretty(node)?)
            )?;
        }
        output.push_str("</details>");
    }
    for _ in &open {
        output.push_str("</details>");
    }
    Ok(html_report("Graf source tree", &output))
}

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

// Display membership comes only from actual incidence edges. Ordinary group
// relations and every original node/edge remain in the snapshot and analysis.

// Reconstruct only recognizable, exclusively generated group incidence records.
// If a group acquired ordinary relations, keep its node/edges representation so
// those relations never lose their endpoints or become duplicate memberships.

// XML 1.0 cannot represent some control characters. Return an error instead of
// silently corrupting the source; JSON formats can carry these characters.

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

// Encode Markdown metacharacters as character references, keeping Unicode text.
// Source paths are text, never interpolated as links or Obsidian wikilinks.
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

mod export_analysis;
use export_analysis::*;
mod report_markdown;
use report_markdown::*;

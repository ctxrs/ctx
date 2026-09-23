//! Cross-project edges backed by unresolved references and public binding keys.
//!
//! No names, source files, or runtime dispatch are inferred here. Producers must
//! explicitly attest `cross_project_public` on declarations. Unattested keys,
//! ambiguous declarations/owning types and unsupported key families remain
//! unresolved.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::model::{Edge, ImportedGraph, Node, Reference};

const REFERENCES: &str = "graf_unresolved_references";
const MARKER: &str = "graf_composition";

/// Recompute this pass's inferred edges without changing nodes or source facts.
/// Returns the resulting generated-edge count, including on repeated calls.
/// Invalid provenance, malformed references or ID collisions leave the graph
/// unchanged. Absent/ambiguous/ineligible targets produce no edge.
///
/// Input is `snapshot::merge` output: `metadata.projects[*].metadata` carries
/// `graf_unresolved_references: Vec<Reference>`, and node metadata identifies
/// `project`, `original_id`, and `original_metadata`. Graf snapshot wrappers are
/// followed to retain reference evidence across snapshot read/import roundtrips.
pub fn link_references(graph: &mut ImportedGraph) -> Result<usize> {
    let projects = graph
        .metadata
        .get("projects")
        .and_then(Value::as_array)
        .context("composition requires metadata.projects from snapshot::merge")?;
    let mut references = BTreeMap::new();
    for project in projects {
        let name = project
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .context("composition project needs a name")?;
        let metadata = project
            .get("metadata")
            .context("composition project needs metadata")?;
        let refs = snapshot_references(metadata).with_context(|| format!("project {name}"))?;
        ensure!(
            references.insert(name, refs).is_none(),
            "duplicate composition project: {name}"
        );
    }

    let mut identities = BTreeMap::new();
    let mut owners = Vec::with_capacity(graph.nodes.len());
    let mut by_key: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    for (index, node) in graph.nodes.iter().enumerate() {
        let project = node
            .metadata
            .get("project")
            .and_then(Value::as_str)
            .context("composition node lacks project provenance")?;
        let original = node
            .metadata
            .get("original_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .context("composition node lacks original_id provenance")?;
        ensure!(
            node.metadata.get("original_metadata").is_some(),
            "composition node lacks original_metadata provenance"
        );
        ensure!(
            references.contains_key(project),
            "node belongs to an unknown project: {project}"
        );
        ensure!(
            node.id == format!("graf:project:{}", json!([project, original])),
            "node ID disagrees with its composition provenance: {}",
            node.id
        );
        ensure!(
            identities.insert((project, original), index).is_none(),
            "duplicate composition node identity"
        );
        owners.push(project);
        if let Some(key) = node.binding_key.as_deref() {
            by_key.entry(key).or_default().insert(index);
        }
        if let Some(aliases) = attributes(node).get("binding_aliases") {
            for alias in aliases
                .as_array()
                .context("binding_aliases must be an array")?
            {
                let key = alias
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .context("binding aliases must be nonempty strings")?;
                by_key.entry(key).or_default().insert(index);
            }
        }
    }

    // Remove only our recognizable previous output. This permits recomputing
    // after its target disappears without accepting dangling source edges.
    let mut ids = BTreeSet::new();
    for edge in &graph.edges {
        ensure!(
            ids.insert(edge.id.as_str()),
            "duplicate edge ID: {}",
            edge.id
        );
    }
    let mut edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| !previous_output(e))
        .cloned()
        .collect();
    crate::snapshot::validate_graph(&graph.nodes, &edges)?;
    let mut edge_ids: BTreeSet<_> = edges.iter().map(|e| e.id.clone()).collect();
    let mut generated = Vec::new();
    for (project, refs) in references {
        let mut ref_ids = BTreeSet::new();
        for reference in &refs {
            ensure!(
                !reference.id.is_empty() && ref_ids.insert(reference.id.as_str()),
                "empty or duplicate reference ID in project {project}"
            );
            ensure!(
                !reference.file.is_empty() && reference.line > 0 && !reference.relation.is_empty(),
                "reference {} lacks source location or relation",
                reference.id
            );
            let source = *identities
                .get(&(project, reference.source.as_str()))
                .with_context(|| {
                    format!(
                        "reference {} has an unknown source in project {project}",
                        reference.id
                    )
                })?;
            let source_node = &graph.nodes[source];
            ensure!(
                reference.file == source_node.file,
                "reference {} file disagrees with source node",
                reference.id
            );
            let Some(language) = attributes(source_node)
                .get("language")
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some((target, key)) =
                resolve(reference, project, language, &graph.nodes, &owners, &by_key)
            else {
                continue;
            };
            let id = edge_id(project, &reference.id);
            ensure!(
                edge_ids.insert(id.clone()),
                "composition edge ID collision: {id}"
            );
            generated.push(Edge {
                id,
                source: source_node.id.clone(),
                target: graph.nodes[target].id.clone(),
                relation: reference.relation.clone(),
                directed: true,
                file: Some(reference.file.clone()),
                line: Some(reference.line),
                confidence: "INFERRED".into(),
                metadata: json!({"graf_composition": {
                    "schema_version": 1, "source_project": project,
                    "target_project": owners[target], "reference_id": reference.id,
                    "binding_key": key, "reference": reference,
                }}),
            });
        }
    }
    generated.sort_by(|a, b| a.id.cmp(&b.id));
    let count = generated.len();
    edges.extend(generated);
    graph.edges = edges;
    Ok(count)
}

fn snapshot_references(mut metadata: &Value) -> Result<Vec<Reference>> {
    loop {
        if let Some(refs) = metadata.get(REFERENCES) {
            ensure!(
                refs.is_array(),
                "graf_unresolved_references must be an array"
            );
            return serde_json::from_value(refs.clone())
                .context("invalid unresolved reference payloads");
        }
        match metadata
            .get("graf_snapshot")
            .and_then(|header| header.get("metadata"))
        {
            Some(nested) => metadata = nested,
            None => return Ok(Vec::new()),
        }
    }
}

fn attributes(node: &Node) -> &Value {
    let mut value = &node.metadata;
    while let Some(original) = value.get("original_metadata") {
        value = original;
    }
    value
}

struct Key<'a> {
    language: &'a str,
    family: &'a str,
    qualified: &'a str,
}

fn stable_key(key: &str) -> Option<Key<'_>> {
    let mut parts = key.splitn(3, ':');
    let language = parts.next()?;
    let family = parts.next()?;
    let qualified = parts.next()?;
    if !matches!(language, "java" | "csharp" | "kotlin" | "cpp")
        || !matches!(family, "symbol" | "static" | "member")
        || !qualified.contains('.')
        || !qualified.split('.').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
                && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        })
    {
        return None;
    }
    Some(Key {
        language,
        family,
        qualified,
    })
}

fn eligible(node: &Node, key: &Key<'_>, relation: &str) -> bool {
    let attrs = attributes(node);
    let Some(primary) = node.binding_key.as_deref().and_then(stable_key) else {
        return false;
    };
    if node.file.is_empty()
        || attrs.get("cross_project_public") != Some(&Value::Bool(true))
        || attrs.get("language").and_then(Value::as_str) != Some(key.language)
        || attrs.get("qualified_symbol").and_then(Value::as_str) != Some(key.qualified)
        || node.qualified_name.as_deref() != Some(key.qualified)
        || primary.language != key.language
        || primary.qualified != key.qualified
    {
        return false;
    }
    let callable = matches!(node.kind.as_str(), "function" | "method")
        && attrs.get("dynamic_dispatch") == Some(&Value::Bool(false));
    let ty = matches!(
        node.kind.as_str(),
        "class" | "struct" | "union" | "enum" | "interface" | "type"
    );
    let dispatch = match key.family {
        "symbol" => true,
        "static" => callable && attrs.get("static") == Some(&Value::Bool(true)),
        "member" => {
            node.kind == "method" && callable && attrs.get("static") == Some(&Value::Bool(false))
        }
        _ => false,
    };
    dispatch
        && match relation {
            "calls" => callable,
            "references" | "uses_type" | "inherits" | "extends" | "implements" => {
                ty && key.family == "symbol"
            }
            "imports" => (ty || callable) && key.family == "symbol",
            _ => false,
        }
}

fn unique_owner(
    target: usize,
    key: &Key<'_>,
    nodes: &[Node],
    owners: &[&str],
    by_key: &BTreeMap<&str, BTreeSet<usize>>,
) -> bool {
    // A method's qualified prefix is its declaring type, including when a
    // symbol key (rather than a static/member alias) references the method.
    // Free functions have no type owner. Do not choose a type by which of its
    // competing declarations happens to contain the requested member.
    if nodes[target].kind != "method" && key.family == "symbol" {
        return true;
    }
    let Some((qualified, _)) = key.qualified.rsplit_once('.') else {
        return false;
    };
    let owner_key = format!("{}:symbol:{qualified}", key.language);
    let Some(matches) = by_key.get(owner_key.as_str()) else {
        return false;
    };
    if matches.len() != 1 {
        return false;
    }
    let owner = *matches.first().unwrap();
    owners[owner] == owners[target]
        && eligible(
            &nodes[owner],
            &Key {
                language: key.language,
                family: "symbol",
                qualified,
            },
            "references",
        )
}

fn resolve<'a>(
    reference: &'a Reference,
    project: &str,
    language: &str,
    nodes: &[Node],
    owners: &[&str],
    by_key: &BTreeMap<&str, BTreeSet<usize>>,
) -> Option<(usize, &'a str)> {
    for candidate in &reference.candidate_keys {
        // Unsupported higher-priority keys are not permission to guess from a
        // later key. A matching local, private or ambiguous declaration also
        // blocks fallback, matching native resolution's ambiguity policy.
        let key = stable_key(candidate)?;
        if key.language != language {
            return None;
        }
        let Some(matches) = by_key.get(candidate.as_str()) else {
            continue;
        };
        if matches.len() != 1 {
            return None;
        }
        let target = *matches.first()?;
        return (owners[target] != project
            && eligible(&nodes[target], &key, &reference.relation)
            && unique_owner(target, &key, nodes, owners, by_key))
        .then_some((target, candidate.as_str()));
    }
    None
}

fn edge_id(project: &str, reference: &str) -> String {
    format!("graf:composition:{}", json!([project, reference]))
}

fn previous_output(edge: &Edge) -> bool {
    let Some(marker) = edge.metadata.get(MARKER) else {
        return false;
    };
    let Some(project) = marker.get("source_project").and_then(Value::as_str) else {
        return false;
    };
    let Some(reference) = marker.get("reference_id").and_then(Value::as_str) else {
        return false;
    };
    marker.get("schema_version").and_then(Value::as_u64) == Some(1)
        && edge.id == edge_id(project, reference)
}

//! Explicit node-link and Graphify-export snapshot import.
//!
//! In strict node-link mode the root requires `nodes`, exactly one of `links`/`edges`, and boolean
//! `directed`/`multigraph`. Nodes require a string or integer `id`; edges require
//! `source`/`target` (or agreeing `from`/`to` aliases). Strings remain unchanged;
//! integers in i64/u64 range become `graphify:integer:<decimal>`. Floating-point
//! IDs and collisions with that string namespace are rejected.
//!
//! Missing labels default to the ID, file types to `concept`, node source files
//! to an empty string, relations to `related`, and confidence to `UNKNOWN`.
//! Missing edge source files remain absent. Present
//! values must have the correct type. Node aliases are `name` for `label` and
//! `source`/`path` for `source_file`; edge `type` aliases `relation`. Aliases must
//! agree. Confidence is a nonempty string; optional confidence_score is 0..=1.
//!
//! `_src` and `_tgt` must appear together and name the same endpoint pair. They
//! orient an undirected legacy edge; on a directed graph they must agree with
//! source/target order. An optional edge `directed` must agree with this result.
//! Simple graphs reject repeated endpoint pairs; multigraphs require distinct
//! string/integer keys per pair. Imported edge IDs are input-order ordinals.
//!
//! Source locations are null or strings. `L12`, `L12-L15`, and `L12-15` produce
//! line numbers; other textual locations remain opaque. All original node/edge
//! fields are retained in metadata, as are all root fields except the node/edge
//! arrays. `graph`, if present, must be an object. `hyperedges` and `groups`
//! arrays at the root or in `graph` become queryable group nodes and undirected
//! `member_of` edges. Nonempty mirrored arrays must agree. Groups accept
//! nodes/members/node_ids arrays of typed IDs or objects containing id. Original
//! group records and member order remain in metadata. Duplicate JSON keys are errors at every depth.
//! Only the supplied regular file is read, with a hard 256 MiB limit.
//!
//! [`read_graphify_export`] explicitly selects Graphify producer semantics:
//! missing root flags are permitted (default false for storage identity).
//! Every edge is directed, using `_src`/`_tgt` when present, otherwise ordered
//! source/target. Markers must name the same unordered endpoint pair, even if
//! the root says directed=true. Root and edge direction flags describe storage;
//! present flags must still be booleans, but do not override export direction.
//! Without multigraph=true, keys are forbidden and every unkeyed fact survives,
//! including repeated pairs/relations. With multigraph=true, every edge needs a
//! string/integer key unique per serialized endpoint pair: ordered when the
//! root directed=true, unordered otherwise. All modes assign ordinal edge IDs
//! and retain original metadata without inserting or rewriting flags.
//! Export mode also accepts known legacy file-type names and finite numeric
//! confidence/weight strings. Converted edge attributes retain their original
//! record in `_graf_import_original`. Import-family targets absent from the
//! snapshot become explicitly marked external concept nodes; missing sources
//! and other dangling relations remain errors. Strict node-link mode does not
//! perform these compatibility conversions.
//!
//! Graf-generated node-link exports opt into lossless record restoration with
//! a versioned root `_graf` header. Node/edge `_graf` and group `_graf_group`
//! payloads must agree with canonical IDs, endpoints and direction; conflicting
//! or duplicate restored identities are errors. In this format mixed direction
//! is explicit even in export mode, and full original record metadata is restored.

use std::collections::HashSet;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};

use crate::model::{Edge, ImportedGraph, Node};

const MAX_BYTES: u64 = 256 * 1024 * 1024;

pub fn read_graphify(path: &Path) -> Result<ImportedGraph> {
    read_snapshot(path, false)
}

/// Read a Graphify-produced export, including raw no-cluster extraction/update.
/// See the module contract for direction and parallel-edge interpretation.
pub fn read_graphify_export(path: &Path) -> Result<ImportedGraph> {
    read_snapshot(path, true)
}

pub(crate) fn read_json(path: &Path) -> Result<Value> {
    // Reject nonregular paths before open; symlinks to regular files remain supported.
    ensure!(
        std::fs::metadata(path)
            .context("cannot inspect snapshot")?
            .is_file(),
        "snapshot must be a regular file"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A regular path can become a FIFO after the check. Nonblocking open
        // lets the descriptor check below reject it without waiting for a writer.
        // O_NONBLOCK has no effect on regular files.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).context("cannot open snapshot")?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "snapshot must be a regular file");
    ensure!(
        metadata.len() <= MAX_BYTES,
        "snapshot exceeds 256 MiB limit"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("cannot read snapshot")?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "snapshot exceeds 256 MiB limit"
    );
    let StrictJson(value) = serde_json::from_slice(&bytes).context("invalid snapshot JSON")?;
    Ok(value)
}

fn read_snapshot(path: &Path, export: bool) -> Result<ImportedGraph> {
    let mut root = into_object(read_json(path)?).context("Graphify root must be an object")?;
    // A versioned Graf header opts into restoration; arbitrary producer _graf
    // metadata without a schema declaration remains opaque for compatibility.
    let graf = root
        .get("_graf")
        .is_some_and(|v| v.get("schema_version").is_some());
    if graf {
        crate::snapshot::graf_header(&root["_graf"])?;
    }
    let directed = if export && !root.contains_key("directed") {
        false
    } else {
        required_bool(&root, "directed")?
    };
    let multigraph = if export && !root.contains_key("multigraph") {
        false
    } else {
        required_bool(&root, "multigraph")?
    };
    let groups = group_records(&root)?.to_vec();
    ensure!(
        root.contains_key("links") != root.contains_key("edges"),
        "expected exactly one of links or edges"
    );
    let nodes = take_array(&mut root, "nodes")?;
    let edge_field = if root.contains_key("links") {
        "links"
    } else {
        "edges"
    };
    let edges = take_array(&mut root, edge_field)?;
    let mut ids = HashSet::new();
    let mut mapped_ids = HashSet::new();
    let mut imported_nodes = Vec::with_capacity(nodes.len());
    for (index, value) in nodes.into_iter().enumerate() {
        let node = (|| -> Result<Node> {
            let attrs = into_object(value)?;
            let id = Id::parse(attrs.get("id").context("missing id")?)?;
            ensure!(ids.insert(id.clone()), "duplicate node ID");
            let mapped = id.mapped();
            ensure!(
                mapped_ids.insert(mapped.clone()),
                "numeric ID mapping collides with a string ID"
            );
            let label = text_alias(&attrs, &["label", "name"])?
                .unwrap_or(&mapped)
                .to_owned();
            let kind = if export && !graf {
                export_kind(&attrs)?
            } else {
                text_alias(&attrs, &["file_type"])?.unwrap_or("concept")
            }
            .to_owned();
            let file = text_alias(&attrs, &["source_file", "source", "path"])?
                .unwrap_or("")
                .to_owned();
            let (line, end_line) = location(&attrs)?;
            let qualified_name = text_alias(&attrs, &["qualified_name"])?.map(str::to_owned);
            let node = Node {
                id: mapped,
                label,
                kind,
                file,
                line,
                end_line,
                qualified_name,
                binding_key: None,
                metadata: Value::Object(attrs),
            };
            if graf { restore_node(node) } else { Ok(node) }
        })()
        .with_context(|| format!("nodes[{index}]"))?;
        imported_nodes.push(node);
    }
    if export && !graf {
        external_import_targets(&edges, &mut ids, &mut mapped_ids, &mut imported_nodes)?;
    }
    let mut identities = HashSet::new();
    let mut imported_edges = Vec::with_capacity(edges.len());
    for (index, value) in edges.into_iter().enumerate() {
        let edge = (|| -> Result<Edge> {
            let mut attrs = into_object(value)?;
            if export && !graf {
                normalize_export_edge(&mut attrs)?;
            }
            let source = endpoint(&attrs, &["source", "from"], &ids)?;
            let target = endpoint(&attrs, &["target", "to"], &ids)?;
            let key = if multigraph {
                Some(Id::parse(
                    attrs.get("key").context("multigraph edge requires key")?,
                )?)
            } else {
                ensure!(!attrs.contains_key("key"), "key requires multigraph=true");
                None
            };
            // Identity follows the serialized graph's direction, even when legacy
            // markers recover a direction from an undirected storage format.
            let pair = if !directed && source > target {
                (target.clone(), source.clone())
            } else {
                (source.clone(), target.clone())
            };
            if !export || multigraph {
                ensure!(identities.insert((pair, key)), "duplicate edge identity");
            }
            let (source, target, edge_directed) = match (attrs.get("_src"), attrs.get("_tgt")) {
                (None, None) => (source, target, export || directed),
                (Some(_), Some(_)) => {
                    let src = endpoint(&attrs, &["_src"], &ids)?;
                    let tgt = endpoint(&attrs, &["_tgt"], &ids)?;
                    let same = src == source && tgt == target;
                    let reversed = src == target && tgt == source;
                    ensure!(
                        same || ((export || !directed) && reversed),
                        "conflicting _src/_tgt orientation"
                    );
                    (src, tgt, true)
                }
                _ => bail!("_src and _tgt must appear together"),
            };
            if attrs.contains_key("directed") {
                let edge_flag = required_bool(&attrs, "directed")?;
                ensure!(
                    export || edge_flag == edge_directed,
                    "edge directed conflicts with graph direction or _src/_tgt"
                );
            }
            let relation = text_alias(&attrs, &["relation", "type"])?
                .unwrap_or("related")
                .to_owned();
            ensure!(!relation.is_empty(), "relation must not be empty");
            let confidence = text_alias(&attrs, &["confidence"])?
                .unwrap_or("UNKNOWN")
                .to_owned();
            ensure!(!confidence.is_empty(), "confidence must not be empty");
            if let Some(score) = attrs.get("confidence_score") {
                let score = score
                    .as_f64()
                    .context("confidence_score must be a number")?;
                ensure!(
                    (0.0..=1.0).contains(&score),
                    "confidence_score must be between 0 and 1"
                );
            }
            let file = text_alias(&attrs, &["source_file"])?.map(str::to_owned);
            let (line, _) = location(&attrs)?;
            let canonical_directed = directed || attrs.contains_key("_src");
            let edge = Edge {
                id: format!("graphify:edge:{index}"),
                source: source.mapped(),
                target: target.mapped(),
                relation,
                directed: edge_directed,
                file,
                line,
                confidence,
                metadata: Value::Object(attrs),
            };
            if graf {
                restore_edge(edge, canonical_directed)
            } else {
                Ok(edge)
            }
        })()
        .with_context(|| format!("{edge_field}[{index}]"))?;
        imported_edges.push(edge);
    }
    materialize_groups(
        graf,
        &groups,
        &ids,
        &mut mapped_ids,
        &mut imported_nodes,
        &mut imported_edges,
    )?;
    if graf {
        crate::snapshot::validate_graph(&imported_nodes, &imported_edges)?;
    }
    Ok(ImportedGraph {
        nodes: imported_nodes,
        edges: imported_edges,
        metadata: Value::Object(root),
    })
}

fn export_kind(attrs: &Map<String, Value>) -> Result<&str> {
    let kind = match attrs.get("file_type") {
        None | Some(Value::Null) => "concept",
        Some(value) => value
            .as_str()
            .context("file_type must be a string or null")?,
    };
    Ok(match kind {
        "" => "concept",
        "markdown" | "text" => "document",
        "tool" | "library" => "code",
        "pattern" | "principle" | "constraint" | "tech" | "technology" | "data-source"
        | "data_source" | "gotcha" | "framework" => "concept",
        other => other,
    })
}

fn normalize_export_edge(attrs: &mut Map<String, Value>) -> Result<()> {
    let original = attrs.clone();
    for key in ["weight", "confidence_score"] {
        if let Some(value) = attrs.get(key) {
            let number = match value {
                Value::String(text) => text.parse::<f64>().ok(),
                value => value.as_f64(),
            }
            .with_context(|| format!("{key} must be numeric"))?;
            ensure!(
                number.is_finite() && number >= 0.0,
                "{key} must be finite and nonnegative"
            );
            if value.is_string() {
                attrs.insert(key.into(), Value::from(number));
            }
        }
    }
    let numeric_confidence = match attrs.get("confidence") {
        Some(Value::Number(number)) => Some(number.as_f64().context("invalid numeric confidence")?),
        Some(Value::String(text)) => text.parse::<f64>().ok(),
        _ => None,
    };
    if let Some(score) = numeric_confidence {
        ensure!(
            score.is_finite() && (0.0..=1.0).contains(&score),
            "numeric confidence must be between 0 and 1"
        );
        attrs
            .entry("confidence_score")
            .or_insert(Value::from(score));
        attrs.insert("confidence".into(), Value::String("INFERRED".into()));
    } else if !attrs.contains_key("confidence") && attrs.contains_key("confidence_score") {
        attrs.insert("confidence".into(), Value::String("INFERRED".into()));
    }
    if *attrs != original {
        attrs.insert("_graf_import_original".into(), Value::Object(original));
    }
    Ok(())
}

fn external_import_targets(
    edges: &[Value],
    ids: &mut HashSet<Id>,
    mapped_ids: &mut HashSet<String>,
    nodes: &mut Vec<Node>,
) -> Result<()> {
    let declared = ids.clone();
    for (index, value) in edges.iter().enumerate() {
        let attrs = value
            .as_object()
            .with_context(|| format!("edge[{index}] must be an object"))?;
        if !matches!(
            text_alias(attrs, &["relation", "type"])?,
            Some("imports" | "imports_from" | "re_exports")
        ) {
            continue;
        }
        // Direction markers are checked against the serialized endpoints in the
        // normal edge reader. Only an originally declared source can mint a stub.
        let (source, target) = match (attrs.get("_src"), attrs.get("_tgt")) {
            (Some(source), Some(target)) => (Some(source), Some(target)),
            (None, None) => (
                alias(attrs, &["source", "from"])?,
                alias(attrs, &["target", "to"])?,
            ),
            _ => continue,
        };
        let (Some(source), Some(target)) = (source, target) else {
            continue;
        };
        let source = Id::parse(source)?;
        let target_id = Id::parse(target)?;
        if !declared.contains(&source) || ids.contains(&target_id) {
            continue;
        }
        let mapped = target_id.mapped();
        ensure!(
            mapped_ids.insert(mapped.clone()),
            "numeric ID mapping collides with a string ID"
        );
        ids.insert(target_id);
        nodes.push(Node {
            id: mapped.clone(), label: mapped, kind: "concept".into(), file: String::new(),
            line: None, end_line: None, qualified_name: None, binding_key: None,
            metadata: serde_json::json!({"id":target,"external":true,"type":"external","graf_generated":"external_import_target"}),
        });
    }
    Ok(())
}

fn restore_node(node: Node) -> Result<Node> {
    let Some(value) = node.metadata.get("_graf") else {
        return Ok(node);
    };
    let original = crate::snapshot::graf_node(value)?;
    ensure!(
        original.id == node.id
            && original.label == node.label
            && original.kind == node.kind
            && original.file == node.file
            && original.line == node.line
            && original.end_line == node.end_line
            && original.qualified_name == node.qualified_name,
        "_graf node conflicts with canonical node fields"
    );
    Ok(original)
}

fn restore_edge(edge: Edge, directed: bool) -> Result<Edge> {
    let Some(value) = edge.metadata.get("_graf") else {
        return Ok(edge);
    };
    let original = crate::snapshot::graf_edge(value)?;
    ensure!(
        edge.metadata.get("id").and_then(Value::as_str) == Some(original.id.as_str())
            && edge
                .metadata
                .get("key")
                .and_then(Value::as_str)
                .is_none_or(|k| k == original.id)
            && original.source == edge.source
            && original.target == edge.target
            && original.directed == directed
            && original.relation == edge.relation
            && original.file == edge.file
            && original.line == edge.line
            && original.confidence == edge.confidence,
        "_graf edge conflicts with canonical edge fields"
    );
    if let Some(flag) = edge.metadata.get("directed").and_then(Value::as_bool) {
        ensure!(
            flag == original.directed,
            "_graf edge conflicts with direction flag"
        );
    }
    Ok(original)
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Id {
    Text(String),
    Integer(String),
}

impl Id {
    fn parse(value: &Value) -> Result<Self> {
        match value {
            Value::String(s) => Ok(Self::Text(s.clone())),
            Value::Number(n) if n.is_i64() || n.is_u64() => Ok(Self::Integer(n.to_string())),
            _ => bail!(
                "ID/key must be a string or an integer in i64/u64 range; floating-point IDs are unsupported"
            ),
        }
    }

    fn mapped(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Integer(n) => format!("graphify:integer:{n}"),
        }
    }
}

fn into_object(value: Value) -> Result<Map<String, Value>> {
    match value {
        Value::Object(map) => Ok(map),
        _ => bail!("expected an object"),
    }
}

fn take_array(root: &mut Map<String, Value>, key: &str) -> Result<Vec<Value>> {
    match root.remove(key) {
        Some(Value::Array(items)) => Ok(items),
        _ => bail!("{key} must be an array"),
    }
}

fn required_bool(attrs: &Map<String, Value>, key: &str) -> Result<bool> {
    attrs
        .get(key)
        .and_then(Value::as_bool)
        .with_context(|| format!("{key} must be a boolean"))
}

// Graphify writes the same hyperedges both at the root and inside graph.
// Accept an empty companion slot, but never silently prefer conflicting data.
fn group_records(root: &Map<String, Value>) -> Result<&[Value]> {
    let nested = root
        .get("graph")
        .map(|g| g.as_object().context("graph must be an object"))
        .transpose()?;
    let mut selected: Option<&Vec<Value>> = None;
    for attrs in std::iter::once(root).chain(nested) {
        for key in ["hyperedges", "groups"] {
            if let Some(value) = attrs.get(key) {
                let records = value
                    .as_array()
                    .with_context(|| format!("{key} must be an array"))?;
                if !records.is_empty() {
                    ensure!(
                        selected.is_none_or(|previous| previous == records),
                        "conflicting hyperedges/groups arrays"
                    );
                    selected = Some(records);
                }
            }
        }
    }
    Ok(selected.map_or(&[], Vec::as_slice))
}

fn materialize_groups(
    graf: bool,
    groups: &[Value],
    ids: &HashSet<Id>,
    mapped_ids: &mut HashSet<String>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> Result<()> {
    let mut group_ids = HashSet::new();
    // Allocate unused IDs without reserving any caller-owned string prefix.
    let mut next_group_id = 0;
    for (ordinal, value) in groups.iter().enumerate() {
        (|| -> Result<()> {
            let attrs = value.as_object().context("group must be an object")?;
            if let Some(id) = attrs.get("id") {
                let id = Id::parse(id)?;
                ensure!(group_ids.insert(id), "duplicate group ID");
            }
            let members = alias(attrs, &["nodes", "members", "node_ids"])?
                .and_then(Value::as_array).context("group nodes/members/node_ids must be an array")?;
            ensure!(!members.is_empty(), "group must have at least one member");
            let members = members.iter().map(|member| {
                let member = if let Some(record) = member.as_object() {
                    record.get("id").context("group member object requires id")?
                } else { member };
                let id = Id::parse(member)?;
                ensure!(ids.contains(&id), "group references an unknown node ID");
                Ok(id.mapped())
            }).collect::<Result<Vec<_>>>()?;
            let label = text_alias(attrs, &["label", "name"])?
                .map(str::to_owned)
                .or_else(|| attrs.get("id").map(|v| match v {
                    Value::String(s) => s.clone(), _ => v.to_string(),
                })).unwrap_or_else(|| format!("Group {}", ordinal + 1));
            let file = text_alias(attrs, &["source_file"])?;
            let (line, end_line) = location(attrs)?;
            let confidence = text_alias(attrs, &["confidence"])?.unwrap_or("UNKNOWN");
            ensure!(!confidence.is_empty(), "confidence must not be empty");
            if let Some(relation) = text_alias(attrs, &["relation", "type"])? {
                ensure!(!relation.is_empty(), "relation must not be empty");
            }
            if let Some(score) = attrs.get("confidence_score") {
                ensure!(score.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n)),
                    "confidence_score must be between 0 and 1");
            }
            if attrs.contains_key("directed") { required_bool(attrs, "directed")?; }
            if graf && let Some(extension) = attrs.get("_graf_group") {
                let extension = extension.as_object().context("_graf_group must be an object")?;
                ensure!(extension.keys().all(|k| matches!(k.as_str(), "node" | "incidences")), "unknown _graf_group field");
                let node = crate::snapshot::graf_node(extension.get("node").context("missing group node")?)?;
                ensure!(node.kind == "group" && !node.id.is_empty() && mapped_ids.insert(node.id.clone()),
                    "_graf_group node ID collides or is not a group");
                let incidences = extension.get("incidences").and_then(Value::as_array).context("group incidences must be an array")?;
                ensure!(incidences.len() == members.len(), "_graf_group membership count conflicts");
                for (member, value) in members.iter().zip(incidences) {
                    let edge = crate::snapshot::graf_edge(value)?;
                    ensure!(edge.source == *member && edge.target == node.id && !edge.directed && edge.relation == "member_of",
                        "_graf_group incidence conflicts with canonical membership");
                    edges.push(edge);
                }
                nodes.push(node);
                return Ok(());
            }
            let id = loop {
                let candidate = format!("graphify:group:{next_group_id}");
                next_group_id += 1;
                if mapped_ids.insert(candidate.clone()) { break candidate; }
            };
            nodes.push(Node {
                id: id.clone(), label, kind: "group".to_owned(),
                file: file.unwrap_or("").to_owned(), line, end_line,
                qualified_name: None, binding_key: None,
                metadata: serde_json::json!({"graphify_group": value}),
            });
            for (index, member) in members.into_iter().enumerate() {
                edges.push(Edge {
                    id: format!("graphify:member:{ordinal}:{index}"),
                    source: member, target: id.clone(), relation: "member_of".to_owned(),
                    directed: false, file: file.map(str::to_owned), line,
                    confidence: confidence.to_owned(),
                    metadata: serde_json::json!({"graphify_group_ordinal": ordinal, "member_index": index}),
                });
            }
            Ok(())
        })().with_context(|| format!("groups[{ordinal}]"))?;
    }
    Ok(())
}

fn alias<'a>(attrs: &'a Map<String, Value>, names: &[&str]) -> Result<Option<&'a Value>> {
    let mut value = None;
    for name in names {
        if let Some(next) = attrs.get(*name) {
            ensure!(
                value.is_none_or(|previous| previous == next),
                "conflicting aliases: {}",
                names.join("/")
            );
            value = Some(next);
        }
    }
    Ok(value)
}

fn text_alias<'a>(attrs: &'a Map<String, Value>, names: &[&str]) -> Result<Option<&'a str>> {
    alias(attrs, names)?
        .map(|v| {
            v.as_str()
                .with_context(|| format!("{} must be a string", names[0]))
        })
        .transpose()
}

fn endpoint(attrs: &Map<String, Value>, names: &[&str], ids: &HashSet<Id>) -> Result<Id> {
    let value = alias(attrs, names)?.with_context(|| format!("missing {}", names[0]))?;
    let id = Id::parse(value)?;
    ensure!(
        ids.contains(&id),
        "{} references an unknown node ID",
        names[0]
    );
    Ok(id)
}

fn location(attrs: &Map<String, Value>) -> Result<(Option<u32>, Option<u32>)> {
    let text = match attrs.get("source_location") {
        None | Some(Value::Null) => return Ok((None, None)),
        Some(Value::String(s)) => s,
        _ => bail!("source_location must be a string or null"),
    };
    let Some(body) = text
        .strip_prefix('L')
        .filter(|s| s.starts_with(|c: char| c.is_ascii_digit()))
    else {
        return Ok((None, None));
    };
    let (start, end) = body.split_once('-').map_or((body, None), |(a, b)| {
        (a, Some(b.strip_prefix('L').unwrap_or(b)))
    });
    let parse = |s: &str| -> Result<u32> {
        ensure!(
            !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()),
            "invalid source line number"
        );
        let n: u32 = s.parse().context("source line number exceeds u32 range")?;
        ensure!(n > 0, "source line numbers are 1-based");
        Ok(n)
    };
    let start = parse(start)?;
    let end = end.map(parse).transpose()?;
    ensure!(
        end.is_none_or(|end| end >= start),
        "source location range is reversed"
    );
    Ok((Some(start), end))
}

// serde_json::Value alone silently overwrites duplicate object keys. Retain its
// ordinary JSON representation, but reject ambiguous objects while deserializing.
struct StrictJson(Value);

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = StrictJson;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::Number(v.into())))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::Number(v.into())))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<StrictJson, E> {
                Number::from_f64(v)
                    .map(|n| StrictJson(Value::Number(n)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::String(v.into())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::String(v)))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<StrictJson, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictJson(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(StrictJson(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<StrictJson, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON object key"));
                    }
                    let StrictJson(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(StrictJson(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}

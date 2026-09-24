use super::*;

pub(super) fn export_analysis(
    snapshot: &GraphSnapshot,
    options: &ExportOptions,
) -> Result<AnalysisReport> {
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

pub(super) fn validate_learning(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<()> {
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

pub(super) fn display_groups(snapshot: &GraphSnapshot) -> Vec<Value> {
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

pub(super) fn location(node: &Node) -> String {
    match (node.line, node.end_line) {
        (Some(a), Some(b)) => format!("L{a}-L{b}"),
        (Some(a), _) => format!("L{a}"),
        _ => String::new(),
    }
}

pub(super) fn graphify(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
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

pub(super) fn active_groups(
    snapshot: &GraphSnapshot,
) -> (BTreeSet<&str>, BTreeSet<&str>, Vec<Value>) {
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

pub(super) fn header(snapshot: &GraphSnapshot) -> Value {
    json!({"schema_version":snapshot.schema_version,"generation":snapshot.generation,"kind":snapshot.kind,"root":snapshot.root,"metadata":snapshot.metadata})
}

pub(super) fn xml(text: &str) -> Result<String> {
    ensure!(text.chars().all(|c| matches!(c, '\t'|'\n'|'\r'|'\u{20}'..='\u{d7ff}'|'\u{e000}'..='\u{fffd}'|'\u{10000}'..='\u{10ffff}')), "XML cannot represent a control character; use snapshot JSON for lossless export");
    Ok(text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

pub(super) fn graphml(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
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

pub(super) fn cypher_string(text: &str) -> String {
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

pub(super) fn cypher(snapshot: &GraphSnapshot) -> Result<String> {
    let mut output = String::from(
        "// Offline Neo4j / FalkorDB Cypher. Execute statements in order.\n// Undirected edges use one stored arrow with directed=false; query them without direction.\n",
    );
    for statement in cypher_statements(snapshot)? {
        writeln!(output, "{statement};")?;
    }
    Ok(output)
}

pub(super) fn mermaid_text(text: &str) -> String {
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

pub(super) fn mermaid(snapshot: &GraphSnapshot) -> String {
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

pub(super) fn positions(snapshot: &GraphSnapshot) -> (Vec<(f64, f64)>, f64) {
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

pub(super) fn svg(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
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

pub(super) fn html(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
    let report = export_analysis(snapshot, options)?;
    let data = serde_json::to_string(&json!({"snapshot":snapshot,"analysis":report,"groups":display_groups(snapshot),"learning":options.learning,"viewer":{"node_limit":options.node_limit,"edge_limit":options.edge_limit}}))?
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    Ok(include_str!("../assets/viewer.html").replace("__GRAF_DATA__", &data))
}

pub(super) fn md(text: &str) -> String {
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

pub(super) fn markdown(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
    let report = export_analysis(snapshot, options)?;
    report_markdown(snapshot, &report, options.learning.as_ref())
}

pub(super) fn learning_node_markdown(node: &LearningNode) -> String {
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

pub(super) fn learning_markdown(
    snapshot: &GraphSnapshot,
    learning: &LearningOverlay,
) -> Result<String> {
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

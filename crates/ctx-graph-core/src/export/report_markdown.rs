use super::*;

pub(super) fn report_markdown(
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

pub(super) fn canvas(
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

pub(super) fn html_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(super) fn html_report(title: &str, body: &str) -> String {
    // Replace body last: user text resembling a template marker stays literal.
    include_str!("../assets/report.html")
        .replace("__TITLE__", title)
        .replace("__BODY__", body)
}

pub(super) fn callflow_html(snapshot: &GraphSnapshot, options: &ExportOptions) -> Result<String> {
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

pub(super) fn tree_html(snapshot: &GraphSnapshot) -> Result<String> {
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

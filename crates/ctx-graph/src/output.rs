use super::*;
use serde::Serialize;

pub(crate) fn human(text: &str) -> impl std::fmt::Display + '_ {
    text.escape_debug()
}

pub(crate) fn diagnostics(out: &mut impl Write, items: &[Diagnostic]) -> io::Result<()> {
    for d in items {
        writeln!(
            out,
            "{}:{}: {}",
            human(&d.file),
            d.line.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
            human(&d.message)
        )?;
    }
    Ok(())
}

pub(crate) fn print_graph(out: &mut impl Write, graph: &GraphResult) -> io::Result<()> {
    writeln!(out, "Generation {} (indexed snapshot)", graph.generation)?;
    for n in &graph.nodes {
        writeln!(
            out,
            "{}  {}  {}  {}:{}",
            human(&n.id),
            human(&n.kind),
            human(&n.label),
            human(&n.file),
            n.line.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
        )?;
    }
    for e in &graph.edges {
        writeln!(
            out,
            "{} --{}{} {}",
            human(&e.source),
            human(&e.relation),
            if e.directed { "-->" } else { "---" },
            human(&e.target)
        )?;
    }
    for r in &graph.unresolved {
        writeln!(
            out,
            "unresolved: {} --{}--> {} ({}:{}; {})",
            human(&r.source),
            human(&r.relation),
            human(&r.label),
            human(&r.file),
            r.line,
            human(&r.reason)
        )?;
    }
    if graph.nodes.is_empty() {
        writeln!(out, "No matching symbols.")?;
    }
    if graph.truncated {
        writeln!(out, "Result truncated by query bounds.")?;
    }
    Ok(())
}

/// Write the native human-readable graph search result.
pub fn write_search(
    out: &mut impl Write,
    result: &ctx_graph_core::query::SearchResult,
) -> io::Result<()> {
    print_graph(out, &result.graph)?;
    writeln!(out, "Estimated JSON tokens: {}", result.estimated_tokens)?;
    for reason in &result.truncation_reasons {
        writeln!(out, "{}", human(reason))?;
    }
    Ok(())
}

pub(crate) fn print_output(output: Output, json: bool) -> Result<()> {
    let mut stdout = io::stdout().lock();
    if json {
        serde_json::to_writer(&mut stdout, &output)?;
        writeln!(stdout)?;
    } else {
        match &output {
            Output::Search(result) => write_search(&mut stdout, result)?,
            Output::Graph(graph) => print_graph(&mut stdout, graph)?,
            Output::SearchPath(path) => {
                writeln!(
                    stdout,
                    "{}",
                    if path.found {
                        "Path found."
                    } else if path.result.graph.truncated {
                        "Search incomplete: no path found within query bounds."
                    } else {
                        "No path found."
                    }
                )?;
                print_graph(&mut stdout, &path.result.graph)?;
                writeln!(
                    stdout,
                    "Estimated JSON tokens: {}",
                    path.result.estimated_tokens
                )?;
                for reason in &path.result.truncation_reasons {
                    writeln!(stdout, "{}", human(reason))?;
                }
            }
            Output::Path(path) => {
                writeln!(
                    stdout,
                    "{}",
                    if path.found {
                        "Path found."
                    } else if path.graph.truncated {
                        "Search incomplete: no path found within query bounds."
                    } else {
                        "No path found."
                    }
                )?;
                print_graph(&mut stdout, &path.graph)?;
            }
            Output::Stats(s) => {
                writeln!(
                    stdout,
                    "Generation {} ({})\n{} nodes, {} edges, {} files, {} unresolved references",
                    s.generation,
                    human(&s.kind),
                    s.nodes,
                    s.edges,
                    s.files,
                    s.unresolved_references
                )?;
                if let Some(root) = &s.root {
                    writeln!(stdout, "Root: {}", human(root))?;
                }
                writeln!(
                    stdout,
                    "Coverage: {} supported, {} unsupported, {} unchanged files",
                    s.coverage.supported_files,
                    s.coverage.unsupported_files,
                    s.coverage.unchanged_files
                )?;
            }
            Output::Index(r) => writeln!(
                stdout,
                "Generation {}: {} parsed, {} unchanged, {} deleted files; {} nodes, {} edges",
                r.generation, r.parsed_files, r.unchanged_files, r.deleted_files, r.nodes, r.edges
            )?,
        }
    }
    // JSON retains the full report; diagnostics never contaminate stdout as prose.
    match &output {
        Output::Index(r) => {
            diagnostics(&mut io::stderr().lock(), &r.diagnostics)?;
            if !json && let Some(t) = &r.timings {
                eprintln!(
                    "Timing (ms): detect={:.3} extract={:.3} commit={:.3} total={:.3}",
                    t.detect_ms, t.extract_ms, t.commit_ms, t.total_ms
                );
            }
        }
        Output::Stats(s) => diagnostics(&mut io::stderr().lock(), &s.diagnostics)?,
        _ => {}
    }
    Ok(())
}

pub(crate) fn print_value(value: &impl Serialize, json: bool) -> Result<()> {
    let mut out = io::stdout().lock();
    if json {
        serde_json::to_writer(&mut out, value)?;
    } else {
        serde_json::to_writer_pretty(&mut out, value)?;
    }
    writeln!(out)?;
    Ok(())
}

// Only explicit CLI/MCP memory configuration calls this helper. The full bounded
// snapshot preserves citation ambiguity and source proofs; selection stays SQL.
pub(crate) fn learning_annotations(
    memory_dir: &Path,
    graph: &GraphResult,
    snapshot: Option<&GraphSnapshot>,
    token_budget: Option<usize>,
) -> serde_json::Value {
    use ctx_graph_core::memory::{ReflectArgs, learning_overlay};
    use serde_json::json;

    let Some(snapshot) = snapshot else {
        return json!({"learning_notice":"Learning omitted: bounded snapshot unavailable."});
    };
    if snapshot.generation != graph.generation {
        return json!({"learning_notice":"Learning omitted: snapshot generation differs from query result."});
    }
    // Deliberately recompute: memory and live source files can change while the
    // indexed generation remains unchanged. This API writes neither out nor a sidecar.
    let args = ReflectArgs {
        memory_dir: memory_dir.to_owned(),
        out: PathBuf::new(),
        half_life_days: 30.0,
        min_corroboration: 2,
        if_stale: false,
    };
    let Ok(overlay) = learning_overlay(&args, Some(snapshot)) else {
        return json!({"learning_notice":"Learning omitted: memory could not be read or validated."});
    };
    let budget = token_budget.map_or(8 * 1024, |tokens| {
        tokens
            .saturating_mul(4)
            .saturating_sub(serde_json::to_vec(graph).map_or(usize::MAX, |bytes| bytes.len()))
    });
    let selected: Vec<_> = graph
        .nodes
        .iter()
        .filter_map(|node| {
            overlay
                .nodes
                .get(&node.id)
                .map(|learning| (&node.id, learning))
        })
        .collect();
    let mut annotation = json!({"learning":{
        "schema_version":overlay.schema_version,
        "snapshot_hash":overlay.snapshot_hash,
        "generated_unix_secs":overlay.generated_unix_secs,
        "status":"truncated", "omitted_nodes":selected.len(), "nodes":{}
    }});
    let fits = |value: &serde_json::Value| {
        serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= budget)
    };
    if !fits(&annotation) {
        return json!({"learning_notice":"Learning omitted: annotation budget exhausted."});
    }
    let total = selected.len();
    let mut included = 0;
    for (id, learning) in selected {
        annotation["learning"]["nodes"][id] = json!(learning);
        // Count the serialized envelope too. Keep whole entries and a stable
        // prefix of the already selected nodes; never spend their graph budget.
        if !fits(&annotation) {
            annotation["learning"]["nodes"]
                .as_object_mut()
                .unwrap()
                .remove(id);
            break;
        }
        included += 1;
    }
    annotation["learning"]["omitted_nodes"] = json!(total - included);
    if included == total {
        annotation["learning"]["status"] = json!("complete");
    } else {
        // Notices are response metadata, outside the graph-payload estimate.
        annotation["learning_notice"] = json!("Learning truncated: annotation budget exhausted.");
    }
    annotation
}

pub(crate) fn print_show_output(
    output: Output,
    annotation: serde_json::Value,
    json: bool,
) -> Result<()> {
    if json {
        let mut value = serde_json::to_value(output)?;
        value
            .as_object_mut()
            .context("show response must be an object")?
            .extend(annotation.as_object().unwrap().clone());
        return print_value(&value, true);
    }
    print_output(output, false)?;
    let mut out = io::stdout().lock();
    if let Some(nodes) = annotation["learning"]["nodes"].as_object() {
        for (id, learning) in nodes {
            writeln!(
                out,
                "Lesson {}: {} (useful={}, negative={}, verified={}, unverified={}); {}",
                human(id),
                human(learning["status"].as_str().unwrap_or("unmarked")),
                learning["useful"],
                learning["negative"],
                learning["verified_useful"],
                learning["unverified"],
                human(learning["reason"].as_str().unwrap_or(""))
            )?;
        }
    }
    if let Some(notice) = annotation["learning_notice"].as_str() {
        writeln!(out, "{}", human(notice))?;
    }
    Ok(())
}

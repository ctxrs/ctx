use super::*;

pub(crate) fn export_graph(
    source: &SourceArgs,
    db: Option<&Path>,
    format: Format,
    output: Option<&Path>,
    json_output: bool,
    view: &ViewArgs,
    report: Option<bool>,
) -> Result<()> {
    let (graph, path) = load(source, db)?;
    let graph_json = matches!(format, Format::SnapshotJson | Format::GraphifyJson);
    ensure!(
        !view.allow_shrink || graph_json,
        "--allow-shrink applies only to JSON graph exports"
    );
    let mut options = view.options(&source.analysis)?;
    if let Some(memory_dir) = &view.memory_dir {
        ensure!(
            matches!(
                format,
                Format::Html | Format::Markdown | Format::Wiki | Format::Obsidian
            ),
            "--memory-dir is supported by HTML, Markdown, wiki, and Obsidian reports"
        );
        options.learning = Some(ctx_graph_core::memory::learning_overlay(
            &ctx_graph_core::memory::ReflectArgs {
                memory_dir: memory_dir.clone(),
                out: PathBuf::new(),
                half_life_days: 30.0,
                min_corroboration: 2,
                if_stale: false,
            },
            Some(&graph),
        )?);
    }
    let context = report
        .map(|check| report_context(&graph, &path, source.snapshot.is_some(), check))
        .transpose()?
        .flatten();
    let Some(single) = format.single() else {
        let parent =
            output.context("wiki/obsidian require --output pointing to an existing directory")?;
        let vault = export::write_vault_with_options(&graph, parent, &options)?;
        let mut result = serde_json::to_value(&vault)?;
        if let Some(context) = context {
            let report = vault.directory.join("report.md");
            let content =
                contextual_report(fs::read_to_string(&report)?, Format::Markdown, &context)?;
            write_atomic(&report, content.as_bytes(), std::slice::from_ref(&path))?;
            result["source_context"] = context;
        }
        return print(&result, json_output);
    };
    let mut content = export::render_with_options(&graph, single, &options)?;
    if let Some(context) = &context {
        content = contextual_report(content, format, context)?;
    }
    if let Some(output) = output {
        if graph_json
            && destination(output, std::slice::from_ref(&path))?
            && fs::metadata(output)?.len() > 0
        {
            let previous = match format {
                Format::SnapshotJson => snapshot::read(output),
                Format::GraphifyJson => ctx_graph_core::import::read_graphify_export(output),
                _ => unreachable!(),
            }
            .context(
                "existing graph output is invalid; choose another output path to preserve it",
            )?;
            ensure!(
                view.allow_shrink
                    || (graph.nodes.len() >= previous.nodes.len()
                        && graph.edges.len() >= previous.edges.len()),
                "graph export would shrink nodes {}->{} or edges {}->{}; use --allow-shrink to accept with a backup",
                previous.nodes.len(),
                graph.nodes.len(),
                previous.edges.len(),
                graph.edges.len()
            );
        }
        write_atomic(output, content.as_bytes(), &[path])?;
        let mut result = json!({"output":output,"format":format,"bytes":content.len(),"nodes":graph.nodes.len(),"edges":graph.edges.len()});
        if let Some(context) = context {
            result["source_context"] = context;
        }
        print(&result, json_output)
    } else if json_output {
        let mut result = json!({"format":format,"content":content});
        if let Some(context) = context {
            result["source_context"] = context;
        }
        print(&result, true)
    } else {
        std::io::stdout().lock().write_all(content.as_bytes())?;
        Ok(())
    }
}

pub(crate) fn report_context(
    graph: &GraphSnapshot,
    path: &Path,
    snapshot_input: bool,
    check: bool,
) -> Result<Option<serde_json::Value>> {
    if snapshot_input || graph.kind != "native" {
        ensure!(
            !check,
            "--check-freshness requires a native database, not an imported graph or JSON snapshot"
        );
        return Ok(None);
    }
    let stats = Store::open_read_only(path)?.stats()?;
    ensure!(
        stats.generation == graph.generation,
        "graph generation changed while preparing report; retry"
    );
    let freshness = if check {
        match graph
            .root
            .as_deref()
            .context("native source root is unavailable")
            .and_then(|root| ctx_graph_core::index::check_update(Path::new(root), path))
        {
            Ok(result) => {
                ensure!(
                    result.generation == graph.generation,
                    "graph generation changed during freshness check; retry"
                );
                json!({"status":if result.fresh {"fresh"} else {"stale"},"details":result})
            }
            Err(error) => json!({"status":"unavailable","reason":error.to_string()}),
        }
    } else {
        json!({"status":"not_checked","reason":"Pass --check-freshness to compare current local source fingerprints; no live source scan was performed."})
    };
    Ok(Some(
        json!({"generation":graph.generation,"indexed_files":stats.files,"coverage":stats.coverage,
        "diagnostics":stats.diagnostics.len(),"unresolved_references":stats.unresolved_references,"freshness":freshness}),
    ))
}

pub(crate) fn contextual_report(
    mut content: String,
    format: Format,
    context: &serde_json::Value,
) -> Result<String> {
    let context = serde_json::to_string_pretty(context)?;
    match format {
        Format::Markdown => content.push_str(&format!(
            "\n## Source coverage and freshness\n\n```json\n{context}\n```\n"
        )),
        Format::Html | Format::CallflowHtml | Format::TreeHtml => {
            let escaped = context
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            let details = format!(
                "<details><summary>Source coverage and freshness</summary><pre>{escaped}</pre></details>"
            );
            if let Some(position) = content.rfind("</body>") {
                content.insert_str(position, &details);
            } else {
                content.push_str(&details);
            }
        }
        _ => (), // Interchange formats remain unchanged; context accompanies JSON CLI output.
    }
    Ok(content)
}

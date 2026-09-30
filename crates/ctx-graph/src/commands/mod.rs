//! Explicit whole-graph workflows. Registry reads use stored SQLite data only.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use crate::observation::{self, GraphObservation, GraphPhase};
use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand, ValueEnum};
use ctx_graph_core::{
    analysis::{self, AnalysisOptions},
    export::{self, ExportFormat, ExportOptions},
    model::{Direction, Edge, GraphSnapshot, ImportedGraph, QueryOptions, SCHEMA_VERSION, Stats},
    snapshot,
    store::Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod args;
use args::*;
mod files;
use files::*;
mod render;
use render::*;
mod composition;
use composition::*;
mod diagnostics;
use diagnostics::*;
mod labels;
pub use args::Command;
use labels::*;

/// Execute an explicitly selected workflow. `db` is the parent's optional global
/// --db argument; this module resolves local/default-global paths as appropriate.
pub fn run_observed(
    args: &Command,
    db: Option<&Path>,
    json_output: bool,
    facts: &mut GraphObservation,
) -> Result<()> {
    match args {
        Command::SaveResult(args) => {
            let graph = optional_graph(args.snapshot.as_deref(), db)?;
            let path = ctx_graph_core::memory::save_result(&args.input, graph.as_ref())?;
            print(&json!({"saved":path}), json_output)
        }
        Command::Reflect(args) => {
            let graph = optional_graph(args.snapshot.as_deref(), db)?;
            let result = ctx_graph_core::memory::reflect(&args.input, graph.as_ref())?;
            print(&serde_json::to_value(result)?, json_output)
        }
        Command::Prs(args) => {
            let graph = optional_graph(args.snapshot.as_deref(), db)?;
            let report = ctx_graph_core::prs::run(&args.input, graph.as_ref())?;
            facts.result_count = Some(report.entries.len() as u64);
            facts.truncated = (!report.list_may_be_truncated).then_some(false);
            facts.execution_succeeded = Some(true);
            if json_output {
                print(&report, true)
            } else {
                crate::output::stdout(format_args!(
                    "{}",
                    crate::human(&ctx_graph_core::prs::format_text(&report))
                ))?;
                Ok(())
            }
        }
        Command::Benchmark(args) => benchmark(args, db, json_output, facts),
        Command::Diagnose(args) => diagnose(args, db, json_output, facts),
        Command::Label(args) => label(args, db, json_output, facts),
        Command::Tree(args) => export_graph(
            &args.source,
            db,
            Format::TreeHtml,
            (args.output.as_deref(), json_output),
            &args.view,
            None,
            facts,
        ),
        Command::Export(args) => export_graph(
            &args.source,
            db,
            args.format,
            (args.output.as_deref(), json_output),
            &args.view,
            None,
            facts,
        ),
        Command::Report(args) => export_graph(
            &args.source,
            db,
            args.format,
            (args.output.as_deref(), json_output),
            &args.view,
            Some(args.check_freshness),
            facts,
        ),
        Command::Merge(args) => merge(args, db, json_output, facts),
        Command::Global(args) => global(args, db, json_output, facts),
        Command::Analyze(args) => {
            let (graph, source) = load(&args.source, db)?;
            let report = analyze_observed(&graph, &args.source.analysis.options(), facts)?;
            if let Some(path) = &args.output {
                let content = serde_json::to_vec_pretty(&report)?;
                facts.phase = GraphPhase::ArtifactWrite;
                write_atomic(path, &content, &[source])?;
                facts.artifact_committed = Some(true);
                facts.artifact_bytes = Some(content.len() as u64);
                facts.execution_succeeded = Some(true);
                print(
                    &json!({"output":path,"generation":report.generation,"nodes":report.nodes.len(),"communities":report.communities.len()}),
                    json_output,
                )
            } else {
                facts.execution_succeeded = Some(true);
                print(&report, json_output)
            }
        }
        Command::Communities(args) => {
            let (graph, _) = load(&args.source, db)?;
            let mut report = analyze_observed(&graph, &args.source.analysis.options(), facts)?;
            if let Some(id) = args.id {
                report.communities.retain(|c| c.id == id);
                ensure!(
                    !report.communities.is_empty(),
                    "community {id} does not exist in this snapshot"
                );
            }
            facts.result_count = Some(report.communities.len() as u64);
            facts.execution_succeeded = Some(true);
            print(
                &json!({"generation":graph.generation,"algorithm":report.community_algorithm,"modularity":report.community_modularity,"converged":report.community_converged,"communities":report.communities,"excluded_hubs":report.excluded_hubs,"noise_filtered_hubs":report.noise_filtered_hubs,"community_split_attempts":report.community_split_attempts,"unsatisfied_community_constraints":report.unsatisfied_community_constraints}),
                json_output,
            )
        }
        Command::Hubs(args) => {
            let (graph, _) = load(&args.source, db)?;
            let mut report = analyze_observed(&graph, &args.source.analysis.options(), facts)?;
            let eligible: BTreeSet<_> = report.hubs.iter().cloned().collect();
            report.nodes.retain(|n| eligible.contains(&n.id));
            report.nodes.sort_by(|a, b| match args.sort {
                HubSort::Degree => b.degree.cmp(&a.degree).then(a.id.cmp(&b.id)),
                HubSort::Pagerank => b.pagerank.total_cmp(&a.pagerank).then(a.id.cmp(&b.id)),
            });
            let nodes: BTreeMap<_, _> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
            let hubs: Vec<_> = report
                .nodes
                .iter()
                .take(args.top as usize)
                .map(|metric| json!({"node":nodes[metric.id.as_str()],"metrics":metric}))
                .collect();
            facts.result_count = Some(hubs.len() as u64);
            facts.execution_succeeded = Some(true);
            facts.truncated = Some(hubs.len() < eligible.len());
            print(
                &json!({"generation":graph.generation,"total_nodes":graph.nodes.len(),"eligible_nodes":eligible.len(),"truncated":hubs.len()<eligible.len(),"pagerank_converged":report.pagerank_converged,"hubs":hubs,"excluded_hubs":report.excluded_hubs,"noise_filtered_hubs":report.noise_filtered_hubs}),
                json_output,
            )
        }
    }
}

fn analyze_observed(
    graph: &GraphSnapshot,
    options: &AnalysisOptions,
    facts: &mut GraphObservation,
) -> Result<analysis::AnalysisReport> {
    facts.phase = GraphPhase::Analysis;
    observation::analysis_options(facts, options);
    facts.edges = Some(graph.edges.len() as u64);
    let started = Instant::now();
    let result = analysis::analyze(graph, options);
    facts.analysis_duration = Some(started.elapsed());
    if let Ok(report) = &result {
        observation::analysis(facts, report);
    }
    result
}

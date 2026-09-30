use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context, Result, bail, ensure};
use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};
use ctx_graph_core::{
    analysis::{self, AnalysisOptions, AnalysisReport, PreservedCommunity},
    export::{self, ExportFormat},
    model::{Direction, GraphSnapshot, QueryOptions},
    prs::{self, PrsArgs},
    query::{SearchOptions, SearchResult, Traversal},
    store::Store,
};
use futures_util::StreamExt;
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ClientJsonRpcMessage, ContentBlock, Implementation, ListResourcesResult,
        PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig,
        ServerJsonRpcMessage,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::{
        async_rw::JsonRpcMessageCodec,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio_util::codec::{FramedRead, FramedWrite};

use crate::{ImpactArgs, PathArgs, QueryArgs, ReadCommand, SymbolArgs};

const MAX_MESSAGE: usize = 1024 * 1024;
const MAX_PROJECTS: usize = 32;
const MAX_ANALYSIS_NODES: usize = 5_000;
const MAX_ANALYSIS_EDGES: usize = 20_000;
const MAX_ANALYSIS_BYTES: usize = 8 * 1024 * 1024;
const MAX_SNAPSHOT_NODES: usize = 100_000;
const MAX_SNAPSHOT_EDGES: usize = 1_000_000;
const DEFAULT_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

mod args;
mod observation;
mod resources;
use crate::observation::{
    self as graph_facts, GraphFailureKind, GraphInvocation, GraphObservation, GraphOperation,
    GraphPhase,
};
use observation::RequestObservation;
mod transport;
pub use args::ServeArgs;
use args::*;
pub use transport::serve_observed;

struct CachedAnalysisFailure {
    kind: GraphFailureKind,
    message: String,
}

struct CachedSnapshot {
    snapshot: GraphSnapshot,
    preserved: Vec<PreservedCommunity>,
    analysis: OnceLock<std::result::Result<AnalysisReport, CachedAnalysisFailure>>,
    report: OnceLock<std::result::Result<String, String>>,
}

impl CachedSnapshot {
    fn check_analysis_limits(&self, facts: &mut GraphObservation) -> Result<()> {
        let references = self
            .snapshot
            .metadata
            .get("graf_unresolved_references")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if self.snapshot.nodes.len() > MAX_ANALYSIS_NODES
            || self.snapshot.edges.len() > MAX_ANALYSIS_EDGES
            || references > MAX_ANALYSIS_EDGES
        {
            facts.fail(GraphFailureKind::WorkLimit);
        }
        ensure!(
            self.snapshot.nodes.len() <= MAX_ANALYSIS_NODES
                && self.snapshot.edges.len() <= MAX_ANALYSIS_EDGES
                && references <= MAX_ANALYSIS_EDGES,
            "MCP analysis limit is 5000 nodes, 20000 edges and 20000 unresolved references; preserved community reads do not require analysis"
        );
        let fits = serde_json::to_vec(&self.snapshot)?.len() <= MAX_ANALYSIS_BYTES;
        if !fits {
            facts.fail(GraphFailureKind::WorkLimit);
        }
        ensure!(
            fits,
            "MCP analysis snapshot limit is 8 MiB; preserved community reads do not require analysis"
        );
        Ok(())
    }

    fn analysis(&self, facts: &mut GraphObservation) -> Result<&AnalysisReport> {
        facts.phase = GraphPhase::Analysis;
        graph_facts::analysis_options(facts, &AnalysisOptions::default());
        facts.analysis_cache_hit = Some(self.analysis.get().is_some());
        let started = std::time::Instant::now();
        let result = self
            .analysis
            .get_or_init(|| {
                self.check_analysis_limits(facts)
                    .and_then(|()| analysis::analyze(&self.snapshot, &AnalysisOptions::default()))
                    .map_err(|error| CachedAnalysisFailure {
                        kind: facts
                            .failure
                            .map(|failure| failure.kind)
                            .unwrap_or_else(|| graph_facts::failure_kind(&error)),
                        message: format!("{error:#}"),
                    })
            })
            .as_ref()
            .map_err(|error| {
                facts.fail(error.kind);
                anyhow::anyhow!(error.message.clone())
            });
        facts.analysis_duration = Some(started.elapsed());
        if let Ok(report) = &result {
            graph_facts::analysis(facts, report);
        }
        result
    }
}

struct Project {
    db: PathBuf,
    memory_dir: Option<PathBuf>,
    snapshot_max_bytes: usize,
    cache: Mutex<Option<Arc<CachedSnapshot>>>,
}

impl Project {
    fn learned_result(
        &self,
        result: SearchResult,
        budget: Option<usize>,
        facts: &mut GraphObservation,
    ) -> Result<Value> {
        graph_facts::search(facts, &result);
        let mut value = serde_json::to_value(&result)?;
        if let Some(memory_dir) = &self.memory_dir {
            let mut learning_facts = *facts;
            let cached = self.snapshot(&mut learning_facts).ok();
            facts.snapshot_cache_hit = learning_facts.snapshot_cache_hit;
            let annotation = crate::learning_annotations(
                memory_dir,
                &result.graph,
                cached.as_ref().map(|cache| &cache.snapshot),
                budget,
            );
            value
                .as_object_mut()
                .unwrap()
                .extend(annotation.as_object().unwrap().clone());
        }
        Ok(value)
    }

    fn snapshot(&self, facts: &mut GraphObservation) -> Result<Arc<CachedSnapshot>> {
        facts.phase = GraphPhase::Snapshot;
        // SQL queries load this only for explicitly configured learning annotations.
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("snapshot worker failed"))?;
        let store = Store::open_read_only(&self.db)?;
        let stats = store.stats()?;
        facts.stats(&stats);
        if let Some(value) = cache
            .as_ref()
            .filter(|v| v.snapshot.generation == stats.generation)
        {
            facts.snapshot_cache_hit = Some(true);
            return Ok(value.clone());
        }
        facts.snapshot_cache_hit = Some(false);
        // Store checks record/payload limits in the same transaction before loading.
        // Index size on disk is not a proxy for the in-memory graph payload.
        let snapshot = store.snapshot_bounded(
            MAX_SNAPSHOT_NODES,
            MAX_SNAPSHOT_EDGES,
            MAX_SNAPSHOT_EDGES,
            self.snapshot_max_bytes,
        )?;
        let preserved = analysis::preserved_communities(&snapshot);
        let value = Arc::new(CachedSnapshot {
            snapshot,
            preserved,
            analysis: OnceLock::new(),
            report: OnceLock::new(),
        });
        *cache = Some(value.clone());
        Ok(value)
    }
}

// CLI explain uses the same default bounds, without a persistent server cache.
pub(super) fn snapshot_for_learning(db: &Path) -> Result<GraphSnapshot> {
    Store::open_read_only(db)?.snapshot_bounded(
        MAX_SNAPSHOT_NODES,
        MAX_SNAPSHOT_EDGES,
        MAX_SNAPSHOT_EDGES,
        DEFAULT_SNAPSHOT_BYTES,
    )
}

#[derive(Clone)]
struct Graf {
    projects: Arc<BTreeMap<String, Arc<Project>>>,
    github_repo: Option<String>,
    workers: Arc<Semaphore>,
    tool_router: ToolRouter<Self>,
    observation: Option<RequestObservation>,
}

impl Graf {
    async fn inspect_prs(
        &self,
        project: Option<String>,
        mut args: PrsArgs,
        include_graph: bool,
    ) -> CallToolResult {
        let configured = (|| -> Result<String> {
            let repo = self.github_repo.as_ref().context(
                "PR tools are disabled; configure serve --github-repo OWNER/REPO explicitly",
            )?;
            if let Some(requested) = &args.repo {
                prs::validate_repo(requested)?;
                ensure!(
                    requested.eq_ignore_ascii_case(repo),
                    "repo is not the configured GitHub repository"
                );
            }
            Ok(repo.clone())
        })();
        let repo = match configured {
            Ok(repo) => repo,
            Err(error) => return self.tool_result(Err(error)),
        };
        // A request can never select the working directory or infer its origin.
        args.repo = Some(repo);
        self.tool_result(
            self.run(project, move |p, facts| {
                let derived = if include_graph {
                    Some(p.snapshot(facts)?)
                } else {
                    None
                };
                let report = prs::run(&args, derived.as_ref().map(|d| &d.snapshot))?;
                facts.result_count = Some(report.entries.len() as u64);
                facts.truncated = (!report.list_may_be_truncated).then_some(false);
                Ok(serde_json::to_value(report)?)
            })
            .await,
        )
    }

    async fn run<T: Send + 'static>(
        &self,
        name: Option<String>,
        operation: impl FnOnce(&Project, &mut GraphObservation) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let mut facts = self
            .observation
            .as_ref()
            .and_then(RequestObservation::snapshot)
            .unwrap_or_else(|| {
                GraphObservation::new(GraphOperation::Protocol, GraphInvocation::Library)
            });
        facts.phase = GraphPhase::Admission;
        let result = async {
            let project = self.projects.get(name.as_deref().unwrap_or("default"));
            if project.is_none() {
                facts.fail(GraphFailureKind::UnknownProject);
            }
            let project = project
                .context("unknown project; use a name registered with --project NAME=DB")?
                .clone();
            let permit = self.workers.clone().try_acquire_owned();
            if permit.is_err() {
                facts.fail(GraphFailureKind::Capacity);
            }
            let permit = permit.context(
                "MCP query capacity reached (4 workers); retry after an active query completes",
            )?;
            let mut worker_facts = facts;
            let joined = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let started = std::time::Instant::now();
                worker_facts.phase = GraphPhase::Query;
                let result = operation(&project, &mut worker_facts);
                if worker_facts.query_duration.is_none() {
                    worker_facts.query_duration = Some(started.elapsed());
                }
                match &result {
                    Ok(_) => worker_facts.execution_succeeded = Some(true),
                    Err(error) => graph_facts::failed(&mut worker_facts, error),
                }
                (result, worker_facts)
            })
            .await;
            match joined {
                Ok((result, observed)) => {
                    facts = observed;
                    result
                }
                Err(error) => {
                    facts.phase = GraphPhase::Worker;
                    facts.fail(GraphFailureKind::Worker);
                    Err(error.into())
                }
            }
        }
        .await;
        if let Some(observation) = &self.observation {
            observation.update(|value| *value = facts);
        }
        result
    }

    async fn execute(&self, name: Option<String>, command: ReadCommand) -> CallToolResult {
        self.tool_result(
            self.run(name, move |p, facts| {
                Ok(serde_json::to_value(crate::read::read_observed(
                    &p.db, command, facts,
                )?)?)
            })
            .await,
        )
    }
}

fn validate_tokens(budget: usize) -> Result<()> {
    ensure!(
        (1..=100_000).contains(&budget),
        "token_budget must be between 1 and 100000"
    );
    Ok(())
}

impl Graf {
    fn tool_result(&self, result: Result<Value>) -> CallToolResult {
        match result.and_then(|value| {
            let fits = serde_json::to_vec(&value)?.len() <= MAX_MESSAGE / 2;
            if !fits && let Some(observation) = &self.observation {
                observation.update(|f| {
                    f.phase = GraphPhase::Render;
                    f.fail(GraphFailureKind::ResponseLimit);
                });
            }
            ensure!(
                fits,
                "tool response exceeds 512 KiB; reduce the result limit or use CLI export"
            );
            Ok(value)
        }) {
            Ok(value) => CallToolResult::structured(value),
            Err(error) => {
                if let Some(observation) = &self.observation {
                    observation.update(|facts| {
                        if facts.execution_succeeded == Some(true) {
                            facts.phase = GraphPhase::Render;
                        }
                        graph_facts::failed(facts, &error);
                    });
                }
                CallToolResult::error(vec![ContentBlock::text(format!("{error:#}"))])
            }
        }
    }

    fn learning_tool_result(&self, result: Result<Value>) -> CallToolResult {
        let mut external_notice = None;
        let result = result.and_then(|mut value| {
            if (value.get("learning").is_some() || value.get("learning_notice").is_some())
                && serde_json::to_vec(&value)?.len() > MAX_MESSAGE / 2
            {
                let fields = value.as_object_mut().unwrap();
                fields.remove("learning");
                fields.insert(
                    "learning_notice".into(),
                    json!("Learning omitted: response size limit."),
                );
                if serde_json::to_vec(&value)?.len() > MAX_MESSAGE / 2 {
                    external_notice = value.as_object_mut().unwrap().remove("learning_notice");
                }
            }
            Ok(value)
        });
        let mut response = self.tool_result(result);
        if let Some(notice) = external_notice.and_then(|value| value.as_str().map(str::to_owned)) {
            response.content.push(ContentBlock::text(notice));
        }
        response
    }
}

fn graph_stats(derived: &CachedSnapshot, facts: &mut GraphObservation) -> Result<Value> {
    let a = derived.analysis(facts)?;
    let total = derived.snapshot.edges.len();
    let percentages: BTreeMap<_, _> = a
        .confidence_counts
        .iter()
        .map(|(key, count)| {
            (
                key,
                if total == 0 {
                    0.0
                } else {
                    *count as f64 * 100.0 / total as f64
                },
            )
        })
        .collect();
    Ok(
        json!({"schema_version": a.schema_version, "generation": a.generation,
        "nodes": derived.snapshot.nodes.len(), "edges": total,
        "communities": a.communities.len(), "confidence_counts": a.confidence_counts,
        "confidence_percentages": percentages, "methodology": a.methodology}),
    )
}

fn hubs(
    derived: &CachedSnapshot,
    top: usize,
    percentile: Option<f64>,
    facts: &mut GraphObservation,
) -> Result<Value> {
    ensure!((1..=500).contains(&top), "top_n must be between 1 and 500");
    let metrics = &derived.analysis(facts)?.nodes;
    let cutoff = if let Some(p) = percentile {
        ensure!(
            p.is_finite() && (0.0..=100.0).contains(&p),
            "percentile must be between 0 and 100"
        );
        let mut degrees: Vec<_> = metrics.iter().map(|n| n.degree).collect();
        degrees.sort_unstable();
        if degrees.is_empty() {
            0.0
        } else {
            let index = ((degrees.len() as f64 * p / 100.0) as usize).saturating_sub(1);
            degrees[index] as f64
        }
    } else {
        f64::INFINITY
    };
    let mut nodes: Vec<_> = metrics
        .iter()
        .filter(|n| n.degree as f64 <= cutoff)
        .collect();
    nodes.sort_by(|a, b| b.degree.cmp(&a.degree).then(a.id.cmp(&b.id)));
    let truncated = nodes.len() > top;
    nodes.truncate(top);
    facts.result_count = Some(nodes.len() as u64);
    facts.truncated = Some(truncated);
    Ok(json!({"schema_version": derived.snapshot.schema_version,
        "generation": derived.snapshot.generation, "nodes": nodes, "truncated": truncated,
        "methodology": "Edge incidences; parallel edges counted separately, self edges count twice. Degree is not proof of architectural importance."}))
}

#[tool_router]
impl Graf {
    #[tool(
        description = "Explicit read-only GitHub call: list up to 50 open PRs from the configured --github-repo. Optional repo must match that repository. base defaults to GitHub's actual default. Requires installed/authenticated gh. Shared 30s command budget, 8 MiB per output stream, 3000 files per PR; inspect completeness fields. No local worktree inspection or model calls.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn list_prs(&self, Parameters(a): Parameters<PrListArgs>) -> CallToolResult {
        self.inspect_prs(
            a.project,
            PrsArgs {
                repo: a.repo,
                base: a.base,
                ..Default::default()
            },
            false,
        )
        .await
    }

    #[tool(
        description = "Explicit read-only GitHub call for one PR in the configured repository, including closed/merged PRs. Returns changed files and direct node/computed-community impact in a registered local snapshot; ambiguous paths remain explicit. Same 30s/8 MiB/3000-file backend bounds as list_prs; snapshot limits apply; recorded communities need no clustering. No model or worktree calls.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_pr_impact(&self, Parameters(a): Parameters<PrImpactArgs>) -> CallToolResult {
        self.inspect_prs(
            a.project,
            PrsArgs {
                repo: a.repo,
                number: Some(a.pr_number),
                ..Default::default()
            },
            true,
        )
        .await
    }

    #[tool(
        description = "Explicit read-only GitHub call: deterministic review queue for up to 50 open PRs from the configured repository, with reasons, direct graph impact and file/community overlaps. Same bounds as list_prs plus bounded snapshot limits. An overlap is not a merge-conflict prediction; no model or worktree calls.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn triage_prs(&self, Parameters(a): Parameters<PrListArgs>) -> CallToolResult {
        self.inspect_prs(
            a.project,
            PrsArgs {
                repo: a.repo,
                base: a.base,
                triage: true,
                ..Default::default()
            },
            true,
        )
        .await
    }

    #[tool(
        description = "Search using bounded SQL BFS/DFS over the indexed snapshot. question, mode, depth, token_budget and context_filter follow Graphify naming. Also accepts exact file/kind filters and induced_edges. Complete ranking requires at most 250000 posting matches across terms, 64 MiB total candidate payloads and 8 MiB per record; broader queries must be refined. project selects a registered alias. Startup --memory-dir adds fresh selected-node learning only for default, within remaining graph-payload budget; omission notices are metadata. No query logs, refresh or network calls; token counts are estimates.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn query_graph(&self, Parameters(a): Parameters<GraphQueryArgs>) -> CallToolResult {
        self.learning_tool_result(
            self.run(a.project, move |p, facts| {
                crate::nonempty(&a.question, "question")?;
                validate_tokens(a.token_budget)?;
                let options = SearchOptions {
                    graph: QueryOptions {
                        depth: a.depth,
                        limit: a.limit,
                        direction: Direction::Both,
                        relation: None,
                    },
                    traversal: match a.mode {
                        QueryMode::Bfs => Traversal::Bfs,
                        QueryMode::Dfs => Traversal::Dfs,
                    },
                    contexts: a.context_filter,
                    files: a.files,
                    kinds: a.kinds,
                    token_budget: Some(a.token_budget),
                    induced_edges: a.induced_edges,
                    infer_context: true,
                };
                p.learned_result(
                    Store::open_read_only(&p.db)?.query_extended(&a.question, &options)?,
                    Some(a.token_budget),
                    facts,
                )
            })
            .await,
        )
    }
    #[tool(
        description = "Get a node by exact ID/label or file::symbol, then unique Unicode/accent-insensitive exact, prefix or substring. Ambiguity and incomplete convenience lookup are errors. project is a registered alias. Startup --memory-dir adds fresh learning only for default, capped at 8 KiB, without changing selected nodes; omissions are explicit.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_node(&self, Parameters(a): Parameters<NodeArgs>) -> CallToolResult {
        self.learning_tool_result(
            self.run(a.project, move |p, facts| {
                crate::nonempty(&a.label, "label")?;
                let options = SearchOptions {
                    graph: QueryOptions {
                        depth: 0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                p.learned_result(
                    Store::open_read_only(&p.db)?.neighbors_resolved(&a.label, &options)?,
                    None,
                    facts,
                )
            })
            .await,
        )
    }
    #[tool(
        description = "Immediate neighbors in both directions. Same exact-first Unicode/accent-insensitive endpoint lookup as get_node. relation_filter uses exact, then unique normalized prefix/substring among incident relations; ties are errors. Estimated token_budget; default limit 100, maximum 500; truncation is explicit. Startup --memory-dir adds fresh selected-node learning only for default within remaining graph-payload budget; omission notices are metadata.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_neighbors(&self, Parameters(a): Parameters<NeighborArgs>) -> CallToolResult {
        self.learning_tool_result(
            self.run(a.project, move |p, facts| {
                crate::nonempty(&a.label, "label")?;
                validate_tokens(a.token_budget)?;
                let options = SearchOptions {
                    graph: QueryOptions {
                        depth: 1,
                        limit: a.limit,
                        direction: Direction::Both,
                        relation: a.relation_filter,
                    },
                    token_budget: Some(a.token_budget),
                    ..Default::default()
                };
                p.learned_result(
                    Store::open_read_only(&p.db)?.neighbors_resolved(&a.label, &options)?,
                    Some(a.token_budget),
                    facts,
                )
            })
            .await,
        )
    }
    #[tool(
        description = "Bounded shortest path following stored direction unless undirected=true. max_hops defaults to six and is capped at six. Exact endpoints support file::symbol. Inspect found and result.graph.truncated together.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn shortest_path(&self, Parameters(a): Parameters<ShortestPathArgs>) -> CallToolResult {
        self.tool_result(
            self.run(a.project, move |p, facts| {
                crate::nonempty(&a.source, "source")?;
                crate::nonempty(&a.target, "target")?;
                validate_tokens(a.token_budget)?;
                let options = SearchOptions {
                    graph: QueryOptions {
                        depth: a.max_hops,
                        limit: a.limit,
                        direction: if a.undirected {
                            Direction::Both
                        } else {
                            Direction::Outgoing
                        },
                        relation: None,
                    },
                    token_budget: Some(a.token_budget),
                    ..Default::default()
                };
                let result =
                    Store::open_read_only(&p.db)?.path_extended(&a.source, &a.target, &options)?;
                graph_facts::path(facts, &result);
                Ok(serde_json::to_value(result)?)
            })
            .await,
        )
    }

    #[tool(
        description = "Search the indexed snapshot with bounded traversal. No source reads or refresh. Optional project is a registered name.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn query(&self, Parameters(a): Parameters<Routed<QueryArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Query(a.args)).await
    }
    #[tool(
        description = "Show an exact node ID or unique symbol plus depth-one neighbors in both directions. Ambiguous names are errors.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn show(&self, Parameters(a): Parameters<Routed<SymbolArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Show(a.args)).await
    }
    #[tool(
        description = "Show immediate incoming calls in the indexed snapshot.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn callers(&self, Parameters(a): Parameters<Routed<SymbolArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Callers(a.args)).await
    }
    #[tool(
        description = "Show immediate outgoing calls, including bounded unresolved references.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn callees(&self, Parameters(a): Parameters<Routed<SymbolArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Callees(a.args)).await
    }
    #[tool(
        description = "Follow incoming calls for potential impact. Reachability is not proof of runtime behavior.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn impact(&self, Parameters(a): Parameters<Routed<ImpactArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Impact(a.args)).await
    }
    #[tool(
        description = "Find a bounded path, outgoing by default. found=false with graph.truncated=true is an incomplete search. Snapshot generation is in graph.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn path(&self, Parameters(a): Parameters<Routed<PathArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Path(a.args)).await
    }
    #[tool(
        description = "Counts, generation, coverage and diagnostics from the configured snapshot; no live freshness check or full graph analysis.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn stats(&self, Parameters(a): Parameters<ProjectArgs>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Stats).await
    }
    #[tool(
        description = "Node, edge and computed community counts plus confidence counts and percentages. Cached per indexed generation; maximum 5000 nodes/20000 edges/20000 unresolved references, 8 MiB snapshot.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn graph_stats(&self, Parameters(a): Parameters<ProjectArgs>) -> CallToolResult {
        self.tool_result(
            self.run(a.project, |p, facts| {
                graph_stats(p.snapshot(facts)?.as_ref(), facts)
            })
            .await,
        )
    }
    #[tool(
        description = "Highest-degree nodes, optional percentile exclusion. Default top_n 10, maximum 500. Cached analysis capped at 5000 nodes/20000 edges/20000 unresolved references, 8 MiB snapshot.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn god_nodes(&self, Parameters(a): Parameters<HubArgs>) -> CallToolResult {
        self.tool_result(
            self.run(a.project, move |p, facts| {
                hubs(
                    p.snapshot(facts)?.as_ref(),
                    a.top_n,
                    a.exclude_hubs_percentile,
                    facts,
                )
            })
            .await,
        )
    }
    #[tool(
        description = "Nodes in a community. community_source auto uses preserved imported IDs when available, otherwise computed structural IDs; computed explicitly selects analysis. Integer and string IDs stay distinct. community_project selects an exact composition path from the communities resource; ambiguous paths error. Default limit 100, maximum 500. Preserved lookup uses the bounded snapshot cache without clustering; computed lookup retains the 5000-node analysis cap.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_community(&self, Parameters(a): Parameters<CommunityArgs>) -> CallToolResult {
        self.tool_result(self.run(a.project, move |p, facts| {
            ensure!((1..=500).contains(&a.limit), "limit must be between 1 and 500");
            let d = p.snapshot(facts)?;
            validate_tokens(a.token_budget)?;
            let requested = a.community_id.value();
            ensure!(requested.as_str().is_none_or(|s| !s.is_empty() && s.len() <= 1024), "community_id string must be nonempty and at most 1024 bytes");
            if let Some(path) = &a.community_project {
                ensure!(path.len() <= 32 && path.iter().all(|s| !s.is_empty() && s.len() <= 1024), "invalid community_project composition path");
            }
            let preserved = match a.community_source {
                CommunitySource::Auto => !d.preserved.is_empty(),
                CommunitySource::Preserved => true,
                CommunitySource::Computed => false,
            };
            let (ids, details) = if preserved {
                let matches: Vec<_> = d.preserved.iter().filter(|c| c.id == requested
                    && a.community_project.as_ref().is_none_or(|p| p == &c.project)).collect();
                ensure!(matches.len() <= 1, "ambiguous community_id across composition paths; supply community_project from the communities resource");
                let community = matches.first().context("unknown preserved community_id; read the communities resource for this generation")?;
                (&community.nodes, json!({"community_source":"preserved", "community_project":community.project, "community_names":community.names}))
            } else {
                ensure!(a.community_project.is_none(), "community_project applies only to preserved communities");
                let id = requested.as_u64().and_then(|n| usize::try_from(n).ok()).context("computed community_id must be a nonnegative integer")?;
                let community = d.analysis(facts)?.communities.iter().find(|c| c.id == id)
                    .context("unknown computed community_id; read the communities resource for this generation")?;
                (&community.nodes, json!({"community_source":"computed", "cohesion":community.cohesion}))
            };
            let mut output = json!({"schema_version": d.snapshot.schema_version, "generation": d.snapshot.generation,
                "community_id": requested,
                "total_nodes": ids.len(), "nodes": [], "truncated": false,
                "token_estimate": "ceil(UTF-8 JSON bytes / 4), not a model tokenizer"});
            output.as_object_mut().unwrap().extend(details.as_object().unwrap().clone());
            ensure!(serde_json::to_vec(&output)?.len() <= a.token_budget * 4,
                "token budget cannot hold the response envelope");
            for id in ids.iter().take(a.limit) {
                let index = d.snapshot.nodes.binary_search_by(|node| node.id.cmp(id)).ok().context("community node is missing")?;
                let node = &d.snapshot.nodes[index];
                output["nodes"].as_array_mut().unwrap().push(serde_json::to_value(node)?);
                if serde_json::to_vec(&output)?.len() > a.token_budget * 4 {
                    output["nodes"].as_array_mut().unwrap().pop();
                    break;
                }
            }
            let returned = output["nodes"].as_array().unwrap().len();
            facts.result_count = Some(returned as u64);
            facts.truncated = Some(returned < ids.len());
            output["truncated"] = json!(returned < ids.len());
            Ok(output)
        }).await)
    }
}

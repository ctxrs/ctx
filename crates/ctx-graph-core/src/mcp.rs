use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context, Result, bail, ensure};
use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};
use futures_util::StreamExt;
use crate::{
    analysis::{self, AnalysisOptions, AnalysisReport, PreservedCommunity},
    export::{self, ExportFormat},
    model::{Direction, GraphSnapshot, QueryOptions},
    prs::{self, PrsArgs},
    query::{SearchOptions, SearchResult, Traversal},
    store::Store,
};
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

use crate::{ImpactArgs, PathArgs, QueryArgs, ReadCommand, SymbolArgs, read};

const MAX_MESSAGE: usize = 1024 * 1024;
const MAX_PROJECTS: usize = 32;
const MAX_ANALYSIS_NODES: usize = 5_000;
const MAX_ANALYSIS_EDGES: usize = 20_000;
const MAX_ANALYSIS_BYTES: usize = 8 * 1024 * 1024;
const MAX_SNAPSHOT_NODES: usize = 100_000;
const MAX_SNAPSHOT_EDGES: usize = 1_000_000;
const DEFAULT_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum, PartialEq, Eq)]
pub enum Transport {
    #[default]
    Stdio,
    Http,
}

#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    #[arg(long, value_enum, default_value = "stdio")]
    pub transport: Transport,
    /// HTTP bind address. Nonloopback listeners require --bearer-token-env.
    #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    pub host: IpAddr,
    #[arg(long, default_value_t = 8080)]
    pub port: u16,
    #[arg(long, default_value = "/mcp")]
    pub path: String,
    /// Name of an environment variable containing a bearer token (never the token itself).
    #[arg(long)]
    pub bearer_token_env: Option<String>,
    /// Additional exact HTTP Host authorities, e.g. example.org:8080. No wildcards.
    #[arg(long)]
    pub allowed_host: Vec<String>,
    /// Register another read-only database as NAME=DB. Tools select the name with project.
    #[arg(long, value_name = "NAME=DB")]
    pub project: Vec<String>,
    /// Enable explicit read-only GitHub PR tools for this single OWNER/REPO.
    #[arg(long, value_name = "OWNER/REPO")]
    pub github_repo: Option<String>,
    /// Fresh read-only learning annotations for the default database only.
    /// Registered named projects never read this directory. Only query_graph,
    /// get_node and get_neighbors annotate; no analysis or graph writes occur.
    #[arg(long)]
    pub memory_dir: Option<PathBuf>,
    /// Maximum stored payload bytes per cached snapshot (1..268435456).
    /// Preserved memberships do not require structural analysis.
    #[arg(long, default_value_t = DEFAULT_SNAPSHOT_BYTES)]
    pub snapshot_max_bytes: usize,
}

#[derive(Deserialize, JsonSchema)]
struct Routed<T> {
    /// Registered project name; omitted selects default. Filesystem paths are not accepted.
    project: Option<String>,
    #[serde(flatten)]
    args: T,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProjectArgs {
    /// Registered project name; omitted selects default.
    project: Option<String>,
}

fn ten() -> usize {
    10
}
fn hundred() -> usize {
    100
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HubArgs {
    project: Option<String>,
    #[serde(default = "ten")]
    #[schemars(range(min = 1, max = 500))]
    top_n: usize,
    /// Suppress degrees above the empirical degree percentile, 0..100.
    #[schemars(range(min = 0, max = 100))]
    exclude_hubs_percentile: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CommunityArgs {
    project: Option<String>,
    community_id: CommunityId,
    /// Auto uses preserved memberships when available, otherwise computed communities.
    #[serde(default)]
    community_source: CommunitySource,
    /// Composition path from the communities resource; distinct from registered project.
    community_project: Option<Vec<String>>,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    token_budget: usize,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: usize,
}

#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
enum CommunityId {
    Integer(i64),
    Unsigned(u64),
    Text(String),
}
impl CommunityId {
    fn value(&self) -> Value {
        match self {
            Self::Integer(id) => json!(id),
            Self::Unsigned(id) => json!(id),
            Self::Text(id) => json!(id),
        }
    }
}

#[derive(Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
enum CommunitySource {
    #[default]
    Auto,
    Preserved,
    Computed,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PrListArgs {
    project: Option<String>,
    /// Must match the repository configured with --github-repo; omitted selects it.
    repo: Option<String>,
    /// Expected base; omitted asks GitHub for the actual default branch.
    base: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PrImpactArgs {
    project: Option<String>,
    repo: Option<String>,
    #[schemars(range(min = 1, max = 2147483647))]
    pr_number: u64,
}

fn three() -> u32 {
    3
}
fn six() -> u32 {
    6
}
fn token_budget() -> usize {
    2000
}

#[derive(Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
enum QueryMode {
    #[default]
    Bfs,
    Dfs,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphQueryArgs {
    project: Option<String>,
    question: String,
    #[serde(default)]
    mode: QueryMode,
    #[serde(default = "three")]
    #[schemars(range(min = 0, max = 6))]
    depth: u32,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: usize,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    token_budget: usize,
    #[serde(default)]
    context_filter: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    kinds: Vec<String>,
    #[serde(default)]
    induced_edges: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NodeArgs {
    project: Option<String>,
    /// Exact ID/label or file::symbol; otherwise a unique normalized prefix/substring.
    label: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NeighborArgs {
    project: Option<String>,
    label: String,
    /// Exact relation, or a unique normalized prefix/substring among incident relations.
    relation_filter: Option<String>,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    token_budget: usize,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: usize,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShortestPathArgs {
    project: Option<String>,
    source: String,
    target: String,
    /// Graf bounds path searches to six hops; default six.
    #[serde(default = "six")]
    #[schemars(range(min = 0, max = 6))]
    max_hops: u32,
    #[serde(default)]
    undirected: bool,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: usize,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    token_budget: usize,
}

struct CachedSnapshot {
    snapshot: GraphSnapshot,
    preserved: Vec<PreservedCommunity>,
    analysis: OnceLock<std::result::Result<AnalysisReport, String>>,
    report: OnceLock<std::result::Result<String, String>>,
}

impl CachedSnapshot {
    fn check_analysis_limits(&self) -> Result<()> {
        let references = self
            .snapshot
            .metadata
            .get("graf_unresolved_references")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        ensure!(
            self.snapshot.nodes.len() <= MAX_ANALYSIS_NODES
                && self.snapshot.edges.len() <= MAX_ANALYSIS_EDGES
                && references <= MAX_ANALYSIS_EDGES,
            "MCP analysis limit is 5000 nodes, 20000 edges and 20000 unresolved references; preserved community reads do not require analysis"
        );
        ensure!(
            serde_json::to_vec(&self.snapshot)?.len() <= MAX_ANALYSIS_BYTES,
            "MCP analysis snapshot limit is 8 MiB; preserved community reads do not require analysis"
        );
        Ok(())
    }

    fn analysis(&self) -> Result<&AnalysisReport> {
        self.analysis
            .get_or_init(|| {
                self.check_analysis_limits()
                    .and_then(|()| analysis::analyze(&self.snapshot, &AnalysisOptions::default()))
                    .map_err(|error| format!("{error:#}"))
            })
            .as_ref()
            .map_err(|error| anyhow::anyhow!(error.clone()))
    }
}

struct Project {
    db: PathBuf,
    memory_dir: Option<PathBuf>,
    snapshot_max_bytes: usize,
    cache: Mutex<Option<Arc<CachedSnapshot>>>,
}

impl Project {
    fn learned_result(&self, result: SearchResult, budget: Option<usize>) -> Result<Value> {
        let mut value = serde_json::to_value(&result)?;
        if let Some(memory_dir) = &self.memory_dir {
            let cached = self.snapshot().ok();
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

    fn snapshot(&self) -> Result<Arc<CachedSnapshot>> {
        // SQL queries load this only for explicitly configured learning annotations.
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("snapshot worker failed"))?;
        let store = Store::open_read_only(&self.db)?;
        let stats = store.stats()?;
        if let Some(value) = cache
            .as_ref()
            .filter(|v| v.snapshot.generation == stats.generation)
        {
            return Ok(value.clone());
        }
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
            Err(error) => return tool_result(Err(error)),
        };
        // A request can never select the working directory or infer its origin.
        args.repo = Some(repo);
        tool_result(
            self.run(project, move |p| {
                let derived = if include_graph {
                    Some(p.snapshot()?)
                } else {
                    None
                };
                let report = prs::run(&args, derived.as_ref().map(|d| &d.snapshot))?;
                Ok(serde_json::to_value(report)?)
            })
            .await,
        )
    }

    async fn run<T: Send + 'static>(
        &self,
        name: Option<String>,
        operation: impl FnOnce(&Project) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let project = self
            .projects
            .get(name.as_deref().unwrap_or("default"))
            .context("unknown project; use a name registered with --project NAME=DB")?
            .clone();
        let permit = self.workers.clone().try_acquire_owned().context(
            "MCP query capacity reached (4 workers); retry after an active query completes",
        )?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation(&project)
        })
        .await
        .context("query worker failed")?
    }

    async fn execute(&self, name: Option<String>, command: ReadCommand) -> CallToolResult {
        tool_result(
            self.run(name, move |p| {
                Ok(serde_json::to_value(read(&p.db, command)?)?)
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

fn tool_result(result: Result<Value>) -> CallToolResult {
    match result.and_then(|value| {
        ensure!(
            serde_json::to_vec(&value)?.len() <= MAX_MESSAGE / 2,
            "tool response exceeds 512 KiB; reduce the result limit or use CLI export"
        );
        Ok(value)
    }) {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => CallToolResult::error(vec![ContentBlock::text(format!("{error:#}"))]),
    }
}

fn learning_tool_result(result: Result<Value>) -> CallToolResult {
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
    let mut response = tool_result(result);
    if let Some(notice) = external_notice.and_then(|value| value.as_str().map(str::to_owned)) {
        response.content.push(ContentBlock::text(notice));
    }
    response
}

fn graph_stats(derived: &CachedSnapshot) -> Result<Value> {
    let a = derived.analysis()?;
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

fn hubs(derived: &CachedSnapshot, top: usize, percentile: Option<f64>) -> Result<Value> {
    ensure!((1..=500).contains(&top), "top_n must be between 1 and 500");
    let metrics = &derived.analysis()?.nodes;
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
        learning_tool_result(
            self.run(a.project, move |p| {
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
        learning_tool_result(
            self.run(a.project, move |p| {
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
        learning_tool_result(
            self.run(a.project, move |p| {
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
        tool_result(
            self.run(a.project, move |p| {
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
                Ok(serde_json::to_value(
                    Store::open_read_only(&p.db)?.path_extended(&a.source, &a.target, &options)?,
                )?)
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
        tool_result(
            self.run(a.project, |p| graph_stats(p.snapshot()?.as_ref()))
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
        tool_result(
            self.run(a.project, move |p| {
                hubs(p.snapshot()?.as_ref(), a.top_n, a.exclude_hubs_percentile)
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
        tool_result(self.run(a.project, move |p| {
            ensure!((1..=500).contains(&a.limit), "limit must be between 1 and 500");
            let d = p.snapshot()?;
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
                let community = d.analysis()?.communities.iter().find(|c| c.id == id)
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
            output["truncated"] = json!(output["nodes"].as_array().unwrap().len() < ids.len());
            Ok(output)
        }).await)
    }
}

const RESOURCES: &[(&str, &str)] = &[
    ("stats", "Snapshot counts and diagnostics"),
    ("graph", "Complete snapshot JSON, maximum 1 MiB"),
    (
        "report",
        "Offline structural Markdown report, maximum 1 MiB",
    ),
    ("god-nodes", "Ten highest-degree nodes"),
    (
        "computed-communities",
        "Explicit structural community analysis, capped at 5000 nodes",
    ),
    (
        "communities",
        "Preserved typed community IDs/composition paths and computed structural communities",
    ),
    (
        "surprises",
        "Up to five scored structural connections with signals and full recorded edge evidence",
    ),
    ("audit", "Confidence counts and percentages"),
    (
        "questions",
        "Up to seven evidence-backed questions with rationale and candidate counts",
    ),
];

fn community_listing(snapshot: &CachedSnapshot, computed: bool) -> Result<Value> {
    let preserved: Vec<_> = snapshot
        .preserved
        .iter()
        .map(|c| json!({"id":c.id,"project":c.project,"names":c.names,"nodes":c.nodes.len()}))
        .collect();
    let mut output = json!({"schema_version":snapshot.snapshot.schema_version,
        "generation":snapshot.snapshot.generation,
        "community_source":"preserved", "communities":preserved,
        "preserved_communities":preserved, "computed_communities":null,
        "computed_status":"not_requested",
        "computed_resource":"computed-communities",
        "preserved_methodology":"Recorded memberships; integer/string IDs and composition paths remain distinct."});
    if computed || snapshot.preserved.is_empty() {
        let analysis = snapshot.analysis()?;
        let groups: Vec<_> = analysis
            .communities
            .iter()
            .map(|c| json!({"id":c.id,"nodes":c.nodes.len(),"cohesion":c.cohesion}))
            .collect();
        output["community_source"] = json!("computed");
        output["communities"] = json!(groups);
        output["computed_communities"] = json!(groups);
        output["computed_status"] = json!("computed");
        output["methodology"] = json!(analysis.methodology);
    }
    Ok(output)
}

fn resource_text(project: &Project, kind: &str) -> Result<String> {
    if kind == "stats" {
        return Ok(serde_json::to_string(
            &Store::open_read_only(&project.db)?.stats()?,
        )?);
    }
    let d = project.snapshot()?;
    // Snapshot-only paths never initialize structural analysis.
    let value = match kind {
        "graph" => serde_json::to_value(&d.snapshot)?,
        "communities" => community_listing(&d, false)?,
        "computed-communities" => community_listing(&d, true)?,
        "report" => {
            d.check_analysis_limits()?;
            return d
                .report
                .get_or_init(|| {
                    export::render(&d.snapshot, ExportFormat::Markdown)
                        .map_err(|e| format!("{e:#}"))
                })
                .clone()
                .map_err(anyhow::Error::msg);
        }
        "god-nodes" => hubs(&d, 10, None)?,
        "audit" => graph_stats(&d)?,
        "surprises" => {
            let a = d.analysis()?;
            json!({"schema_version":a.schema_version,"generation":a.generation,
                "surprises":a.surprises,"surprise_candidates":a.surprise_candidates,
                "truncated":a.surprise_candidates > a.surprises.len(),
                "methodology":a.methodology})
        }
        "questions" => {
            let a = d.analysis()?;
            json!({"schema_version":a.schema_version,"generation":a.generation,
                "questions":a.suggested_questions,
                "suggested_question_candidates":a.suggested_question_candidates,
                "truncated":a.suggested_question_candidates > a.suggested_questions.len(),
                "methodology":a.methodology})
        }
        _ => bail!("unknown resource"),
    };
    Ok(serde_json::to_string(&value)?)
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Graf {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(Implementation::new("graf", env!("CARGO_PKG_VERSION")))
            .with_instructions("Graf reads only explicitly registered local databases. Optional tool project selects a registry name, never a path. Graph queries/resources do not check worktree freshness, refresh, launch processes, or make network/model calls. PR tools are available only with --github-repo and invoke bounded read-only GitHub commands only when explicitly called; repo must match the configured repository, and project selects a registered database. No PR tool invokes a model or inspects local worktrees. Run index/update explicitly outside MCP. Generation identifies each snapshot. SQL queries: depth 0..6, limit 1..500, at most 20 seeds/5000 examined edges. Snapshot cache: at most 100000 nodes/1000000 edges/1000000 unresolved references per registered project; payload bytes default to 64 MiB, configured by --snapshot-max-bytes. Preserved-community reads do not cluster; computed communities require explicit selection when recorded memberships exist. Structural analysis remains capped at 5000 nodes/20000 edges/20000 unresolved references and 8 MiB snapshot. Resource responses at most 1 MiB, structured tool payloads 512 KiB. Four concurrent read workers. Resources enumerate registered project names; graphify:// aliases select default. HTTP is stateless Streamable HTTP at the configured path; browser Origins are rejected. Input messages at most 1 MiB. Inspect truncation and unresolved references.")
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListResourcesResult, ErrorData> {
        if request.is_some_and(|r| r.cursor.is_some()) {
            return Err(ErrorData::invalid_params(
                "resources are returned in one page; cursor is not supported",
                None,
            ));
        }
        let mut resources = Vec::new();
        for (kind, description) in RESOURCES {
            for prefix in ["graf://", "graphify://"] {
                resources.push(
                    Resource::new(format!("{prefix}{kind}"), format!("default/{kind}"))
                        .with_description(*description)
                        .with_mime_type(mime(kind)),
                );
            }
            for name in self.projects.keys() {
                resources.push(
                    Resource::new(
                        format!("graf://projects/{name}/{kind}"),
                        format!("{name}/{kind}"),
                    )
                    .with_description(*description)
                    .with_mime_type(mime(kind)),
                );
            }
        }
        Ok(ListResourcesResult {
            resources,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ReadResourceResponse, ErrorData> {
        let uri = request.uri;
        let (project, kind) = if let Some(path) = uri.strip_prefix("graf://projects/") {
            path.split_once('/')
                .ok_or_else(|| ErrorData::invalid_params("invalid resource URI", None))?
        } else if let Some(kind) = uri
            .strip_prefix("graf://")
            .or_else(|| uri.strip_prefix("graphify://"))
        {
            ("default", kind)
        } else {
            return Err(ErrorData::resource_not_found("unknown resource URI", None));
        };
        if !RESOURCES.iter().any(|(key, _)| *key == kind) {
            return Err(ErrorData::resource_not_found("unknown resource URI", None));
        }
        let content_type = mime(kind);
        let kind = kind.to_owned();
        let text = self
            .run(Some(project.to_owned()), move |p| {
                let text = resource_text(p, &kind)?;
                ensure!(
                    text.len() <= MAX_MESSAGE,
                    "resource exceeds 1 MiB; use CLI export"
                );
                Ok(text)
            })
            .await
            .map_err(|e| ErrorData::invalid_params(format!("{e:#}"), None))?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri).with_mime_type(content_type),
        ])
        .into())
    }
}

fn mime(kind: &str) -> &'static str {
    if kind == "report" {
        "text/markdown"
    } else {
        "application/json"
    }
}

fn registered(db: PathBuf, args: &ServeArgs) -> Result<Graf> {
    ensure!(
        (1..=256 * 1024 * 1024).contains(&args.snapshot_max_bytes),
        "snapshot-max-bytes must be between 1 and 268435456"
    );
    if let Some(repo) = &args.github_repo {
        prs::validate_repo(repo)?;
    }
    ensure!(
        args.project.len() < MAX_PROJECTS,
        "at most 32 databases including default may be registered"
    );
    let mut paths = BTreeMap::from([("default".to_owned(), db)]);
    for entry in &args.project {
        let (name, path) = entry.split_once('=').context("project must be NAME=DB")?;
        ensure!(
            !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "project names must be 1..64 ASCII letters, digits, underscores or hyphens"
        );
        ensure!(!path.is_empty(), "project database path must not be empty");
        ensure!(
            !paths.contains_key(name),
            "duplicate or reserved project name: {name}"
        );
        paths.insert(name.to_owned(), PathBuf::from(path));
    }
    let mut projects = BTreeMap::new();
    for (name, db) in paths {
        let db = db
            .canonicalize()
            .with_context(|| format!("cannot open registered project {name}"))?;
        drop(Store::open_read_only(&db)?);
        let memory_dir = (name == "default")
            .then(|| args.memory_dir.clone())
            .flatten();
        projects.insert(
            name,
            Arc::new(Project {
                db,
                memory_dir,
                snapshot_max_bytes: args.snapshot_max_bytes,
                cache: Mutex::new(None),
            }),
        );
    }
    let mut tool_router = Graf::tool_router();
    if args.github_repo.is_none() {
        for name in ["list_prs", "get_pr_impact", "triage_prs"] {
            tool_router.remove_route(name);
        }
    }
    Ok(Graf {
        projects: Arc::new(projects),
        github_repo: args.github_repo.clone(),
        workers: Arc::new(Semaphore::new(4)),
        tool_router,
    })
}

async fn authorize(
    expected: Option<blake3::Hash>,
    request: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    if let Some(expected) = expected {
        let provided = request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
            .map(|(_, token)| blake3::hash(token.as_bytes()));
        // blake3::Hash equality compares in constant time.
        if provided != Some(expected) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    Ok(next.run(request).await)
}

pub async fn serve(db: PathBuf, args: ServeArgs) -> Result<()> {
    let server = registered(db, &args)?;
    if args.transport == Transport::Stdio {
        ensure!(
            args.bearer_token_env.is_none() && args.allowed_host.is_empty(),
            "HTTP authentication/host options require --transport http"
        );
        // Use the SDK codec so oversized lines terminate the transport without unbounded buffering.
        let input = FramedRead::new(
            tokio::io::stdin(),
            JsonRpcMessageCodec::<ClientJsonRpcMessage>::new_with_max_length(MAX_MESSAGE),
        )
        .take_while(|result| std::future::ready(result.is_ok()))
        .map(|result| result.expect("codec errors terminate input"));
        let output = FramedWrite::new(
            tokio::io::stdout(),
            JsonRpcMessageCodec::<ServerJsonRpcMessage>::default(),
        );
        server.serve((output, input)).await?.waiting().await?;
        return Ok(());
    }
    ensure!(
        args.path.starts_with('/')
            && !args.path.contains(['{', '}', '*', '?', '#'])
            && args.path.len() <= 256,
        "HTTP path must be an absolute literal path without query or fragment"
    );
    ensure!(
        args.host.is_loopback() || args.bearer_token_env.is_some(),
        "nonloopback HTTP requires --bearer-token-env NAME"
    );
    let bearer = args.bearer_token_env.as_ref().map(|name| -> Result<_> {
        ensure!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'), "invalid bearer environment variable name");
        let token = std::env::var(name).map_err(|_| anyhow::anyhow!("bearer environment variable is missing or not Unicode"))?;
        ensure!(!token.is_empty() && token.len() <= 4096 && token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b)),
            "bearer environment variable must contain a nonempty ASCII bearer token of at most 4096 bytes");
        Ok(blake3::hash(token.as_bytes()))
    }).transpose()?;
    let mut config = StreamableHttpServerConfig::default().enforce_origin_validation();
    config.legacy_session_mode = false;
    config.json_response = true;
    config.max_request_body_bytes = MAX_MESSAGE;
    if !args.host.is_unspecified() {
        config.allowed_hosts.push(args.host.to_string());
    }
    for host in args.allowed_host {
        ensure!(
            !host.is_empty()
                && !host.contains(['*', '/', '@'])
                && !host.chars().any(char::is_whitespace),
            "allowed-host must be an exact HTTP authority"
        );
        config.allowed_hosts.push(host);
    }
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let app = axum::Router::new()
        .route_service(&args.path, service)
        .layer(axum::middleware::from_fn(move |request, next| {
            authorize(bearer, request, next)
        }));
    let listener = tokio::net::TcpListener::bind((args.host, args.port)).await?;
    eprintln!(
        "Graf MCP listening on http://{}{}",
        listener.local_addr()?,
        args.path
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

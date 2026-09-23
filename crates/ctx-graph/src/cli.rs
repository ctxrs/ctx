use super::*;
use clap::{Args, Parser, Subcommand, ValueEnum};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Database path. Otherwise discover the nearest ancestor .graf/index.db.
    #[arg(long, global = true)]
    pub(crate) db: Option<PathBuf>,
    /// Print machine-readable JSON instead of human-readable output.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Explicitly append read-command metadata to a JSONL file.
    #[arg(long, global = true)]
    pub(crate) query_log: Option<PathBuf>,
    /// Also include returned graph records in the explicit query log.
    #[arg(long, global = true, requires = "query_log")]
    pub(crate) log_responses: bool,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    #[command(flatten)]
    Extended(commands::Command),
    #[command(flatten)]
    Connect(connect::Command),
    /// Install explicit graph tool hooks; use ctx integrations for skills and MCP.
    Install(agent_setup::SetupArgs),
    /// Remove explicit graph tool hooks; use ctx integrations for skills and MCP.
    Uninstall(agent_setup::SetupArgs),
    /// Explicitly manage optional Git refresh hooks.
    Hook(agent_setup::HookArgs),
    /// Supply optional local graph context to an agent hook; never deny a tool.
    HookGuard(hook_guard::HookGuardArgs),
    /// Import a Graphify snapshot and switch this project's MCP connection.
    Switch(switch::SwitchArgs),
    /// Index supported source code and documents into a persistent local graph.
    #[command(alias = "extract")]
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        extraction: extraction::ExtractionArgs,
    },
    /// Refresh the native source root recorded in the database.
    Update {
        /// Include measured detection, extraction and commit times in the report.
        #[arg(long)]
        timing: bool,
        /// Re-extract local files once; saved remote sources remain offline.
        #[arg(long)]
        force: bool,
        /// Bypass semantic/transcript cache reads for local sources once.
        #[arg(long)]
        refresh_cache: bool,
        /// Accept fewer semantic facts, saving the previous graph in .graf/backups.
        #[arg(long)]
        allow_semantic_shrink: bool,
    },
    /// Compare local source fingerprints without model or converter calls.
    CheckUpdate,
    /// Reclaim unused database space without reading sources or changing graph facts.
    Compact,
    /// Explicit foreground polling; queries themselves never refresh the graph.
    Watch {
        #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(100..=3_600_000))]
        interval_ms: u64,
        /// Stop after this many polls; omitted means until interrupted.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        iterations: Option<u32>,
    },
    /// Explicitly import a URL, Google pointer or local document and retain its extracted facts.
    Add {
        source: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        contributor: Option<String>,
        #[arg(long)]
        captured_at_unix_secs: Option<u64>,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[command(flatten)]
        extraction: extraction::ExtractionArgs,
    },
    /// Manage semantic provider configurations; keys remain in the environment.
    Provider(extraction::ProviderArgs),
    /// Inspect or explicitly repair an extraction cache without provider calls.
    Cache(extraction::CacheArgs),
    /// Clone a GitHub repository with Git; optionally index it after checkout.
    Clone {
        url: String,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        index: bool,
        /// Fetch and fast-forward an existing cached checkout; local changes are never reset.
        #[arg(long)]
        refresh: bool,
    },
    /// Find symbols and explore a bounded neighborhood.
    #[command(name = "search", visible_alias = "query")]
    Query(QueryArgs),
    /// Show an exact ID or unique symbol and its immediate neighbors.
    #[command(alias = "explain")]
    Show(ShowArgs),
    /// Show immediate incoming calls to a symbol.
    Callers(SymbolArgs),
    /// Show immediate outgoing calls from a symbol.
    Callees(SymbolArgs),
    /// Follow reverse dependencies from a symbol, class members, or source file.
    #[command(alias = "affected")]
    Impact(ImpactArgs),
    /// Find a bounded path, following outgoing edges by default.
    Path(PathArgs),
    /// Report graph-wide counts, coverage, and diagnostics.
    Stats,
    /// Import a graph snapshot into an empty database (default: .graf/index.db).
    Import {
        #[command(subcommand)]
        format: ImportFormat,
    },
    /// Serve read-only MCP tools over stdin/stdout or explicit HTTP.
    Serve(mcp::ServeArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum ImportFormat {
    Graphify {
        file: PathBuf,
        /// node-link honors graph flags; export uses Graphify's logical edge direction and raw export layouts.
        #[arg(long, value_enum, default_value = "node-link")]
        format: SnapshotFormat,
        /// Atomically replace an existing imported graph, retaining it on failure.
        #[arg(long)]
        refresh: bool,
    },
    Graf {
        file: PathBuf,
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum SnapshotFormat {
    NodeLink,
    Export,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize, Deserialize, JsonSchema)]
pub(crate) enum TraversalDirection {
    #[serde(rename = "in")]
    In,
    #[serde(rename = "out")]
    Out,
    #[serde(rename = "both")]
    Both,
}

impl From<TraversalDirection> for Direction {
    fn from(value: TraversalDirection) -> Self {
        match value {
            TraversalDirection::In => Self::Incoming,
            TraversalDirection::Out => Self::Outgoing,
            TraversalDirection::Both => Self::Both,
        }
    }
}

pub(crate) fn one() -> u32 {
    1
}
pub(crate) fn three() -> u32 {
    3
}
pub(crate) fn six() -> u32 {
    6
}
pub(crate) fn hundred() -> u32 {
    100
}
pub(crate) fn both() -> TraversalDirection {
    TraversalDirection::Both
}
pub(crate) fn outgoing() -> TraversalDirection {
    TraversalDirection::Out
}

#[derive(Debug, Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct QueryArgs {
    /// Symbol search text (nonempty).
    #[schemars(length(min = 1))]
    pub(crate) text: String,
    /// Maximum traversal depth, 0..6. Default 1.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(0..=6))]
    #[serde(default = "one")]
    #[schemars(range(min = 0, max = 6))]
    pub(crate) depth: u32,
    /// Maximum results, 1..500. Default 100. Truncation is reported explicitly.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    pub(crate) limit: u32,
    /// Traverse incoming, outgoing, or both directions. Default both.
    #[arg(long, value_enum, default_value = "both")]
    #[serde(default = "both")]
    pub(crate) direction: TraversalDirection,
    /// Exact relation filter, such as calls, imports, or contains; omitted means all.
    #[arg(long)]
    pub(crate) relation: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub(crate) navigation: NavigationArgs,
}

#[derive(Debug, Default, Args, Deserialize, JsonSchema)]
#[serde(default)]
pub(crate) struct NavigationArgs {
    /// Use depth-first traversal instead of breadth-first.
    #[arg(long)]
    pub(crate) dfs: bool,
    /// Restrict relationship contexts (repeatable).
    #[arg(long)]
    pub(crate) context: Vec<String>,
    /// Restrict node source files (repeatable).
    #[arg(long)]
    pub(crate) file: Vec<String>,
    /// Restrict node kinds (repeatable).
    #[arg(long)]
    pub(crate) kind: Vec<String>,
    /// Approximate JSON token budget: UTF-8 bytes divided by four.
    #[arg(long)]
    pub(crate) budget: Option<usize>,
    /// Include edges between any returned nodes, within the query bounds.
    #[arg(long)]
    pub(crate) induced_edges: bool,
    #[arg(long)]
    pub(crate) infer_context: bool,
}
impl NavigationArgs {
    pub(crate) fn enabled(&self) -> bool {
        self.dfs
            || !self.context.is_empty()
            || !self.file.is_empty()
            || !self.kind.is_empty()
            || self.budget.is_some()
            || self.induced_edges
            || self.infer_context
    }
    pub(crate) fn options(self, graph: QueryOptions) -> graf::query::SearchOptions {
        graf::query::SearchOptions {
            graph,
            traversal: if self.dfs {
                graf::query::Traversal::Dfs
            } else {
                graf::query::Traversal::Bfs
            },
            contexts: self.context,
            files: self.file,
            kinds: self.kind,
            token_budget: self.budget,
            induced_edges: self.induced_edges,
            infer_context: self.infer_context,
        }
    }
}

#[derive(Debug, Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SymbolArgs {
    /// Exact node ID or unique label/qualified name. Ambiguous names are errors.
    #[schemars(length(min = 1))]
    pub(crate) symbol: String,
    /// Maximum results, 1..500. Default 100.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    pub(crate) limit: u32,
    #[command(flatten)]
    #[serde(flatten)]
    pub(crate) navigation: NavigationArgs,
}

#[derive(Debug, Args)]
pub(crate) struct ShowArgs {
    #[command(flatten)]
    pub(crate) symbol: SymbolArgs,
    /// Read fresh learning evidence for returned nodes; never writes or changes selection.
    /// Uses remaining --budget space, or at most 8 KiB of annotations without --budget.
    #[arg(long)]
    pub(crate) memory_dir: Option<PathBuf>,
}

#[derive(Debug, Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImpactArgs {
    /// Exact node ID or unique label/qualified name. Ambiguous names are errors.
    #[schemars(length(min = 1))]
    pub(crate) symbol: String,
    /// Maximum reverse dependency depth, 0..6. Default 3.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(0..=6))]
    #[serde(default = "three")]
    #[schemars(range(min = 0, max = 6))]
    pub(crate) depth: u32,
    /// Maximum results, 1..500. Default 100.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    pub(crate) limit: u32,
    /// Follow these dependency relations; repeat to combine. Defaults to known dependency relations.
    #[arg(long)]
    #[serde(default)]
    pub(crate) relation: Vec<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub(crate) navigation: NavigationArgs,
}

#[derive(Debug, Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PathArgs {
    /// Starting node: exact ID or unique label/qualified name.
    #[schemars(length(min = 1))]
    pub(crate) source: String,
    /// Destination node: exact ID or unique label/qualified name.
    #[schemars(length(min = 1))]
    pub(crate) target: String,
    /// Maximum search depth, 0..6. Default 6.
    #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u32).range(0..=6))]
    #[serde(default = "six")]
    #[schemars(range(min = 0, max = 6))]
    pub(crate) depth: u32,
    /// Maximum results, 1..500. Default 100.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    pub(crate) limit: u32,
    /// Edge direction. Default out; undirected imported edges remain bidirectional.
    #[arg(long, value_enum, default_value = "out")]
    #[serde(default = "outgoing")]
    pub(crate) direction: TraversalDirection,
    /// Exact relation filter; omitted means all relations.
    #[arg(long)]
    pub(crate) relation: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub(crate) navigation: NavigationArgs,
}

#[derive(Debug, Parser)]
#[command(
    name = "graph",
    bin_name = "ctx graph",
    version,
    styles = clap::builder::Styles::plain(),
    about = "Navigate a persistent local code graph",
    after_help = "Reads use the indexed snapshot. Only explicit --memory-dir annotations check live source evidence. Run ctx graph update explicitly to refresh native indexes."
)]
pub(crate) struct Cli {
    #[command(flatten)]
    pub(crate) graph: GraphArgs,
}

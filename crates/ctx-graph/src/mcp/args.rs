use super::*;

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
pub(crate) struct Routed<T> {
    /// Registered project name; omitted selects default. Filesystem paths are not accepted.
    pub(crate) project: Option<String>,
    #[serde(flatten)]
    pub(crate) args: T,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectArgs {
    /// Registered project name; omitted selects default.
    pub(crate) project: Option<String>,
}

pub(crate) fn ten() -> usize {
    10
}
pub(crate) fn hundred() -> usize {
    100
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct HubArgs {
    pub(crate) project: Option<String>,
    #[serde(default = "ten")]
    #[schemars(range(min = 1, max = 500))]
    pub(crate) top_n: usize,
    /// Suppress degrees above the empirical degree percentile, 0..100.
    #[schemars(range(min = 0, max = 100))]
    pub(crate) exclude_hubs_percentile: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommunityArgs {
    pub(crate) project: Option<String>,
    pub(crate) community_id: CommunityId,
    /// Auto uses preserved memberships when available, otherwise computed communities.
    #[serde(default)]
    pub(crate) community_source: CommunitySource,
    /// Composition path from the communities resource; distinct from registered project.
    pub(crate) community_project: Option<Vec<String>>,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    pub(crate) token_budget: usize,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    pub(crate) limit: usize,
}

#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum CommunityId {
    Integer(i64),
    Unsigned(u64),
    Text(String),
}
impl CommunityId {
    pub(crate) fn value(&self) -> Value {
        match self {
            Self::Integer(id) => json!(id),
            Self::Unsigned(id) => json!(id),
            Self::Text(id) => json!(id),
        }
    }
}

#[derive(Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CommunitySource {
    #[default]
    Auto,
    Preserved,
    Computed,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrListArgs {
    pub(crate) project: Option<String>,
    /// Must match the repository configured with --github-repo; omitted selects it.
    pub(crate) repo: Option<String>,
    /// Expected base; omitted asks GitHub for the actual default branch.
    pub(crate) base: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrImpactArgs {
    pub(crate) project: Option<String>,
    pub(crate) repo: Option<String>,
    #[schemars(range(min = 1, max = 2147483647))]
    pub(crate) pr_number: u64,
}

pub(crate) fn three() -> u32 {
    3
}
pub(crate) fn six() -> u32 {
    6
}
pub(crate) fn token_budget() -> usize {
    2000
}

#[derive(Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum QueryMode {
    #[default]
    Bfs,
    Dfs,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphQueryArgs {
    pub(crate) project: Option<String>,
    pub(crate) question: String,
    #[serde(default)]
    pub(crate) mode: QueryMode,
    #[serde(default = "three")]
    #[schemars(range(min = 0, max = 6))]
    pub(crate) depth: u32,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    pub(crate) limit: usize,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    pub(crate) token_budget: usize,
    #[serde(default)]
    pub(crate) context_filter: Vec<String>,
    #[serde(default)]
    pub(crate) files: Vec<String>,
    #[serde(default)]
    pub(crate) kinds: Vec<String>,
    #[serde(default)]
    pub(crate) induced_edges: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NodeArgs {
    pub(crate) project: Option<String>,
    /// Exact ID/label or file::symbol; otherwise a unique normalized prefix/substring.
    pub(crate) label: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NeighborArgs {
    pub(crate) project: Option<String>,
    pub(crate) label: String,
    /// Exact relation, or a unique normalized prefix/substring among incident relations.
    pub(crate) relation_filter: Option<String>,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    pub(crate) token_budget: usize,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    pub(crate) limit: usize,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShortestPathArgs {
    pub(crate) project: Option<String>,
    pub(crate) source: String,
    pub(crate) target: String,
    /// Graf bounds path searches to six hops; default six.
    #[serde(default = "six")]
    #[schemars(range(min = 0, max = 6))]
    pub(crate) max_hops: u32,
    #[serde(default)]
    pub(crate) undirected: bool,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    pub(crate) limit: usize,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    pub(crate) token_budget: usize,
}

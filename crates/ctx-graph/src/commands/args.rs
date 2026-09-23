use super::*;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Analyze the complete stored graph; does not refresh source files.
    #[command(alias = "cluster-only")]
    Analyze(AnalyzeArgs),
    /// List structural communities, or select one by ID.
    Communities(CommunitiesArgs),
    /// Rank the complete graph's hubs by degree or PageRank.
    #[command(alias = "god-nodes")]
    Hubs(HubsArgs),
    /// Diagnose stored multigraph edges without collapsing or changing them.
    Diagnose(DiagnoseArgs),
    /// Time explicitly selected bounded SQL queries without refreshing or writing the graph.
    Benchmark(BenchmarkArgs),
    /// Save deterministic community labels; reuse labels only for identical membership.
    Label(LabelArgs),
    /// Export an offline interactive tree view.
    Tree(TreeArgs),
    /// Render a graph report, visualization, or wiki.
    Report(ReportArgs),
    /// Export a complete snapshot in a selected offline format.
    Export(ExportArgs),
    /// Compose explicitly named sources into a new SQLite database.
    Merge(MergeArgs),
    /// Explicitly manage a stored cross-project aggregate.
    Global(GlobalArgs),
    /// Explicitly save a local answer and its outcome; never captures queries automatically.
    SaveResult(MemorySaveArgs),
    /// Summarize explicitly saved outcomes into local lessons without model calls.
    Reflect(MemoryReflectArgs),
    /// Read GitHub pull requests and optionally map their files to the stored graph.
    Prs(PrsCommandArgs),
}

#[derive(Debug, Args)]
pub struct PrsCommandArgs {
    #[command(flatten)]
    pub input: ctx_graph_core::prs::PrsArgs,
    /// Use this Graf snapshot for changed-file impact instead of a database.
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct MemorySaveArgs {
    #[command(flatten)]
    pub input: ctx_graph_core::memory::SaveResultArgs,
    /// Validate cited nodes against this Graf snapshot instead of a database.
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct MemoryReflectArgs {
    #[command(flatten)]
    pub input: ctx_graph_core::memory::ReflectArgs,
    /// Validate citations and group lessons using this Graf snapshot.
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SourceArgs {
    /// Read a Graf snapshot JSON file instead of a database.
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
    #[command(flatten)]
    pub analysis: AnalysisArgs,
}

#[derive(Debug, Args)]
pub struct AnalysisArgs {
    /// Community detection backend. Leiden uses a deterministic seeded native implementation.
    #[arg(long, value_enum, default_value_t = analysis::CommunityAlgorithm::default())]
    pub community_algorithm: analysis::CommunityAlgorithm,
    /// Leiden starts: 1 stops on repeated membership; 4 uses four fixed allocations
    /// within the shared 100-iteration CLI budget.
    ///
    /// Four can cost more and does not guarantee better semantic communities.
    /// A positive initial graph uses the whole budget, leaving no size/cohesion
    /// retries; unmet soft targets are reported in unsatisfied_community_constraints.
    #[arg(long, default_value_t = 1, value_parser = community_starts)]
    pub community_starts: u32,
    /// Reproducible Leiden random seed.
    #[arg(long, default_value_t = 42)]
    pub community_seed: u64,
    /// Maximum local Leiden sweeps per level; does not guarantee convergence.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
    pub community_local_max_passes: u32,
    /// Community resolution; larger values favor smaller groups.
    #[arg(long, default_value_t = 1.0, value_parser = positive_float)]
    pub resolution: f64,
    /// Soft community size target; splitting shares the analysis pass budget.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub max_community_size: Option<u32>,
    /// Soft minimum community cohesion target, from zero to one.
    #[arg(long, value_parser = cohesion)]
    pub min_cohesion: Option<f64>,
    /// Exclude nodes above this degree percentile from community partitioning and hub ranks.
    #[arg(long, value_parser = percentile)]
    pub exclude_hubs: Option<f64>,
    /// Include file/container/builtin noise in hub ranks and community labels.
    #[arg(long)]
    pub include_noise: bool,
}

pub(crate) fn community_starts(text: &str) -> Result<u32, String> {
    match text {
        "1" => Ok(1),
        "4" => Ok(4),
        _ => Err("community starts must be 1 or 4".into()),
    }
}

pub(crate) fn positive_float(text: &str) -> Result<f64, String> {
    let value = text
        .parse::<f64>()
        .map_err(|_| "expected a finite positive number")?;
    if !value.is_finite() || value <= 0.0 {
        return Err("expected a finite positive number".into());
    }
    Ok(value)
}

pub(crate) fn cohesion(text: &str) -> Result<f64, String> {
    let value = text
        .parse::<f64>()
        .map_err(|_| "cohesion must be between 0 and 1")?;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err("cohesion must be between 0 and 1".into());
    }
    Ok(value)
}

pub(crate) fn percentile(text: &str) -> Result<f64, String> {
    let value: f64 = text
        .parse()
        .map_err(|_| "expected a percentile between 0 and 100")?;
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err("percentile must be between 0 and 100".into());
    }
    Ok(value)
}

impl AnalysisArgs {
    pub(crate) fn options(&self) -> AnalysisOptions {
        AnalysisOptions {
            community_algorithm: self.community_algorithm,
            community_starts: self.community_starts,
            community_seed: self.community_seed,
            community_local_max_passes: self.community_local_max_passes,
            resolution: self.resolution,
            max_community_size: self.max_community_size.map(|v| v as usize),
            min_cohesion: self.min_cohesion,
            exclude_hubs_percentile: self.exclude_hubs,
            filter_noise: !self.include_noise,
            ..AnalysisOptions::default()
        }
    }
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    /// Include explicit saved observations in HTML, Markdown, or vault reports.
    /// Reads this directory and checks cited local source files without changing the graph.
    #[arg(long)]
    pub memory_dir: Option<PathBuf>,
    /// Accept a smaller JSON graph export; the previous output is backed up first.
    #[arg(long)]
    pub allow_shrink: bool,
    /// Apply saved Graf community labels only where complete membership still matches.
    #[arg(long)]
    pub labels: Option<PathBuf>,
    /// Maximum displayed nodes in interactive visualizations; exported full data remains available.
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u32).range(1..))]
    pub node_limit: u32,
    /// Maximum drawn edges in interactive visualizations.
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..))]
    pub edge_limit: u32,
}

impl ViewArgs {
    pub(crate) fn options(&self, analysis: &AnalysisArgs) -> Result<ExportOptions> {
        let labels = self.labels.as_deref().map(read_labels).transpose()?;
        let mut options = ExportOptions {
            analysis: analysis.options(),
            node_limit: self.node_limit as usize,
            edge_limit: self.edge_limit as usize,
            ..ExportOptions::default()
        };
        options.community_labels = labels
            .map(|f| {
                f.labels
                    .into_iter()
                    .map(|(sig, value)| (sig, value.label))
                    .collect()
            })
            .unwrap_or_default();
        Ok(options)
    }
}

#[derive(Debug, Args)]
pub struct AnalyzeArgs {
    #[command(flatten)]
    pub source: SourceArgs,
    /// Save complete analysis JSON atomically instead of printing it.
    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct CommunitiesArgs {
    #[command(flatten)]
    pub source: SourceArgs,
    /// Community ID from this snapshot's analysis.
    #[arg(long)]
    pub id: Option<usize>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum HubSort {
    Degree,
    Pagerank,
}

#[derive(Debug, Args)]
pub struct HubsArgs {
    #[command(flatten)]
    pub source: SourceArgs,
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..))]
    pub top: u32,
    #[arg(long, value_enum, default_value = "degree")]
    pub sort: HubSort,
}

#[derive(Debug, Args)]
pub struct DiagnoseArgs {
    /// Optional diagnostic name, for compatibility with `diagnose multigraph`.
    #[arg(value_parser = ["multigraph"])]
    pub kind: Option<String>,
    /// Read a Graf snapshot JSON file instead of a database.
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
    /// Maximum endpoint-collapse examples; zero prints counts only.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(0..=100))]
    pub max_examples: u32,
}

#[derive(Debug, Args)]
pub struct BenchmarkArgs {
    /// Search text to time; repeat for up to 32 queries. No implicit corpus-wide query.
    #[arg(long, required = true)]
    pub query: Vec<String>,
    /// Measured calls per query, after one unmeasured warm-up call.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub iterations: u32,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(0..=6))]
    pub depth: u32,
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    pub limit: u32,
    #[arg(long, value_enum, default_value = "both")]
    pub direction: QueryDirection,
    #[arg(long)]
    pub relation: Option<String>,
}

#[derive(Debug, Args)]
pub struct LabelArgs {
    #[command(flatten)]
    pub source: SourceArgs,
    /// Save labels here atomically; existing labels are reused unless --input is supplied.
    #[arg(long)]
    pub output: PathBuf,
    /// Reuse this Graf label JSON file (maximum 8 MiB) instead of existing --output labels.
    #[arg(long)]
    pub input: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct TreeArgs {
    #[command(flatten)]
    pub source: SourceArgs,
    #[command(flatten)]
    pub view: ViewArgs,
    /// Replace this output file atomically; otherwise print the HTML.
    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    SnapshotJson,
    GraphifyJson,
    #[value(name = "graphml")]
    #[serde(rename = "graphml")]
    GraphMl,
    Cypher,
    Mermaid,
    Svg,
    Html,
    Markdown,
    Canvas,
    CallflowHtml,
    TreeHtml,
    Wiki,
    Obsidian,
}
impl Format {
    pub(crate) fn single(self) -> Option<ExportFormat> {
        Some(match self {
            Self::SnapshotJson => ExportFormat::SnapshotJson,
            Self::GraphifyJson => ExportFormat::GraphifyJson,
            Self::GraphMl => ExportFormat::GraphMl,
            Self::Cypher => ExportFormat::Cypher,
            Self::Mermaid => ExportFormat::Mermaid,
            Self::Svg => ExportFormat::Svg,
            Self::Html => ExportFormat::Html,
            Self::Markdown => ExportFormat::Markdown,
            Self::Canvas => ExportFormat::Canvas,
            Self::CallflowHtml => ExportFormat::CallflowHtml,
            Self::TreeHtml => ExportFormat::TreeHtml,
            Self::Wiki | Self::Obsidian => return None,
        })
    }
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    #[command(flatten)]
    pub source: SourceArgs,
    #[command(flatten)]
    pub view: ViewArgs,
    #[arg(long, value_enum, default_value = "markdown")]
    pub format: Format,
    /// Replace this output file atomically; wiki/obsidian require an existing directory.
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// Explicitly compare native source fingerprints; otherwise report stored coverage only.
    #[arg(long)]
    pub check_freshness: bool,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    #[arg(value_enum)]
    pub format: Format,
    #[command(flatten)]
    pub source: SourceArgs,
    #[command(flatten)]
    pub view: ViewArgs,
    /// Replace this output file atomically; wiki/obsidian create a new folder here.
    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct NamedSource {
    pub name: String,
    pub path: PathBuf,
}
pub(crate) fn named_source(value: &str) -> Result<NamedSource, String> {
    let (name, path) = value.split_once('=').ok_or("expected NAME=PATH")?;
    if name.trim().is_empty() || path.is_empty() {
        return Err("NAME and PATH must not be empty".into());
    }
    Ok(NamedSource {
        name: name.into(),
        path: path.into(),
    })
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// Named Graf database or project directory. Repeat for additional sources.
    #[arg(long, value_name = "NAME=PATH", value_parser = named_source)]
    pub project: Vec<NamedSource>,
    /// Named Graf snapshot JSON. Repeat for additional snapshots.
    #[arg(long, value_name = "NAME=PATH", value_parser = named_source)]
    pub snapshot: Vec<NamedSource>,
    /// Link distinct package nodes across projects when their canonical package keys match exactly.
    #[arg(long)]
    pub link_packages: bool,
    /// Resolve eligible exact public namespace references across source projects.
    #[arg(long)]
    pub link_references: bool,
    /// New Graf SQLite database; existing files are never replaced by merge.
    #[arg(long)]
    pub output: PathBuf,
}

#[derive(Debug, Args)]
pub struct GlobalArgs {
    #[command(subcommand)]
    pub command: GlobalCommand,
}

#[derive(Debug, Subcommand)]
pub enum GlobalCommand {
    /// Register or replace NAME; link exact package keys across projects and skip unchanged content.
    Add {
        name: String,
        /// Graf database, project directory, or a Graf JSON file with --snapshot.
        path: PathBuf,
        #[arg(long)]
        snapshot: bool,
    },
    /// Remove NAME and rebuild from the remaining sources.
    Remove { name: String },
    /// List stored source registrations without reading their files.
    List,
    /// Explicitly rebuild the aggregate from all registered sources.
    Refresh,
    /// Query the stored aggregate without reading or refreshing its sources.
    Query(GlobalQueryArgs),
    /// Print the selected aggregate database location.
    Path,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum QueryDirection {
    In,
    Out,
    Both,
}
impl From<QueryDirection> for Direction {
    fn from(value: QueryDirection) -> Self {
        match value {
            QueryDirection::In => Self::Incoming,
            QueryDirection::Out => Self::Outgoing,
            QueryDirection::Both => Self::Both,
        }
    }
}

#[derive(Debug, Args)]
pub struct GlobalQueryArgs {
    pub text: String,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(0..=6))]
    pub depth: u32,
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    pub limit: u32,
    #[arg(long, value_enum, default_value = "both")]
    pub direction: QueryDirection,
    #[arg(long)]
    pub relation: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SourceKind {
    Database,
    Snapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) kind: SourceKind,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registry {
    pub(crate) version: u32,
    pub(crate) entries: Vec<Registration>,
}

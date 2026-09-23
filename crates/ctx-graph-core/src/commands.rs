//! Explicit whole-graph workflows. Registry reads use stored SQLite data only.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand, ValueEnum};
use crate::{
    analysis::{self, AnalysisOptions},
    export::{self, ExportFormat, ExportOptions},
    model::{Direction, Edge, GraphSnapshot, ImportedGraph, QueryOptions, SCHEMA_VERSION, Stats},
    snapshot,
    store::Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
    pub input: crate::prs::PrsArgs,
    /// Use this Graf snapshot for changed-file impact instead of a database.
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct MemorySaveArgs {
    #[command(flatten)]
    pub input: crate::memory::SaveResultArgs,
    /// Validate cited nodes against this Graf snapshot instead of a database.
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct MemoryReflectArgs {
    #[command(flatten)]
    pub input: crate::memory::ReflectArgs,
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

fn community_starts(text: &str) -> Result<u32, String> {
    match text {
        "1" => Ok(1),
        "4" => Ok(4),
        _ => Err("community starts must be 1 or 4".into()),
    }
}

fn positive_float(text: &str) -> Result<f64, String> {
    let value = text
        .parse::<f64>()
        .map_err(|_| "expected a finite positive number")?;
    if !value.is_finite() || value <= 0.0 {
        return Err("expected a finite positive number".into());
    }
    Ok(value)
}

fn cohesion(text: &str) -> Result<f64, String> {
    let value = text
        .parse::<f64>()
        .map_err(|_| "cohesion must be between 0 and 1")?;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err("cohesion must be between 0 and 1".into());
    }
    Ok(value)
}

fn percentile(text: &str) -> Result<f64, String> {
    let value: f64 = text
        .parse()
        .map_err(|_| "expected a percentile between 0 and 100")?;
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err("percentile must be between 0 and 100".into());
    }
    Ok(value)
}

impl AnalysisArgs {
    fn options(&self) -> AnalysisOptions {
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
    fn options(&self, analysis: &AnalysisArgs) -> Result<ExportOptions> {
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
    fn single(self) -> Option<ExportFormat> {
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
fn named_source(value: &str) -> Result<NamedSource, String> {
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
enum SourceKind {
    Database,
    Snapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    name: String,
    path: PathBuf,
    kind: SourceKind,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    version: u32,
    entries: Vec<Registration>,
}

fn print(value: &impl Serialize, compact: bool) -> Result<()> {
    let mut out = std::io::stdout().lock();
    if compact {
        serde_json::to_writer(&mut out, value)?;
    } else {
        serde_json::to_writer_pretty(&mut out, value)?;
    }
    writeln!(out)?;
    Ok(())
}

fn local_database(db: Option<&Path>) -> Result<PathBuf> {
    optional_local_database(db)?
        .context("no .graf/index.db found; run graf index or pass --db or --snapshot")
}

fn optional_local_database(db: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(db) = db {
        return Ok(Some(db.to_owned()));
    }
    for dir in std::env::current_dir()?.ancestors() {
        let path = dir.join(".graf/index.db");
        if path.try_exists()? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn optional_graph(snapshot: Option<&Path>, db: Option<&Path>) -> Result<Option<GraphSnapshot>> {
    ensure!(
        snapshot.is_none() || db.is_none(),
        "--snapshot conflicts with --db"
    );
    if let Some(path) = snapshot {
        return load_source(path, SourceKind::Snapshot).map(Some);
    }
    optional_local_database(db)?
        .map(|path| load_source(&path, SourceKind::Database))
        .transpose()
}

fn regular(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "expected a regular file, not a symlink: {}",
        path.display()
    );
    Ok(())
}

fn registered_source(name: &str, path: &Path, kind: SourceKind) -> Result<Registration> {
    ensure!(!name.trim().is_empty(), "project name must not be empty");
    let path = if matches!(kind, SourceKind::Database) && path.is_dir() {
        path.join(".graf/index.db")
    } else {
        path.to_owned()
    };
    regular(&path)?;
    Ok(Registration {
        name: name.into(),
        path: path.canonicalize()?,
        kind,
    })
}

fn load_source(path: &Path, kind: SourceKind) -> Result<GraphSnapshot> {
    regular(path)?;
    match kind {
        SourceKind::Database => Store::open_read_only(path)?.snapshot(),
        SourceKind::Snapshot => {
            let graph = snapshot::read(path)?;
            // The strict snapshot reader keeps its validated source header here.
            let header = &graph.metadata["graf_snapshot"];
            Ok(GraphSnapshot {
                schema_version: SCHEMA_VERSION,
                generation: header["generation"]
                    .as_u64()
                    .context("missing snapshot generation")?,
                kind: header["kind"]
                    .as_str()
                    .context("missing snapshot kind")?
                    .into(),
                root: header["root"].as_str().map(str::to_owned),
                metadata: header["metadata"].clone(),
                nodes: graph.nodes,
                edges: graph.edges,
            })
        }
    }
}

fn load(source: &SourceArgs, db: Option<&Path>) -> Result<(GraphSnapshot, PathBuf)> {
    ensure!(
        source.snapshot.is_none() || db.is_none(),
        "--snapshot conflicts with --db"
    );
    let (path, kind) = if let Some(path) = &source.snapshot {
        (path.clone(), SourceKind::Snapshot)
    } else {
        (local_database(db)?, SourceKind::Database)
    };
    let path = path
        .canonicalize()
        .with_context(|| format!("cannot find source {}", path.display()))?;
    Ok((load_source(&path, kind)?, path))
}

fn same_file(a: &Path, b: &Path) -> Result<bool> {
    if a.canonicalize()? == b.canonicalize()? {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (a.metadata()?, b.metadata()?);
        if (a.dev(), a.ino()) == (b.dev(), b.ino()) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn destination(path: &Path, sources: &[PathBuf]) -> Result<bool> {
    let exists = match fs::symlink_metadata(path) {
        Ok(m) => {
            ensure!(
                m.is_file(),
                "output must be a regular file, not a symlink or directory"
            );
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };
    if exists {
        for source in sources {
            ensure!(
                !same_file(path, source)?,
                "output must not overwrite a source database or snapshot"
            );
        }
        let mut header = [0; 16];
        if File::open(path)?.read(&mut header)? == header.len() {
            ensure!(
                &header != b"SQLite format 3\0",
                "output must not overwrite a SQLite database"
            );
        }
    }
    protect_sidecars(path, sources)?;
    Ok(exists)
}

fn protect_sidecars(path: &Path, sources: &[PathBuf]) -> Result<()> {
    let full = std::path::absolute(path)?;
    let parent = full.parent().context("output has no parent")?;
    if parent.try_exists()? {
        let full = parent
            .canonicalize()?
            .join(full.file_name().context("output has no filename")?);
        for source in sources {
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut sidecar = source.as_os_str().to_os_string();
                sidecar.push(suffix);
                ensure!(
                    full != sidecar,
                    "output must not overwrite a source SQLite sidecar"
                );
            }
        }
    }
    Ok(())
}

fn write_atomic(path: &Path, content: &[u8], sources: &[PathBuf]) -> Result<()> {
    let exists = destination(path, sources)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    if exists {
        let original = read_output(path)?;
        if original == content {
            return Ok(());
        }
        backup_output(parent, &original)?;
    }
    let stage = tempfile::tempdir_in(parent)?;
    crate::switch_files::protect(stage.path())?;
    let mut temp = tempfile::NamedTempFile::new_in(stage.path())?;
    crate::switch_files::protect(temp.path())?;
    if exists {
        crate::switch_files::preserve_permissions(temp.as_file(), path)?;
    }
    temp.write_all(content)?;
    temp.as_file().sync_all()?;
    ensure!(
        destination(path, sources)? == exists,
        "output appeared or disappeared while rendering; retry"
    );
    crate::switch_files::replace(temp, path, exists)
}

fn read_output(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(256 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 256 * 1024 * 1024,
        "existing output exceeds 256 MiB backup limit"
    );
    Ok(bytes)
}

fn backup_output(parent: &Path, original: &[u8]) -> Result<()> {
    let directory = parent.join(".graf/export-backups");
    for path in [parent.join(".graf"), directory.clone()] {
        match fs::create_dir(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&path)?;
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "export backup path must be a regular directory"
                );
            }
            Err(error) => return Err(error).context("cannot create export backup directory"),
        }
    }
    let path = directory.join(format!("{}.bak", blake3::hash(original).to_hex()));
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "export backup must be a regular file"
            );
            ensure!(
                read_output(&path)? == original,
                "existing export backup differs"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
            crate::switch_files::protect(staged.path())?;
            staged.write_all(original)?;
            staged.as_file().sync_all()?;
            staged
                .persist_noclobber(&path)
                .map_err(|e| e.error)
                .context("cannot save previous export")?;
        }
        Err(error) => return Err(error.into()),
    }
    eprintln!("Saved previous output to {}", path.display());
    Ok(())
}

fn export_graph(
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
        options.learning = Some(crate::memory::learning_overlay(
            &crate::memory::ReflectArgs {
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
                Format::GraphifyJson => crate::import::read_graphify_export(output),
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

fn report_context(
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
            .and_then(|root| crate::index::check_update(Path::new(root), path))
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

fn contextual_report(
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

fn compose(entries: &[Registration], destination: &Path) -> Result<ImportedGraph> {
    let sources: Vec<_> = entries.iter().map(|entry| entry.path.clone()).collect();
    protect_sidecars(destination, &sources)?;
    let mut names = BTreeSet::new();
    let mut snapshots = Vec::new();
    for entry in entries {
        ensure!(
            !entry.name.trim().is_empty() && names.insert(&entry.name),
            "duplicate or empty project name"
        );
        regular(&entry.path).with_context(|| {
            format!("source {} is unavailable; aggregate unchanged", entry.name)
        })?;
        if destination.try_exists()? {
            ensure!(
                !same_file(destination, &entry.path)?,
                "aggregate destination must not be one of its sources"
            );
        }
        snapshots.push((
            entry.name.clone(),
            load_source(&entry.path, entry.kind)
                .with_context(|| format!("cannot load {}; aggregate unchanged", entry.name))?,
        ));
    }
    if snapshots.is_empty() {
        return Ok(ImportedGraph {
            nodes: vec![],
            edges: vec![],
            metadata: json!({"projects":[]}),
        });
    }
    snapshot::merge(snapshots)
}

// New SQLite outputs are built entirely in a private sibling staging directory.
// Closing the last connection checkpoints the WAL before the standalone DB copy
// is published without clobbering an existing destination.
fn create_database(path: &Path, graph: ImportedGraph) -> Result<Stats> {
    ensure!(
        fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "destination already exists; choose a new database path"
    );
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let stage = tempfile::tempdir_in(parent)?;
    crate::switch_files::protect(stage.path())?;
    let staged_db = stage.path().join("graph.db");
    let stats = {
        let mut store = Store::create(&staged_db)?;
        store.import_graph(graph)?
    };
    let mut temp = tempfile::NamedTempFile::new_in(stage.path())?;
    crate::switch_files::protect(temp.path())?;
    std::io::copy(&mut File::open(&staged_db)?, temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    crate::switch_files::replace(temp, path, false)?;
    Ok(stats)
}

fn merge(args: &MergeArgs, db: Option<&Path>, json_output: bool) -> Result<()> {
    ensure!(db.is_none(), "merge requires --output; omit --db");
    ensure!(
        args.project.len() + args.snapshot.len() >= 2,
        "merge requires at least two explicitly named sources"
    );
    let mut entries = Vec::new();
    for (kind, sources) in [
        (SourceKind::Database, &args.project),
        (SourceKind::Snapshot, &args.snapshot),
    ] {
        for source in sources {
            entries.push(registered_source(&source.name, &source.path, kind)?);
        }
    }
    let mut graph = compose(&entries, &args.output)?;
    let package_links = if args.link_packages {
        link_packages(&mut graph)
    } else {
        0
    };
    let reference_links = if args.link_references {
        crate::composition::link_references(&mut graph)?
    } else {
        0
    };
    let stats = create_database(&args.output, graph)?;
    print(
        &json!({"output":args.output,"projects":entries,"package_links":package_links,"reference_links":reference_links,"stats":stats}),
        json_output,
    )
}

fn global_path(db: Option<&Path>) -> Result<PathBuf> {
    if let Some(db) = db {
        return Ok(db.to_owned());
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("cannot find home directory; pass --db")?;
    Ok(PathBuf::from(home).join(".graf/global.db"))
}

fn registry(store: &Store) -> Result<Registry> {
    let metadata = store.graph_metadata()?;
    let value = metadata
        .get("graf_registry")
        .context("database is not a Graf global registry; choose a different --db")?;
    let registry: Registry =
        serde_json::from_value(value.clone()).context("invalid Graf registry metadata")?;
    ensure!(registry.version == 1, "unsupported registry version");
    let mut names = BTreeSet::new();
    for entry in &registry.entries {
        ensure!(
            !entry.name.trim().is_empty() && names.insert(&entry.name) && entry.path.is_absolute(),
            "invalid registry entry"
        );
    }
    Ok(registry)
}

// Canonical binding keys are extractor evidence, unlike labels or basenames.
// Keep each source node and its version metadata; this is a relationship only.
fn link_packages(graph: &mut ImportedGraph) -> usize {
    let mut packages = BTreeMap::<&str, Vec<_>>::new();
    for node in &graph.nodes {
        if node.kind != "package" {
            continue;
        }
        let Some(key) = node.binding_key.as_deref() else {
            continue;
        };
        let Some((ecosystem, name)) = key.strip_prefix("package:").and_then(|s| s.split_once(':'))
        else {
            continue;
        };
        if ecosystem.is_empty() || name.is_empty() || key.chars().any(char::is_whitespace) {
            continue;
        }
        packages.entry(key).or_default().push(node);
    }
    let before = graph.edges.len();
    for (key, mut nodes) in packages {
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        for (i, a) in nodes.iter().enumerate() {
            for b in &nodes[i + 1..] {
                let (Some(ap), Some(bp)) = (
                    a.metadata["project"].as_str(),
                    b.metadata["project"].as_str(),
                ) else {
                    continue;
                };
                if ap == bp {
                    continue;
                }
                graph.edges.push(Edge {
                    id: format!("graf:same_package:{}", json!([key, a.id, b.id])),
                    source:a.id.clone(),target:b.id.clone(),relation:"same_package".into(),directed:false,
                    file:None,line:None,confidence:"INFERRED".into(),
                    metadata:json!({"method":"exact_package_binding_key","package_key":key,
                        "sources":[
                            {"project":ap,"original_id":a.metadata["original_id"],"version":a.metadata["original_metadata"]["version"]},
                            {"project":bp,"original_id":b.metadata["original_id"],"version":b.metadata["original_metadata"]["version"]}
                        ]}),
                });
            }
        }
    }
    graph.edges.len() - before
}

const REGISTRY_FINGERPRINT: &str = "graf_registry_fingerprint";

fn aggregate_fingerprint(graph: &ImportedGraph) -> Result<String> {
    let mut nodes: Vec<_> = graph.nodes.iter().collect();
    let mut edges: Vec<_> = graph.edges.iter().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    edges.sort_by(|a, b| a.id.cmp(&b.id));
    let mut metadata = graph.metadata.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.remove(REGISTRY_FINGERPRINT);
    }
    let mut canonical = json!({"nodes":nodes,"edges":edges,"metadata":metadata});
    canonical.sort_all_objects();
    Ok(format!(
        "blake3:{}",
        blake3::hash(&serde_json::to_vec(&canonical)?).to_hex()
    ))
}

fn global(args: &GlobalArgs, db: Option<&Path>, json_output: bool) -> Result<()> {
    let path = global_path(db)?;
    if matches!(args.command, GlobalCommand::Path) {
        return print(&json!({"database":path}), json_output);
    }
    let read_only = matches!(args.command, GlobalCommand::List | GlobalCommand::Query(_));
    let mut store = if path.try_exists()? {
        regular(&path)?;
        Some(if read_only {
            Store::open_read_only(&path)?
        } else {
            Store::open(&path)?
        })
    } else {
        None
    };
    let mut registry = if let Some(store) = &store {
        registry(store)?
    } else {
        Registry {
            version: 1,
            entries: vec![],
        }
    };
    match &args.command {
        GlobalCommand::List => {
            return print(
                &json!({"database":path,"entries":registry.entries,"generation":store.as_ref().map(Store::stats).transpose()?.map(|s|s.generation)}),
                json_output,
            );
        }
        GlobalCommand::Query(args) => {
            ensure!(!args.text.trim().is_empty(), "query text must not be empty");
            let store = store
                .as_ref()
                .context("no global aggregate exists; run graf global add first")?;
            return print(
                &store.query(
                    &args.text,
                    &QueryOptions {
                        depth: args.depth,
                        limit: args.limit as usize,
                        direction: args.direction.into(),
                        relation: args.relation.clone(),
                    },
                )?,
                json_output,
            );
        }
        GlobalCommand::Add {
            name,
            path,
            snapshot,
        } => {
            let entry = registered_source(
                name,
                path,
                if *snapshot {
                    SourceKind::Snapshot
                } else {
                    SourceKind::Database
                },
            )?;
            registry.entries.retain(|entry| entry.name != *name);
            registry.entries.push(entry);
        }
        GlobalCommand::Remove { name } => {
            let before = registry.entries.len();
            registry.entries.retain(|entry| entry.name != *name);
            ensure!(
                registry.entries.len() != before,
                "project name is not registered: {name}"
            );
        }
        GlobalCommand::Refresh => ensure!(
            store.is_some(),
            "no global registry exists; run graf global add first"
        ),
        GlobalCommand::Path => unreachable!(),
    }
    registry.entries.sort_by(|a, b| a.name.cmp(&b.name));
    // Keep the original Store handle open across input reads: its baseline
    // generation makes refresh_import reject a concurrent registry update.
    let mut graph = compose(&registry.entries, &path)?;
    let package_links = link_packages(&mut graph);
    let reference_links = crate::composition::link_references(&mut graph)?;
    graph.metadata["graf_registry"] = serde_json::to_value(&registry)?;
    let fingerprint = aggregate_fingerprint(&graph)?;
    if let Some(store) = &store
        && store.graph_metadata()?[REGISTRY_FINGERPRINT].as_str() == Some(&fingerprint)
    {
        return print(
            &json!({"database":path,"entries":registry.entries,"stats":store.stats()?,
                "unchanged":true,"package_links":package_links,"reference_links":reference_links}),
            json_output,
        );
    }
    graph.metadata[REGISTRY_FINGERPRINT] = json!(fingerprint);
    let stats = if let Some(store) = &mut store {
        store.refresh_import(graph)?
    } else {
        create_database(&path, graph)?
    };
    print(
        &json!({"database":path,"entries":registry.entries,"stats":stats,"unchanged":false,"package_links":package_links,"reference_links":reference_links}),
        json_output,
    )
}

fn diagnose(args: &DiagnoseArgs, db: Option<&Path>, json_output: bool) -> Result<()> {
    let source = SourceArgs {
        snapshot: args.snapshot.clone(),
        analysis: AnalysisArgs {
            community_algorithm: analysis::CommunityAlgorithm::default(),
            community_starts: 1,
            community_seed: 42,
            community_local_max_passes: 100,
            resolution: 1.0,
            max_community_size: None,
            min_cohesion: None,
            exclude_hubs: None,
            include_noise: false,
        },
    };
    let (graph, _) = load(&source, db)?;
    let mut ordered = BTreeMap::<(&str, &str), usize>::new();
    let mut pairs = BTreeMap::<(&str, &str), Vec<&Edge>>::new();
    let mut parallel = BTreeMap::<(bool, &str, &str), usize>::new();
    let mut records = BTreeSet::new();
    let mut duplicate_records = 0;
    for edge in &graph.edges {
        let (a, b) = (edge.source.as_str(), edge.target.as_str());
        let pair = if a <= b { (a, b) } else { (b, a) };
        *ordered.entry((a, b)).or_default() += 1;
        pairs.entry(pair).or_default().push(edge);
        let endpoints = if edge.directed { (a, b) } else { pair };
        *parallel
            .entry((edge.directed, endpoints.0, endpoints.1))
            .or_default() += 1;
        // IDs identify distinct records; compare the remaining stored facts.
        let mut record = edge.clone();
        record.id.clear();
        if !record.directed && a > b {
            std::mem::swap(&mut record.source, &mut record.target);
        }
        if !records.insert(serde_json::to_string(&record)?) {
            duplicate_records += 1;
        }
    }
    let mixed = pairs
        .values()
        .filter(|edges| edges.iter().any(|e| e.directed) && edges.iter().any(|e| !e.directed))
        .count();
    let relation_variants = pairs
        .values()
        .filter(|edges| {
            edges
                .iter()
                .map(|e| &e.relation)
                .collect::<BTreeSet<_>>()
                .len()
                > 1
        })
        .count();
    let mut risks: Vec<_> = pairs.iter().filter(|(_, edges)| edges.len() > 1).collect();
    risks.sort_by(|(a, ae), (b, be)| be.len().cmp(&ae.len()).then(a.cmp(b)));
    let examples: Vec<_> = risks
        .iter()
        .take(args.max_examples as usize)
        .map(|(pair, edges)| {
            let mut samples = edges.to_vec();
            samples.sort_by(|a, b| a.id.cmp(&b.id));
            samples.truncate(5);
            json!({"endpoints":[pair.0,pair.1],"edge_count":edges.len(),
            "undirected_collapse_loss":edges.len()-1,"samples":samples,
            "samples_truncated":samples.len()<edges.len()})
        })
        .collect();
    print(
        &json!({
            "generation":graph.generation,"node_count":graph.nodes.len(),"edge_count":graph.edges.len(),
            "unresolved_reference_count":graph.metadata.get("graf_unresolved_references").and_then(Value::as_array).map(Vec::len),
            "directed_edges":graph.edges.iter().filter(|e| e.directed).count(),
            "undirected_edges":graph.edges.iter().filter(|e| !e.directed).count(),
            "self_loop_edges":graph.edges.iter().filter(|e| e.source==e.target).count(),
            "parallel_edge_groups":parallel.values().filter(|&&n| n>1).count(),
            "parallel_extra_edges":parallel.values().map(|n| n-1).sum::<usize>(),
            "mixed_endpoint_groups":mixed,"relation_variant_groups":relation_variants,
            "duplicate_record_edges":duplicate_records,
            "ordered_unique_endpoint_pairs":ordered.len(),
            "ordered_same_endpoint_collapse_loss":graph.edges.len()-ordered.len(),
            "undirected_unique_endpoint_pairs":pairs.len(),
            "undirected_same_endpoint_collapse_loss":graph.edges.len()-pairs.len(),
            "collapse_risk_groups":risks.len(),"examples":examples,"examples_truncated":examples.len()<risks.len(),
            "methodology":{
                "scope":"Validated stored graph; malformed or dangling input records are rejected during loading. Unresolved references, including external imports, are stored separately and are not dangling edges. Their count is null when no authoritative reference inventory is present. No graph changes are made.",
                "parallel":"Same directed source/target, or same unordered undirected endpoints, with direction kinds kept separate.",
                "collapse":"Hypothetical losses retaining one edge per ordered or unordered endpoint pair, ignoring relation and direction kind. No collapse is performed.",
                "duplicates":"Equal stored edge facts excluding ID; undirected endpoint order is normalized.",
                "examples":"Unordered endpoint groups, largest first then endpoint IDs, with up to five edge samples each."
            }
        }),
        json_output,
    )
}

fn benchmark(args: &BenchmarkArgs, db: Option<&Path>, json_output: bool) -> Result<()> {
    ensure!(
        args.query.len() <= 32,
        "benchmark accepts at most 32 explicit queries"
    );
    ensure!(
        args.query.len() * args.iterations as usize <= 10_000,
        "benchmark accepts at most 10000 measured calls in total"
    );
    ensure!(
        args.query
            .iter()
            .all(|q| !q.trim().is_empty() && q.len() <= 4096),
        "each benchmark query must be nonempty and at most 4096 bytes"
    );
    let path = local_database(db)?;
    regular(&path)?;
    let store = Store::open_read_only(&path)?;
    let stats = store.stats()?;
    let options = QueryOptions {
        depth: args.depth,
        limit: args.limit as usize,
        direction: args.direction.into(),
        relation: args.relation.clone(),
    };
    let mut results = Vec::new();
    for query in &args.query {
        let warmup = store.query(query, &options)?;
        ensure!(
            warmup.generation == stats.generation,
            "graph generation changed during benchmark; retry"
        );
        let mut samples = Vec::new();
        let mut nodes = 0;
        let mut edges = 0;
        let mut unresolved = 0;
        let mut truncated = false;
        for _ in 0..args.iterations {
            let start = Instant::now();
            let result = store.query(query, &options)?;
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            ensure!(
                result.generation == stats.generation,
                "graph generation changed during benchmark; retry"
            );
            nodes += result.nodes.len();
            edges += result.edges.len();
            unresolved += result.unresolved.len();
            truncated |= result.truncated;
        }
        samples.sort_by(f64::total_cmp);
        let n = samples.len();
        let median = if n.is_multiple_of(2) {
            (samples[n / 2 - 1] + samples[n / 2]) / 2.0
        } else {
            samples[n / 2]
        };
        results.push(json!({"query":query,"iterations":args.iterations,"median_ms":median,
            "p95_ms":samples[(95*n).div_ceil(100)-1],"min_ms":samples[0],"max_ms":samples[n-1],
            "result":{"nodes":warmup.nodes.len(),"edges":warmup.edges.len(),"unresolved":warmup.unresolved.len(),"truncated":warmup.truncated},
            "measured_totals":{"nodes":nodes,"edges":edges,"unresolved":unresolved,"any_truncated":truncated}}));
    }
    print(
        &json!({"schema_version":1,"graf_version":env!("CARGO_PKG_VERSION"),"generation":stats.generation,
        "graph":{"kind":stats.kind,"nodes":stats.nodes,"edges":stats.edges},"options":options,
        "iterations_per_query":args.iterations,"warmup_calls_per_query":1,
        "measured_calls":args.iterations as usize*args.query.len(),"queries":results,
        "methodology":"One read-only SQLite connection; one unmeasured warm-up per query. Timings include SQL search/traversal and result construction, excluding database open, warm-up, and output serialization. Median averages middle values; p95 uses nearest rank. Cache and host load affect results; no external-system comparison."}),
        json_output,
    )
}

const LABEL_BYTES: u64 = 8 * 1024 * 1024;
const LABEL_SIGNATURE: &str = "blake3-sorted-json-members-v1";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CommunityLabel {
    community_id: usize,
    members: Vec<String>,
    label: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LabelFile {
    schema_version: u32,
    generation: u64,
    signature_algorithm: String,
    labels: BTreeMap<String, CommunityLabel>,
}

fn member_signature(members: &[String]) -> Result<String> {
    // JSON strings are unambiguous even for IDs containing delimiters or NULs.
    Ok(blake3::hash(&serde_json::to_vec(members)?)
        .to_hex()
        .to_string())
}

fn read_labels(path: &Path) -> Result<LabelFile> {
    regular(path)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(LABEL_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= LABEL_BYTES,
        "label file exceeds 8 MiB"
    );
    let mut file: LabelFile = serde_json::from_slice(&bytes).context("invalid Graf label file")?;
    ensure!(
        file.schema_version == 1 && file.signature_algorithm == LABEL_SIGNATURE,
        "unsupported Graf label schema or signature algorithm"
    );
    for (signature, entry) in &mut file.labels {
        entry.members.sort();
        ensure!(
            !entry.members.is_empty() && !entry.members.windows(2).any(|m| m[0] == m[1]),
            "label membership must be nonempty and contain no duplicates"
        );
        ensure!(
            *signature == member_signature(&entry.members)?,
            "label membership signature does not match its members"
        );
    }
    Ok(file)
}

fn label(args: &LabelArgs, db: Option<&Path>, json_output: bool) -> Result<()> {
    let (graph, source) = load(&args.source, db)?;
    let exists = destination(&args.output, std::slice::from_ref(&source))?;
    let input = args
        .input
        .as_deref()
        .or_else(|| exists.then_some(args.output.as_path()));
    let previous = input.map(read_labels).transpose()?;
    let report = analysis::analyze(&graph, &args.source.analysis.options())?;
    let mut labels = BTreeMap::new();
    let mut reused = 0;
    for community in report.communities {
        let mut members = community.nodes;
        members.sort();
        let signature = member_signature(&members)?;
        let prior = previous
            .as_ref()
            .and_then(|file| file.labels.get(&signature))
            .filter(|entry| entry.members == members);
        let label = if let Some(prior) = prior {
            reused += 1;
            prior.label.clone()
        } else {
            community.label
        };
        labels.insert(
            signature,
            CommunityLabel {
                community_id: community.id,
                members,
                label,
            },
        );
    }
    let count = labels.len();
    let file = LabelFile {
        schema_version: 1,
        generation: graph.generation,
        signature_algorithm: LABEL_SIGNATURE.into(),
        labels,
    };
    let bytes = serde_json::to_vec_pretty(&file)?;
    ensure!(
        bytes.len() as u64 <= LABEL_BYTES,
        "label output exceeds 8 MiB"
    );
    write_atomic(&args.output, &bytes, &[source])?;
    print(
        &json!({"output":args.output,"generation":graph.generation,"communities":count,
        "reused":reused,"generated":count-reused,"signature_algorithm":LABEL_SIGNATURE}),
        json_output,
    )
}

/// Execute an explicitly selected workflow. `db` is the parent's optional global
/// --db argument; this module resolves local/default-global paths as appropriate.
pub fn run(args: &Command, db: Option<&Path>, json_output: bool) -> Result<()> {
    match args {
        Command::SaveResult(args) => {
            let graph = optional_graph(args.snapshot.as_deref(), db)?;
            let path = crate::memory::save_result(&args.input, graph.as_ref())?;
            print(&json!({"saved":path}), json_output)
        }
        Command::Reflect(args) => {
            let graph = optional_graph(args.snapshot.as_deref(), db)?;
            let result = crate::memory::reflect(&args.input, graph.as_ref())?;
            print(&serde_json::to_value(result)?, json_output)
        }
        Command::Prs(args) => {
            let graph = optional_graph(args.snapshot.as_deref(), db)?;
            let report = crate::prs::run(&args.input, graph.as_ref())?;
            if json_output {
                print(&report, true)
            } else {
                writeln!(
                    std::io::stdout().lock(),
                    "{}",
                    crate::human(&crate::prs::format_text(&report))
                )?;
                Ok(())
            }
        }
        Command::Benchmark(args) => benchmark(args, db, json_output),
        Command::Diagnose(args) => diagnose(args, db, json_output),
        Command::Label(args) => label(args, db, json_output),
        Command::Tree(args) => export_graph(
            &args.source,
            db,
            Format::TreeHtml,
            args.output.as_deref(),
            json_output,
            &args.view,
            None,
        ),
        Command::Export(args) => export_graph(
            &args.source,
            db,
            args.format,
            args.output.as_deref(),
            json_output,
            &args.view,
            None,
        ),
        Command::Report(args) => export_graph(
            &args.source,
            db,
            args.format,
            args.output.as_deref(),
            json_output,
            &args.view,
            Some(args.check_freshness),
        ),
        Command::Merge(args) => merge(args, db, json_output),
        Command::Global(args) => global(args, db, json_output),
        Command::Analyze(args) => {
            let (graph, source) = load(&args.source, db)?;
            let report = analysis::analyze(&graph, &args.source.analysis.options())?;
            if let Some(path) = &args.output {
                let content = serde_json::to_vec_pretty(&report)?;
                write_atomic(path, &content, &[source])?;
                print(
                    &json!({"output":path,"generation":report.generation,"nodes":report.nodes.len(),"communities":report.communities.len()}),
                    json_output,
                )
            } else {
                print(&report, json_output)
            }
        }
        Command::Communities(args) => {
            let (graph, _) = load(&args.source, db)?;
            let mut report = analysis::analyze(&graph, &args.source.analysis.options())?;
            if let Some(id) = args.id {
                report.communities.retain(|c| c.id == id);
                ensure!(
                    !report.communities.is_empty(),
                    "community {id} does not exist in this snapshot"
                );
            }
            print(
                &json!({"generation":graph.generation,"algorithm":report.community_algorithm,"modularity":report.community_modularity,"converged":report.community_converged,"communities":report.communities,"excluded_hubs":report.excluded_hubs,"noise_filtered_hubs":report.noise_filtered_hubs,"community_split_attempts":report.community_split_attempts,"unsatisfied_community_constraints":report.unsatisfied_community_constraints}),
                json_output,
            )
        }
        Command::Hubs(args) => {
            let (graph, _) = load(&args.source, db)?;
            let mut report = analysis::analyze(&graph, &args.source.analysis.options())?;
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
            print(
                &json!({"generation":graph.generation,"total_nodes":graph.nodes.len(),"eligible_nodes":eligible.len(),"truncated":hubs.len()<eligible.len(),"pagerank_converged":report.pagerank_converged,"hubs":hubs,"excluded_hubs":report.excluded_hubs,"noise_filtered_hubs":report.noise_filtered_hubs}),
                json_output,
            )
        }
    }
}

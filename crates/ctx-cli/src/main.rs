use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use uuid::Uuid;
use work_record_capture::{
    import_provider_fixture_jsonl, import_spool, inbox_dir as capture_inbox_dir,
    retry_failed_spool_files, spool_counts, write_fixture, write_shim_command, FixtureOptions,
    ProviderFixtureImportOptions, ProviderImportSummary, ShimCommandOptions,
};
use work_record_core::{
    blob_dir, database_path, default_data_root, device_path, inbox_dir, new_id, work_record_dir,
    Artifact, CaptureProvider, EntityTimestamps, Evidence, EvidenceMetadata, Fidelity, FileTouched,
    PullRequest, Run, Session, Summary, SummaryKind, SyncMetadata, VcsChange, VcsWorkspace,
    WorkRecord, WorkRecordArchive,
};
use work_record_publish::{
    render_pr_comment, PullRequestTarget, RawTranscriptOptIn, RenderOptions,
};
use work_record_store::Store;
use work_record_vcs::{
    inspect_path, parse_pull_request_url, GitDetection, GitStatus, JjCommit, JjDetection,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const DEFAULT_EVIDENCE_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_EVIDENCE_TIMEOUT_SECONDS: u64 = 300;
const DEFAULT_SHIM_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const TIMEOUT_EXIT_CODE: i32 = 124;

#[derive(Debug, Parser)]
#[command(name = "ctx", about = "Work Recorder command line")]
struct Cli {
    #[arg(long, env = "CTX_DATA_ROOT", global = true)]
    data_root: Option<PathBuf>,
    #[command(subcommand)]
    command: CommandRoot,
}

#[derive(Debug, Subcommand)]
enum CommandRoot {
    #[command(about = "Create the local Work Recorder data store")]
    Setup,
    #[command(about = "Show local Work Recorder workspace status")]
    Status,
    #[command(about = "Remove local Work Recorder product data")]
    Uninstall {
        #[arg(long)]
        yes: bool,
    },
    #[command(about = "Print the local SQLite schema")]
    Schema,
    #[command(about = "Create a work record")]
    Record(RecordArgs),
    #[command(about = "List recent work records")]
    List(ListArgs),
    #[command(about = "Show one work record")]
    Show(IdArgs),
    #[command(about = "Search work records")]
    Search(SearchArgs),
    #[command(about = "Render work context for a query")]
    Context(ContextArgs),
    #[command(about = "Summarize recorded work")]
    Report(ReportArgs),
    #[command(about = "Export a local static Work Recorder dashboard")]
    Dashboard(DashboardCommand),
    #[command(about = "Capture evidence for work records")]
    Evidence(EvidenceCommand),
    #[command(about = "Import passive capture spool events")]
    Capture(CaptureCommand),
    #[command(about = "Install local git/jj/gh capture shims")]
    Shim(ShimCommand),
    #[command(about = "Inspect local VCS workspace metadata")]
    Vcs(VcsCommand),
    #[command(about = "Parse pull request URLs")]
    Pr(PrCommand),
    #[command(about = "Publish Work Recorder finished-product output")]
    Publish(PublishCommand),
    #[command(about = "Attach a pull request URL to a work record")]
    LinkPr(LinkPrArgs),
    #[command(about = "Export work records and evidence as JSON")]
    Export(ExportArgs),
    #[command(about = "Import work records and evidence from JSON")]
    Import(ImportArgs),
    #[command(about = "Validate local Work Recorder storage")]
    Validate,
    #[command(about = "Check local Work Recorder health")]
    Doctor(DoctorArgs),
    #[command(about = "Retry failed local capture imports")]
    Repair(RepairArgs),
    #[command(hide = true, about = "Compatibility alias for setup/status/uninstall")]
    Workspace(WorkspaceCommand),
    #[command(
        hide = true,
        about = "Compatibility alias for record/search/report commands"
    )]
    Work(WorkCommand),
}

#[derive(Debug, Args)]
struct WorkspaceCommand {
    #[command(subcommand)]
    command: WorkspaceSubcommand,
}

#[derive(Debug, Subcommand)]
enum WorkspaceSubcommand {
    #[command(about = "Create the local Work Recorder data store")]
    Setup,
    #[command(about = "Show local Work Recorder workspace status")]
    Status,
    #[command(about = "Remove local Work Recorder product data")]
    Uninstall {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
struct WorkCommand {
    #[command(subcommand)]
    command: WorkSubcommand,
}

#[derive(Debug, Subcommand)]
enum WorkSubcommand {
    #[command(about = "Print the local SQLite schema")]
    Schema,
    #[command(about = "Create a work record")]
    Record(RecordArgs),
    #[command(about = "List recent work records")]
    List(ListArgs),
    #[command(about = "Show one work record")]
    Show(IdArgs),
    #[command(about = "Search work records")]
    Search(SearchArgs),
    #[command(about = "Render work context for a query")]
    Context(ContextArgs),
    #[command(about = "Summarize recorded work")]
    Report(ReportArgs),
    #[command(about = "Capture evidence for work records")]
    Evidence(EvidenceCommand),
    #[command(about = "Attach a pull request URL to a work record")]
    LinkPr(LinkPrArgs),
    #[command(about = "Export work records and evidence as JSON")]
    Export(ExportArgs),
    #[command(about = "Import work records and evidence from JSON")]
    Import(ImportArgs),
    #[command(about = "Validate local Work Recorder storage")]
    Validate,
    #[command(about = "Check local Work Recorder health")]
    Doctor(DoctorArgs),
    #[command(about = "Retry failed local capture imports")]
    Repair(RepairArgs),
}

#[derive(Debug, Args)]
struct RecordArgs {
    #[arg(long)]
    title: String,
    #[arg(long, default_value = "")]
    body: String,
    #[arg(long = "tag")]
    tags: Vec<String>,
    #[arg(long, default_value = "note")]
    kind: String,
    #[arg(long)]
    workspace: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct IdArgs {
    id: Uuid,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SearchArgs {
    query: String,
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ContextArgs {
    query: Option<String>,
    #[arg(long, default_value_t = 10)]
    limit: usize,
    #[arg(long, default_value_t = work_record_search::DEFAULT_MAX_TOKENS)]
    max_tokens: u32,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ReportArgs {
    #[arg(long, default_value_t = 1000)]
    limit: usize,
    #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
    format: ReportFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ReportFormat {
    Text,
    Json,
}

#[derive(Debug, Args)]
struct DashboardCommand {
    #[command(subcommand)]
    command: DashboardSubcommand,
}

#[derive(Debug, Subcommand)]
enum DashboardSubcommand {
    #[command(about = "Export a static local HTML dashboard")]
    Export(DashboardExportArgs),
}

#[derive(Debug, Args)]
struct DashboardExportArgs {
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 1000)]
    limit: usize,
}

#[derive(Debug, Args)]
struct EvidenceCommand {
    #[command(subcommand)]
    command: EvidenceSubcommand,
}

#[derive(Debug, Subcommand)]
enum EvidenceSubcommand {
    #[command(about = "Run a command and store its output as evidence")]
    Run(EvidenceRunArgs),
}

#[derive(Debug, Args)]
struct EvidenceRunArgs {
    #[arg(long)]
    record: Option<Uuid>,
    #[arg(long, default_value_t = DEFAULT_EVIDENCE_MAX_OUTPUT_BYTES)]
    max_output_bytes: usize,
    #[arg(long, default_value_t = DEFAULT_EVIDENCE_TIMEOUT_SECONDS)]
    timeout_seconds: u64,
    #[arg(required = true, trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct CaptureCommand {
    #[command(subcommand)]
    command: CaptureSubcommand,
}

#[derive(Debug, Subcommand)]
enum CaptureSubcommand {
    #[command(hide = true, about = "Write one capture fixture to the JSONL spool")]
    WriteFixture(CaptureWriteFixtureArgs),
    #[command(hide = true, about = "Write one local shim command to the JSONL spool")]
    WriteShimCommand(CaptureWriteShimCommandArgs),
    #[command(about = "Import pending capture spool files")]
    Import(CaptureImportArgs),
    #[command(about = "Import a provider fixture JSONL transcript")]
    ImportProvider(CaptureImportProviderArgs),
}

#[derive(Debug, Args)]
struct CaptureWriteFixtureArgs {
    #[arg(long, default_value = "Fixture capture")]
    title: String,
    #[arg(long, default_value = "fixture body")]
    body: String,
    #[arg(long = "tag")]
    tags: Vec<String>,
    #[arg(long)]
    dedupe_key: Option<String>,
    #[arg(long)]
    machine_id: Option<String>,
    #[arg(long)]
    cwd: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct CaptureImportArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct CaptureImportProviderArgs {
    #[arg(long, value_enum)]
    provider: ProviderFixtureProvider,
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProviderFixtureProvider {
    Codex,
    Claude,
    Pi,
}

impl ProviderFixtureProvider {
    fn capture_provider(self) -> CaptureProvider {
        match self {
            Self::Codex => CaptureProvider::Codex,
            Self::Claude => CaptureProvider::Claude,
            Self::Pi => CaptureProvider::Pi,
        }
    }

    fn as_str(self) -> &'static str {
        self.capture_provider().as_str()
    }
}

#[derive(Debug, Args)]
struct CaptureWriteShimCommandArgs {
    #[arg(long, value_enum)]
    provider: ShimTool,
    #[arg(long)]
    exit_code: i32,
    #[arg(long)]
    stdout_file: PathBuf,
    #[arg(long)]
    stderr_file: PathBuf,
    #[arg(long)]
    started_at: String,
    #[arg(long)]
    duration_ms: i64,
    #[arg(long)]
    machine_id: Option<String>,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long)]
    real_command: Option<PathBuf>,
    #[arg(long)]
    shim_dir: Option<PathBuf>,
    #[arg(required = true, trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct ShimCommand {
    #[command(subcommand)]
    command: ShimSubcommand,
}

#[derive(Debug, Subcommand)]
enum ShimSubcommand {
    #[command(about = "Create local git/jj/gh wrapper scripts")]
    Install(ShimDirArgs),
    #[command(about = "Print shell exports for local wrapper scripts")]
    Env(ShimDirArgs),
    #[command(about = "Remove local wrapper scripts created by ctx")]
    Uninstall(ShimDirArgs),
}

#[derive(Debug, Args)]
struct ShimDirArgs {
    #[arg(long)]
    dir: PathBuf,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ShimTool {
    Git,
    Jj,
    Gh,
}

impl ShimTool {
    const ALL: [Self; 3] = [Self::Git, Self::Jj, Self::Gh];

    fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Jj => "jj",
            Self::Gh => "gh",
        }
    }

    fn provider(self) -> CaptureProvider {
        match self {
            Self::Git => CaptureProvider::Git,
            Self::Jj => CaptureProvider::Jj,
            Self::Gh => CaptureProvider::Gh,
        }
    }
}

#[derive(Debug, Args)]
struct LinkPrArgs {
    id: Uuid,
    pr_url: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct VcsCommand {
    #[command(subcommand)]
    command: VcsSubcommand,
}

#[derive(Debug, Subcommand)]
enum VcsSubcommand {
    #[command(about = "Inspect Git and jj workspace metadata")]
    Inspect(VcsInspectArgs),
}

#[derive(Debug, Args)]
struct VcsInspectArgs {
    #[arg(default_value = ".")]
    path: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct PrCommand {
    #[command(subcommand)]
    command: PrSubcommand,
}

#[derive(Debug, Subcommand)]
enum PrSubcommand {
    #[command(about = "Parse a GitHub/GitLab pull request URL")]
    Parse(PrParseArgs),
}

#[derive(Debug, Args)]
struct PrParseArgs {
    url: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct PublishCommand {
    #[command(subcommand)]
    command: PublishSubcommand,
}

#[derive(Debug, Subcommand)]
enum PublishSubcommand {
    #[command(about = "Render or publish a marker-bounded pull request comment")]
    PrComment(PublishPrCommentArgs),
}

#[derive(Debug, Args)]
struct PublishPrCommentArgs {
    record_id: Uuid,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    include_raw_transcript: bool,
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ImportArgs {
    #[arg(long)]
    input: Option<PathBuf>,
    #[arg(long)]
    overwrite: bool,
}

#[derive(Debug, Args)]
struct RepairArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Args)]
struct DoctorArgs {
    #[arg(long)]
    privacy: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_root = cli
        .data_root
        .clone()
        .map(Ok)
        .unwrap_or_else(default_data_root)
        .context("resolve ctx data root")?;

    match cli.command {
        CommandRoot::Setup => run_workspace_subcommand(WorkspaceSubcommand::Setup, data_root),
        CommandRoot::Status => run_workspace_subcommand(WorkspaceSubcommand::Status, data_root),
        CommandRoot::Uninstall { yes } => {
            run_workspace_subcommand(WorkspaceSubcommand::Uninstall { yes }, data_root)
        }
        CommandRoot::Schema => run_work_subcommand(WorkSubcommand::Schema, data_root),
        CommandRoot::Record(args) => run_work_subcommand(WorkSubcommand::Record(args), data_root),
        CommandRoot::List(args) => run_work_subcommand(WorkSubcommand::List(args), data_root),
        CommandRoot::Show(args) => run_work_subcommand(WorkSubcommand::Show(args), data_root),
        CommandRoot::Search(args) => run_work_subcommand(WorkSubcommand::Search(args), data_root),
        CommandRoot::Context(args) => run_work_subcommand(WorkSubcommand::Context(args), data_root),
        CommandRoot::Report(args) => run_work_subcommand(WorkSubcommand::Report(args), data_root),
        CommandRoot::Dashboard(args) => run_dashboard(args, data_root),
        CommandRoot::Evidence(args) => {
            run_work_subcommand(WorkSubcommand::Evidence(args), data_root)
        }
        CommandRoot::Capture(args) => run_capture(args, data_root),
        CommandRoot::Shim(args) => run_shim(args),
        CommandRoot::Vcs(args) => run_vcs(args),
        CommandRoot::Pr(args) => run_pr(args),
        CommandRoot::Publish(args) => run_publish(args, data_root),
        CommandRoot::LinkPr(args) => run_work_subcommand(WorkSubcommand::LinkPr(args), data_root),
        CommandRoot::Export(args) => run_work_subcommand(WorkSubcommand::Export(args), data_root),
        CommandRoot::Import(args) => run_work_subcommand(WorkSubcommand::Import(args), data_root),
        CommandRoot::Validate => run_work_subcommand(WorkSubcommand::Validate, data_root),
        CommandRoot::Doctor(args) => run_work_subcommand(WorkSubcommand::Doctor(args), data_root),
        CommandRoot::Repair(args) => run_work_subcommand(WorkSubcommand::Repair(args), data_root),
        CommandRoot::Workspace(command) => run_workspace(command, data_root),
        CommandRoot::Work(command) => run_work(command, data_root),
    }
}

fn run_vcs(command: VcsCommand) -> Result<()> {
    match command.command {
        VcsSubcommand::Inspect(args) => {
            let inspection = inspect_path(args.path)?;
            if args.json {
                print_json(serde_json::json!({
                    "schema_version": 1,
                    "inspection": inspection,
                }))?;
            } else {
                print_git_detection(&inspection.git);
                print_jj_detection(&inspection.jj);
            }
        }
    }
    Ok(())
}

fn run_pr(command: PrCommand) -> Result<()> {
    match command.command {
        PrSubcommand::Parse(args) => {
            let parsed = parse_pull_request_url(&args.url)?;
            if args.json {
                print_json(serde_json::json!({
                    "schema_version": 1,
                    "pull_request": parsed,
                }))?;
            } else {
                println!(
                    "{} {}/{} #{}",
                    parsed.provider, parsed.owner, parsed.repo, parsed.number
                );
                println!("url: {}", parsed.normalized_url);
                println!("confidence: {}", parsed.confidence);
            }
        }
    }
    Ok(())
}

fn print_git_detection(git: &GitDetection) {
    if !git.available {
        println!(
            "git: unavailable ({})",
            git.error.as_deref().unwrap_or("unknown error")
        );
        return;
    }
    let Some(workspace) = &git.workspace else {
        println!(
            "git: no workspace ({})",
            git.error.as_deref().unwrap_or("not a git workspace")
        );
        return;
    };

    println!("git: {}", workspace.root_path);
    if let Some(branch) = &workspace.branch {
        println!("branch: {branch}");
    }
    if let Some(head_sha) = &workspace.head_sha {
        println!("head: {}", short_id(head_sha));
    }
    if let Some(upstream) = &workspace.upstream {
        println!(
            "upstream: {} (ahead {}, behind {})",
            upstream.name, upstream.ahead, upstream.behind
        );
    }
    print_git_status("status", &workspace.status);
    println!("fingerprint: {}", workspace.repo_fingerprint.value);
    if let Some(remote) = &workspace.primary_remote {
        println!("remote: {} {}", remote.name, remote.redacted_url);
    }
    if !workspace.recent_commits.is_empty() {
        println!("recent_commits:");
        for commit in workspace.recent_commits.iter().take(5) {
            println!("  {} {}", commit.short_sha, commit.summary);
        }
    }
    if workspace.is_worktree {
        println!("worktree: true");
    }
}

fn print_jj_detection(jj: &JjDetection) {
    if !jj.available {
        println!(
            "jj: unavailable ({})",
            jj.error.as_deref().unwrap_or("unknown error")
        );
        return;
    }
    match &jj.workspace {
        Some(workspace) => {
            println!("jj: {}", workspace.root_path);
            if let Some(working_copy) = &workspace.working_copy {
                print_jj_commit("working_copy", working_copy);
            }
            if !workspace.parents.is_empty() {
                println!("parents:");
                for parent in &workspace.parents {
                    print_jj_commit("  parent", parent);
                }
            }
            if !workspace.bookmarks.is_empty() {
                println!("bookmarks:");
                for bookmark in workspace.bookmarks.iter().take(8) {
                    let remote = if bookmark.remote { " remote" } else { "" };
                    let target = bookmark
                        .change_id
                        .as_deref()
                        .or(bookmark.commit_id.as_deref())
                        .map(short_id)
                        .unwrap_or_else(|| "unknown".to_owned());
                    println!("  {} -> {}{}", bookmark.name, target, remote);
                }
            }
            if !workspace.recent_changes.is_empty() {
                println!("recent_changes:");
                for change in workspace.recent_changes.iter().take(5) {
                    print_jj_commit("  change", change);
                }
            }
            if let Some(git) = &workspace.colocated_git {
                println!("colocated_git: {}", git.root_path);
                if let Some(branch) = &git.branch {
                    println!("colocated_git_branch: {branch}");
                }
                if let Some(head_sha) = &git.head_sha {
                    println!("colocated_git_head: {}", short_id(head_sha));
                }
                print_git_status("colocated_git_status", &git.status);
                if git.is_worktree {
                    println!("colocated_git_worktree: true");
                }
            }
        }
        None => println!(
            "jj: no workspace ({})",
            jj.error.as_deref().unwrap_or("not a jj workspace")
        ),
    }
}

fn print_git_status(label: &str, status: &GitStatus) {
    println!(
        "{label}: dirty={} staged={} unstaged={} untracked={} conflicted={}",
        status.dirty, status.staged, status.unstaged, status.untracked, status.conflicted
    );
    if !status.entries.is_empty() {
        println!("{label}_entries:");
        for entry in status.entries.iter().take(8) {
            match &entry.original_path {
                Some(original) => println!(
                    "  {}{} {} <- {}",
                    entry.index_status, entry.worktree_status, entry.path, original
                ),
                None => println!(
                    "  {}{} {}",
                    entry.index_status, entry.worktree_status, entry.path
                ),
            }
        }
    }
}

fn print_jj_commit(label: &str, commit: &JjCommit) {
    let id = commit
        .short_commit_id
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| short_id(&commit.commit_id));
    let description = commit.description.as_deref().unwrap_or("(no description)");
    if commit.bookmarks.is_empty() {
        println!(
            "{label}: {} {} {}",
            short_id(&commit.change_id),
            id,
            description
        );
    } else {
        println!(
            "{label}: {} {} {} [{}]",
            short_id(&commit.change_id),
            id,
            description,
            commit.bookmarks.join(", ")
        );
    }
}

fn short_id(value: &str) -> String {
    value.chars().take(12).collect()
}

fn run_publish(command: PublishCommand, data_root: PathBuf) -> Result<()> {
    let mut store = Store::open(database_path(data_root.clone()))?;
    auto_import_pending_spool(&data_root, &mut store)?;
    match command.command {
        PublishSubcommand::PrComment(args) => run_publish_pr_comment(args, &store),
    }
}

fn run_publish_pr_comment(args: PublishPrCommentArgs, store: &Store) -> Result<()> {
    let record = store.get_record(args.record_id)?;
    let evidence = store.evidence_for_record(record.id)?;
    let pr_url = record.pr_url.as_deref().ok_or_else(|| {
        anyhow!(
            "record {} has no linked pull request; run `ctx link-pr {} <pr-url>` first",
            record.id,
            record.id
        )
    })?;
    let target = PullRequestTarget::github_from_url(pr_url)?;
    let options = RenderOptions {
        raw_transcript: args
            .include_raw_transcript
            .then(|| RawTranscriptOptIn::acknowledge_private_data_risk("ctx CLI opt-in flag"))
            .transpose()?,
    };
    let rendered = render_pr_comment(&[record], &evidence, &options);

    if args.dry_run {
        print!("{}", rendered.markdown);
        return Ok(());
    }

    let token = env::var("GITHUB_TOKEN")
        .or_else(|_| env::var("GH_TOKEN"))
        .map(|value| value.trim().to_owned())
        .unwrap_or_default();
    if token.is_empty() {
        return Err(anyhow!(
            "live GitHub PR comment publishing requires GITHUB_TOKEN or GH_TOKEN; rerun with --dry-run to render locally"
        ));
    }

    Err(anyhow!(
        "live GitHub PR comment publishing for {}/{}#{} requires an HTTP client integration that is not available yet; rerun with --dry-run to render locally",
        target.owner,
        target.repo,
        target.number
    ))
}

fn run_workspace(command: WorkspaceCommand, data_root: PathBuf) -> Result<()> {
    run_workspace_subcommand(command.command, data_root)
}

fn run_workspace_subcommand(command: WorkspaceSubcommand, data_root: PathBuf) -> Result<()> {
    match command {
        WorkspaceSubcommand::Setup => {
            let db_path = database_path(data_root);
            let store = Store::open(&db_path)?;
            println!("Work Recorder workspace ready");
            println!("database: {}", store.path().display());
        }
        WorkspaceSubcommand::Status => {
            let db_path = database_path(data_root.clone());
            let capture_inbox = capture_inbox_dir(&data_root);
            let counts = spool_counts(&capture_inbox)?;
            println!("data_root: {}", data_root.display());
            println!(
                "work_record_dir: {}",
                work_record_dir(data_root.clone()).display()
            );
            println!("blob_dir: {}", blob_dir(data_root.clone()).display());
            println!("inbox_dir: {}", inbox_dir(data_root.clone()).display());
            println!("device_path: {}", device_path(data_root.clone()).display());
            println!("database: {}", db_path.display());
            println!("initialized: {}", db_path.exists());
            println!("spool_pending: {}", counts.pending);
            println!("spool_tmp: {}", counts.tmp);
            println!("spool_processing: {}", counts.processing);
            println!("spool_done: {}", counts.done);
            println!("spool_failed: {}", counts.failed);
        }
        WorkspaceSubcommand::Uninstall { yes } => {
            if !yes {
                return Err(anyhow!("refusing to uninstall without --yes"));
            }
            let dir = work_record_dir(data_root);
            if dir.exists() {
                fs::remove_dir_all(&dir)?;
            }
            println!("removed {}", dir.display());
        }
    }
    Ok(())
}

fn run_work(command: WorkCommand, data_root: PathBuf) -> Result<()> {
    run_work_subcommand(command.command, data_root)
}

fn run_work_subcommand(command: WorkSubcommand, data_root: PathBuf) -> Result<()> {
    let mut store = Store::open(database_path(data_root.clone()))?;
    auto_import_pending_spool(&data_root, &mut store)?;
    match command {
        WorkSubcommand::Schema => println!("{}", store.schema()?),
        WorkSubcommand::Record(args) => {
            let body = read_body(args.body)?;
            let record = WorkRecord::new(args.title, body, args.tags, args.kind, args.workspace);
            store.insert_record(&record)?;
            print_record(&record, args.json)?;
        }
        WorkSubcommand::List(args) => {
            let records = store.list_records(args.limit)?;
            print_records(&records, args.json)?;
        }
        WorkSubcommand::Show(args) => {
            let record = store.get_record(args.id)?;
            print_record(&record, args.json)?;
        }
        WorkSubcommand::Search(args) => {
            if args.json {
                let packet = work_record_search::search_packet(
                    &store,
                    &args.query,
                    &packet_options(args.limit, None),
                )?;
                println!("{}", serde_json::to_string_pretty(&packet)?);
            } else {
                let records = store.search_records(&args.query, args.limit)?;
                print_records(&records, false)?;
            }
        }
        WorkSubcommand::Context(args) => {
            if args.json {
                let packet = work_record_search::context_packet(
                    &store,
                    args.query.as_deref(),
                    &packet_options(args.limit, Some(args.max_tokens)),
                )?;
                println!("{}", serde_json::to_string_pretty(&packet)?);
            } else {
                let context = store.context(args.query.as_deref(), args.limit)?;
                println!("{}", work_record_report::context_markdown(&context));
            }
        }
        WorkSubcommand::Report(args) => {
            let data = load_dashboard_data(&store, args.limit)?;
            let report = data.report();
            match args.format {
                ReportFormat::Text => {
                    print!(
                        "{}",
                        work_record_report::render_text(&data.records, &data.evidence)
                    );
                    if data.has_rich_sections() {
                        print!(
                            "\n{}",
                            work_record_report::render_evidence_report_markdown(&report)
                        );
                    }
                }
                ReportFormat::Json => {
                    let summary = work_record_report::summarize(&data.records, &data.evidence);
                    let report_v2: serde_json::Value = serde_json::from_str(
                        &work_record_report::render_evidence_report_json(&report)?,
                    )?;
                    print_json(serde_json::json!({
                        "schema_version": 1,
                        "summary": summary,
                        "report_v2": report_v2,
                    }))?;
                }
            }
        }
        WorkSubcommand::Evidence(args) => run_evidence(args, &store)?,
        WorkSubcommand::LinkPr(args) => {
            let record = store.link_pr(args.id, &args.pr_url)?;
            print_record(&record, args.json)?;
        }
        WorkSubcommand::Export(args) => {
            let json = work_record_report::archive_json(&store.export_archive()?)?;
            if let Some(path) = args.output {
                fs::write(path, json)?;
            } else {
                println!("{json}");
            }
        }
        WorkSubcommand::Import(args) => {
            let json = match args.input {
                Some(path) => fs::read_to_string(path)?,
                None => {
                    let mut input = String::new();
                    io::stdin().read_to_string(&mut input)?;
                    input
                }
            };
            let archive: WorkRecordArchive = serde_json::from_str(&json)?;
            let record_count = archive.records.len();
            let evidence_count = archive.evidence.len();
            store.import_archive(&archive, args.overwrite)?;
            println!("imported {record_count} records and {evidence_count} evidence items");
        }
        WorkSubcommand::Validate => print_doctor_findings(&store, &data_root)?,
        WorkSubcommand::Doctor(args) => {
            if args.privacy {
                print_privacy_doctor(&store, &data_root)?;
            } else {
                print_doctor_findings(&store, &data_root)?;
            }
        }
        WorkSubcommand::Repair(args) => run_repair(args, &mut store, &data_root)?,
    }
    Ok(())
}

fn run_dashboard(command: DashboardCommand, data_root: PathBuf) -> Result<()> {
    match command.command {
        DashboardSubcommand::Export(args) => {
            let mut store = Store::open(database_path(data_root.clone()))?;
            auto_import_pending_spool(&data_root, &mut store)?;
            let data = load_dashboard_data(&store, args.limit)?;
            let report = data.report();
            let html = work_record_report::render_dashboard_html_report(&report);
            fs::create_dir_all(&args.output)?;
            let index = args.output.join("index.html");
            fs::write(&index, html)?;
            println!("dashboard: {}", index.display());
        }
    }
    Ok(())
}

struct DashboardData {
    records: Vec<WorkRecord>,
    evidence: Vec<Evidence>,
    sessions: Vec<Session>,
    runs: Vec<Run>,
    events: Vec<work_record_core::Event>,
    vcs_workspaces: Vec<VcsWorkspace>,
    vcs_changes: Vec<VcsChange>,
    pull_requests: Vec<PullRequest>,
    artifacts: Vec<Artifact>,
    evidence_metadata: Vec<EvidenceMetadata>,
    files_touched: Vec<FileTouched>,
    summaries: Vec<Summary>,
}

impl DashboardData {
    fn report(&self) -> work_record_report::DashboardReport<'_> {
        work_record_report::DashboardReport {
            records: &self.records,
            evidence: &self.evidence,
            archive_artifacts: &[],
            sessions: &self.sessions,
            runs: &self.runs,
            events: &self.events,
            vcs_workspaces: &self.vcs_workspaces,
            vcs_changes: &self.vcs_changes,
            pull_requests: &self.pull_requests,
            artifacts: &self.artifacts,
            evidence_metadata: &self.evidence_metadata,
            files_touched: &self.files_touched,
            summaries: &self.summaries,
        }
    }

    fn has_rich_sections(&self) -> bool {
        !self.sessions.is_empty()
            || !self.runs.is_empty()
            || !self.events.is_empty()
            || !self.vcs_workspaces.is_empty()
            || !self.vcs_changes.is_empty()
            || !self.pull_requests.is_empty()
            || !self.artifacts.is_empty()
            || !self.evidence_metadata.is_empty()
            || !self.files_touched.is_empty()
            || !self.summaries.is_empty()
    }
}

fn load_dashboard_data(store: &Store, limit: usize) -> Result<DashboardData> {
    let records = store.list_records(limit)?;
    let evidence = store.recent_evidence(limit)?;
    let mut sessions = Vec::new();
    let mut runs = Vec::new();
    let mut events = Vec::new();
    let mut vcs_changes = Vec::new();
    let mut pull_requests = Vec::new();
    let mut artifacts = Vec::new();
    let mut files_touched = Vec::new();
    let mut summaries = Vec::new();

    for record in &records {
        sessions.extend(store.sessions_for_record(record.id)?);
        runs.extend(store.runs_for_record(record.id)?);
        events.extend(store.events_for_record(record.id)?);
        vcs_changes.extend(store.vcs_changes_for_record(record.id)?);
        pull_requests.extend(store.pull_requests_for_record(record.id)?);
        artifacts.extend(store.artifacts_for_record(record.id)?);
        files_touched.extend(store.files_touched_for_record(record.id)?);
        summaries.extend(store.summaries_for_record(record.id)?);
    }

    Ok(DashboardData {
        records,
        evidence,
        sessions,
        runs,
        events,
        vcs_workspaces: Vec::new(),
        vcs_changes,
        pull_requests,
        artifacts,
        evidence_metadata: Vec::new(),
        files_touched,
        summaries,
    })
}

fn auto_import_pending_spool(data_root: &Path, store: &mut Store) -> Result<()> {
    let inbox = capture_inbox_dir(data_root);
    let counts = spool_counts(&inbox)?;
    if counts.pending == 0 {
        return Ok(());
    }

    let summary = import_spool(&inbox, store)?;
    if summary.failed_files > 0 {
        eprintln!(
            "ctx: failed to import {} capture spool file(s); run `ctx doctor` or `ctx repair`",
            summary.failed_files
        );
    }
    Ok(())
}

fn print_doctor_findings(store: &Store, data_root: &Path) -> Result<()> {
    let mut findings = store.validate()?;
    let counts = spool_counts(capture_inbox_dir(data_root))?;
    if counts.failed > 0 {
        findings.push(format!(
            "{} failed capture spool file(s) need retry or inspection",
            counts.failed
        ));
    }
    if counts.processing > 0 {
        findings.push(format!(
            "{} capture spool file(s) are still marked processing",
            counts.processing
        ));
    }
    if findings.is_empty() {
        println!("valid");
    } else {
        for finding in findings {
            println!("{finding}");
        }
    }
    Ok(())
}

fn print_privacy_doctor(store: &Store, data_root: &Path) -> Result<()> {
    let findings = store.validate()?;
    let counts = spool_counts(capture_inbox_dir(data_root))?;
    let work_dir = work_record_dir(data_root.to_path_buf());
    let db_path = database_path(data_root.to_path_buf());
    let inbox = capture_inbox_dir(data_root);

    println!("Privacy health");
    println!("data_root: {}", data_root.display());
    println!("storage: local_only");
    println!("hosted_sync: disabled");
    println!("database: {}", db_path.display());
    println!("inbox_dir: {}", inbox.display());
    println!(
        "validation: {}",
        if findings.is_empty() {
            "valid"
        } else {
            "findings_present"
        }
    );
    println!("spool_pending: {}", counts.pending);
    println!("spool_processing: {}", counts.processing);
    println!("spool_failed: {}", counts.failed);
    println!(
        "permissions_work_record_dir: {}",
        privacy_permission_status(&work_dir)?
    );
    println!(
        "permissions_database: {}",
        privacy_permission_status(&db_path)?
    );
    println!("permissions_inbox: {}", privacy_permission_status(&inbox)?);
    if counts.failed > 0 {
        println!("action: inspect failed spool files before sharing logs or retrying");
    }
    if counts.pending > 0 || counts.processing > 0 {
        println!("action: import or inspect pending capture spool files");
    }
    for finding in findings {
        println!("finding: {finding}");
    }
    Ok(())
}

fn privacy_permission_status(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok("missing".to_owned());
    }
    let metadata = fs::metadata(path)?;
    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 == 0 {
            Ok(format!("private ({mode:o})"))
        } else {
            Ok(format!("shared ({mode:o})"))
        }
    }
    #[cfg(not(unix))]
    {
        let readonly = metadata.permissions().readonly();
        Ok(if readonly {
            "readonly".to_owned()
        } else {
            "platform_default".to_owned()
        })
    }
}

fn run_repair(args: RepairArgs, store: &mut Store, data_root: &Path) -> Result<()> {
    let inbox = capture_inbox_dir(data_root);
    let repair = retry_failed_spool_files(&inbox)?;
    let import = import_spool(&inbox, store)?;
    if args.json {
        print_json(serde_json::json!({
            "schema_version": 1,
            "repair": repair,
            "import": import,
        }))?;
    } else {
        println!(
            "retried {} failed capture spool file(s)",
            repair.retried_files
        );
        println!(
            "imported {} records and {} evidence items from {} spool files",
            import.imported_records, import.imported_evidence, import.processed_files
        );
    }
    if import.failed_files > 0 {
        return Err(anyhow!(
            "failed to import {} capture spool file(s)",
            import.failed_files
        ));
    }
    Ok(())
}

fn run_capture(command: CaptureCommand, data_root: PathBuf) -> Result<()> {
    match command.command {
        CaptureSubcommand::WriteFixture(args) => {
            write_fixture(
                capture_inbox_dir(&data_root),
                FixtureOptions {
                    title: args.title,
                    body: args.body,
                    tags: args.tags,
                    dedupe_key: args.dedupe_key,
                    machine_id: args.machine_id,
                    cwd: args.cwd,
                    ..FixtureOptions::default()
                },
            )?;
        }
        CaptureSubcommand::WriteShimCommand(args) => {
            let started_at = DateTime::parse_from_rfc3339(&args.started_at)
                .with_context(|| format!("parse started_at `{}`", args.started_at))?
                .with_timezone(&Utc);
            write_shim_command(
                capture_inbox_dir(&data_root),
                ShimCommandOptions {
                    provider: args.provider.provider(),
                    command: args.command,
                    exit_code: args.exit_code,
                    stdout: read_file_capped(&args.stdout_file, DEFAULT_SHIM_MAX_OUTPUT_BYTES)?,
                    stderr: read_file_capped(&args.stderr_file, DEFAULT_SHIM_MAX_OUTPUT_BYTES)?,
                    started_at,
                    duration_ms: args.duration_ms,
                    machine_id: args.machine_id,
                    cwd: args.cwd,
                    real_command: args.real_command,
                    shim_dir: args.shim_dir,
                },
            )?;
        }
        CaptureSubcommand::Import(args) => {
            let mut store = Store::open(database_path(data_root.clone()))?;
            let summary = import_spool(capture_inbox_dir(&data_root), &mut store)?;
            if args.json {
                print_json(serde_json::json!({
                    "schema_version": 1,
                    "import": summary,
                }))?;
            } else {
                println!(
                    "imported {} records and {} evidence items from {} spool files",
                    summary.imported_records, summary.imported_evidence, summary.processed_files
                );
            }
            if summary.failed_files > 0 {
                return Err(anyhow!(
                    "failed to import {} capture spool file(s)",
                    summary.failed_files
                ));
            }
        }
        CaptureSubcommand::ImportProvider(args) => {
            let mut store = Store::open(database_path(data_root.clone()))?;
            auto_import_pending_spool(&data_root, &mut store)?;
            let fixture = read_provider_fixture_summary(&args.input, args.provider)?;
            let summary = import_provider_fixture_jsonl(
                &args.input,
                &mut store,
                ProviderFixtureImportOptions {
                    source_path: Some(args.input.clone()),
                    ..ProviderFixtureImportOptions::default()
                },
            )?;
            let record = if summary.imported_sessions > 0 || summary.imported_events > 0 {
                Some(insert_provider_import_summary_record(
                    &store,
                    args.provider,
                    &args.input,
                    &summary,
                    &fixture,
                )?)
            } else {
                None
            };
            if args.json {
                print_json(serde_json::json!({
                    "schema_version": 1,
                    "provider": args.provider.as_str(),
                    "input": args.input,
                    "import": summary,
                    "record": record,
                }))?;
            } else {
                println!(
                    "imported {} provider item(s), skipped {}, failed {}",
                    summary.imported, summary.skipped, summary.failed
                );
                if let Some(record) = record {
                    println!("record: {}", record.id);
                }
            }
            if summary.failed > 0 {
                return Err(anyhow!(
                    "failed to import {} provider fixture line(s)",
                    summary.failed
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ProviderFixtureSummary {
    sessions: Vec<String>,
    events: Vec<String>,
}

fn read_provider_fixture_summary(
    path: &Path,
    expected_provider: ProviderFixtureProvider,
) -> Result<ProviderFixtureSummary> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("read provider fixture {}", path.display()))?;
    let mut sessions = Vec::new();
    let mut events = Vec::new();
    let expected = expected_provider.as_str();

    for (index, line) in contents.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("parse provider fixture line {line_number}"))?;
        let provider = value["provider"].as_str().unwrap_or_default();
        if provider != expected {
            return Err(anyhow!(
                "provider fixture line {} is `{}` but --provider is `{}`",
                line_number,
                provider,
                expected
            ));
        }
        if let Some(session_id) = value["session"]["provider_session_id"].as_str() {
            let session_id = session_id.to_owned();
            if !sessions.iter().any(|existing| existing == &session_id) {
                sessions.push(session_id);
            }
        }
        if let Some(event) = value.get("event") {
            let event_type = event["event_type"].as_str().unwrap_or("event");
            let role = event["role"].as_str().unwrap_or("unknown");
            let text = event["payload"]["text"]
                .as_str()
                .or_else(|| event["payload"]["cmd"].as_str())
                .or_else(|| event["payload"]["tool"].as_str())
                .unwrap_or("");
            if text.is_empty() {
                events.push(format!("{role} {event_type}"));
            } else {
                events.push(format!("{role} {event_type}: {text}"));
            }
        }
    }

    Ok(ProviderFixtureSummary { sessions, events })
}

fn insert_provider_import_summary_record(
    store: &Store,
    provider: ProviderFixtureProvider,
    input: &Path,
    import: &ProviderImportSummary,
    fixture: &ProviderFixtureSummary,
) -> Result<WorkRecord> {
    let provider_name = provider.as_str();
    let mut body = String::new();
    body.push_str(&format!(
        "Provider fixture import for {provider_name} from {}.\n",
        input.display()
    ));
    body.push_str(&format!(
        "Imported {} sessions, {} events, {} edges; skipped {} items; redacted {} fields.",
        import.imported_sessions,
        import.imported_events,
        import.imported_edges,
        import.skipped,
        import.redacted
    ));
    if !fixture.sessions.is_empty() {
        body.push_str("\n\nSessions:\n");
        for session in fixture.sessions.iter().take(12) {
            body.push_str("- ");
            body.push_str(session);
            body.push('\n');
        }
    }
    if !fixture.events.is_empty() {
        body.push_str("\nProvider events:\n");
        for event in fixture.events.iter().take(24) {
            body.push_str("- ");
            body.push_str(event);
            body.push('\n');
        }
    }

    let record = WorkRecord::new(
        format!("Imported {provider_name} provider fixture"),
        body.clone(),
        vec!["provider-import".to_owned(), provider_name.to_owned()],
        "provider-import",
        input.parent().map(|path| path.display().to_string()),
    );
    store.insert_record(&record)?;

    let now = Utc::now();
    store.upsert_summary(&Summary {
        id: new_id(),
        work_record_id: Some(record.id),
        session_id: None,
        kind: SummaryKind::ImportedProviderSummary,
        model_or_source: Some(format!("{provider_name}-fixture")),
        text: body,
        citations: Vec::new(),
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        source_id: None,
        sync: SyncMetadata {
            fidelity: Fidelity::Imported,
            metadata: serde_json::json!({
                "provider": provider_name,
                "input": input.display().to_string(),
                "imported_sessions": import.imported_sessions,
                "imported_events": import.imported_events,
                "imported_edges": import.imported_edges,
            }),
            ..SyncMetadata::default()
        },
    })?;

    Ok(record)
}

fn run_shim(command: ShimCommand) -> Result<()> {
    match command.command {
        ShimSubcommand::Install(args) => install_shims(&args.dir),
        ShimSubcommand::Env(args) => {
            println!("export PATH={}:$PATH", shell_escape_path(&args.dir));
            Ok(())
        }
        ShimSubcommand::Uninstall(args) => uninstall_shims(&args.dir),
    }
}

fn install_shims(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let ctx_bin = env::current_exe()?.canonicalize()?;
    for tool in ShimTool::ALL {
        let path = dir.join(tool.as_str());
        if path.exists() && !is_ctx_shim(&path)? {
            return Err(anyhow!(
                "refusing to overwrite unrecognized file {}",
                path.display()
            ));
        }
        fs::write(&path, wrapper_script(tool, &ctx_bin)?)?;
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&path)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions)?;
        }
        println!("installed {}", path.display());
    }
    Ok(())
}

fn uninstall_shims(dir: &Path) -> Result<()> {
    for tool in ShimTool::ALL {
        let path = dir.join(tool.as_str());
        if !path.exists() {
            continue;
        }
        if !is_ctx_shim(&path)? {
            return Err(anyhow!(
                "refusing to remove unrecognized shim {}",
                path.display()
            ));
        }
        fs::remove_file(&path)?;
        println!("removed {}", path.display());
    }
    Ok(())
}

fn is_ctx_shim(path: &Path) -> Result<bool> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("read shim {}", path.display()))?;
    Ok(contents.contains("CTX_WORK_RECORD_SHIM=1"))
}

fn wrapper_script(tool: ShimTool, ctx_bin: &Path) -> Result<String> {
    let tool_name = tool.as_str();
    let ctx_bin = shell_escape_path(ctx_bin);
    Ok(format!(
        r#"#!/bin/sh
# CTX_WORK_RECORD_SHIM=1
tool="{tool_name}"
shim_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ctx_bin="${{CTX_SHIM_CTX_BIN:-{ctx_bin}}}"
old_ifs=$IFS
IFS=:
clean_path=
for entry in $PATH; do
    if [ "$entry" = "$shim_dir" ]; then
        continue
    fi
    if [ -z "$clean_path" ]; then
        clean_path=$entry
    else
        clean_path=$clean_path:$entry
    fi
done
IFS=$old_ifs
real_cmd=$(PATH="$clean_path" command -v "$tool" 2>/dev/null)
if [ -z "$real_cmd" ]; then
    echo "ctx shim: real $tool not found outside $shim_dir" >&2
    exit 127
fi
tmpdir=$(mktemp -d "${{TMPDIR:-/tmp}}/ctx-shim-$tool.XXXXXX") || exit 125
stdout_file=$tmpdir/stdout
stderr_file=$tmpdir/stderr
started_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
start_ms=$(date +%s%3N 2>/dev/null || printf '%s000' "$(date +%s)")
"$real_cmd" "$@" >"$stdout_file" 2>"$stderr_file"
status=$?
end_ms=$(date +%s%3N 2>/dev/null || printf '%s000' "$(date +%s)")
duration_ms=$((end_ms - start_ms))
cat "$stdout_file"
cat "$stderr_file" >&2
"$ctx_bin" capture write-shim-command \
    --provider "$tool" \
    --exit-code "$status" \
    --stdout-file "$stdout_file" \
    --stderr-file "$stderr_file" \
    --started-at "$started_at" \
    --duration-ms "$duration_ms" \
    --cwd "$PWD" \
    --real-command "$real_cmd" \
    --shim-dir "$shim_dir" \
    -- "$tool" "$@" >/dev/null 2>&1 || true
rm -rf "$tmpdir"
exit "$status"
"#
    ))
}

fn shell_escape_path(path: &Path) -> String {
    let raw = path.display().to_string();
    if raw
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-'))
    {
        raw
    } else {
        format!("'{}'", raw.replace('\'', "'\\''"))
    }
}

fn run_evidence(args: EvidenceCommand, store: &Store) -> Result<()> {
    match args.command {
        EvidenceSubcommand::Run(args) => {
            let (program, rest) = args
                .command
                .split_first()
                .ok_or_else(|| anyhow!("missing command"))?;
            let started_at = Utc::now();
            let timer = Instant::now();
            let output = run_with_limits(
                program,
                rest,
                args.max_output_bytes,
                Duration::from_secs(args.timeout_seconds),
            )
            .with_context(|| format!("run evidence command `{}`", args.command.join(" ")))?;
            let duration_ms = timer.elapsed().as_millis().try_into().unwrap_or(i64::MAX);
            let record_id = match args.record {
                Some(record_id) => Some(record_id),
                None => {
                    let workspace = std::env::current_dir()
                        .ok()
                        .map(|path| path.display().to_string());
                    let record = WorkRecord::new(
                        format!("Command evidence: {}", args.command.join(" ")),
                        "Command evidence captured without an explicit Work Record.",
                        vec!["evidence".to_owned()],
                        "evidence",
                        workspace,
                    );
                    store.insert_record(&record)?;
                    Some(record.id)
                }
            };
            let evidence = Evidence::new(
                record_id,
                args.command.join(" "),
                output.exit_code,
                output.stdout,
                output.stderr,
                started_at,
                duration_ms,
            );
            store.insert_evidence(&evidence)?;
            let persisted_evidence = store.get_evidence(evidence.id)?;
            print_json(serde_json::json!({
                "schema_version": 1,
                "evidence": persisted_evidence,
            }))?;
            if output.exit_code == 0 {
                Ok(())
            } else {
                Err(anyhow!("evidence command exited with {}", output.exit_code))
            }
        }
    }
}

struct LimitedOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn run_with_limits(
    program: &str,
    args: &[String],
    max_output_bytes: usize,
    timeout: Duration,
) -> Result<LimitedOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_timeout_isolation(&mut command);

    let mut child = command.spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture stderr"))?;
    let stdout_task = thread::spawn(move || capture_capped(stdout, max_output_bytes));
    let stderr_task = thread::spawn(move || capture_capped(stderr, max_output_bytes));

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            terminate_timed_out_child(&mut child)?;
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(20));
    };

    let stdout = join_capture_task(stdout_task, "stdout")?;
    let mut stderr = join_capture_task(stderr_task, "stderr")?;
    let exit_code = if timed_out {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "command timed out after {} seconds",
            timeout.as_secs()
        ));
        TIMEOUT_EXIT_CODE
    } else {
        status.code().unwrap_or(1)
    };

    Ok(LimitedOutput {
        exit_code,
        stdout,
        stderr,
    })
}

#[cfg(unix)]
fn configure_timeout_isolation(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }
}

#[cfg(not(unix))]
fn configure_timeout_isolation(_command: &mut Command) {
    // Non-Unix platforms keep the direct-child timeout behavior because the
    // portable std::process API has no process-group equivalent.
}

#[cfg(unix)]
fn terminate_timed_out_child(child: &mut std::process::Child) -> io::Result<()> {
    let pgid = child.id() as libc::pid_t;
    if unsafe { libc::killpg(pgid, libc::SIGKILL) } == 0 {
        Ok(())
    } else {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            child.kill()
        }
    }
}

#[cfg(not(unix))]
fn terminate_timed_out_child(child: &mut std::process::Child) -> io::Result<()> {
    child.kill()
}

fn capture_capped(mut stream: impl Read, max_output_bytes: usize) -> io::Result<String> {
    let mut output = Vec::with_capacity(max_output_bytes.min(8192));
    let mut buffer = [0_u8; 8192];
    loop {
        let bytes = stream.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        if output.len() < max_output_bytes {
            let remaining = max_output_bytes - output.len();
            output.extend_from_slice(&buffer[..bytes.min(remaining)]);
        }
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

fn read_file_capped(path: &Path, max_output_bytes: usize) -> Result<String> {
    let file = fs::File::open(path).with_context(|| format!("read file {}", path.display()))?;
    let mut reader = file.take(max_output_bytes as u64 + 1);
    let mut output = Vec::with_capacity(max_output_bytes.min(8192));
    reader.read_to_end(&mut output)?;
    let truncated = output.len() > max_output_bytes;
    if truncated {
        output.truncate(max_output_bytes);
    }
    let mut text = String::from_utf8_lossy(&output).into_owned();
    if truncated {
        text.push_str("\n[ctx shim output truncated]\n");
    }
    Ok(text)
}

fn join_capture_task(
    handle: thread::JoinHandle<io::Result<String>>,
    stream_name: &str,
) -> Result<String> {
    handle
        .join()
        .map_err(|_| anyhow!("{stream_name} capture thread panicked"))?
        .map_err(Into::into)
}

fn read_body(body: String) -> Result<String> {
    if body == "-" {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input)?;
        Ok(input)
    } else {
        Ok(body)
    }
}

fn packet_options(limit: usize, max_tokens: Option<u32>) -> work_record_search::PacketOptions {
    work_record_search::PacketOptions {
        limit,
        max_tokens: max_tokens.unwrap_or(work_record_search::DEFAULT_MAX_TOKENS),
        dashboard_base_url: env::var("CTX_DASHBOARD_URL")
            .ok()
            .and_then(|value| work_record_search::share_safe_dashboard_base_url(&value)),
        ..Default::default()
    }
}

fn print_record(record: &WorkRecord, json: bool) -> Result<()> {
    if json {
        print_json(serde_json::json!({
            "schema_version": 1,
            "record": record,
        }))?;
    } else {
        println!("{} {}", record.id, record.title);
        if !record.body.is_empty() {
            println!("{}", record.body);
        }
        if !record.tags.is_empty() {
            println!("tags: {}", record.tags.join(", "));
        }
        if let Some(pr_url) = &record.pr_url {
            println!("pr: {pr_url}");
        }
    }
    Ok(())
}

fn print_records(records: &[WorkRecord], json: bool) -> Result<()> {
    if json {
        print_json(serde_json::json!({
            "schema_version": 1,
            "records": records,
        }))?;
    } else {
        for record in records {
            println!("{} [{}] {}", record.id, record.kind, record.title);
        }
    }
    Ok(())
}

fn print_json(value: serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

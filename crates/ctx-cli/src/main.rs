use std::{
    collections::BTreeSet,
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
    import_codex_history_jsonl, import_pi_session_jsonl, import_provider_fixture_jsonl,
    import_spool, inbox_dir as capture_inbox_dir, retry_failed_spool_files, spool_counts,
    stable_capture_uuid, write_fixture, write_shim_command, CodexHistoryImportOptions,
    FixtureOptions, PiSessionImportOptions, ProviderFixtureImportOptions, ProviderImportSummary,
    ShimCommandOptions,
};
use work_record_core::{
    blob_dir, database_path, default_data_root, device_path, inbox_dir, new_id,
    redact_share_safe_markers, work_record_dir, Artifact, CaptureProvider, Confidence,
    EntityTimestamps, Evidence, EvidenceFreshness, EvidenceKind, EvidenceMetadata, EvidenceStatus,
    Fidelity, FileTouched, PullRequest, Run, Session, Summary, SummaryKind, SyncMetadata,
    VcsChange, VcsChangeKind, VcsKind, VcsWorkspace, Visibility, WorkRecord, WorkRecordArchive,
    WorkRecordLink, WorkRecordLinkTargetType, WorkRecordLinkType,
};
use work_record_publish::{
    render_pr_comment, upsert_github_pr_comment, GhCliGitHubPrCommentClient, PublishOptions,
    PublishOutcome, PullRequestTarget, RawTranscriptOptIn, RenderOptions,
};
use work_record_store::{classify_evidence_freshness, Store, StoreError};
use work_record_vcs::{
    inspect_path, parse_pull_request_url, GitDetection, GitStatus, GitWorkspace, JjCommit,
    JjDetection, JjWorkspace,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const DEFAULT_EVIDENCE_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_EVIDENCE_TIMEOUT_SECONDS: u64 = 300;
const DEFAULT_SHIM_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const TIMEOUT_EXIT_CODE: i32 = 124;
const SHELL_RC_BEGIN: &str = "# >>> ctx work recorder passive capture >>>";
const SHELL_RC_END: &str = "# <<< ctx work recorder passive capture <<<";

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
    Setup(SetupArgs),
    #[command(about = "Show local Work Recorder workspace status")]
    Status(StatusArgs),
    #[command(about = "Remove local Work Recorder product data")]
    Uninstall(UninstallArgs),
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
    Validate(ValidateArgs),
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
    Setup(SetupArgs),
    #[command(about = "Show local Work Recorder workspace status")]
    Status(StatusArgs),
    #[command(about = "Remove local Work Recorder product data")]
    Uninstall(UninstallArgs),
}

#[derive(Debug, Args)]
struct WorkCommand {
    #[command(subcommand)]
    command: WorkSubcommand,
}

#[derive(Debug, Args, Clone)]
struct SetupArgs {
    #[arg(long, value_name = "FILE")]
    shell_rc: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
struct StatusArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args, Clone)]
struct UninstallArgs {
    #[arg(long)]
    yes: bool,
    #[arg(long, value_name = "FILE")]
    shell_rc: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
struct ValidateArgs {
    #[arg(long)]
    json: bool,
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
    Validate(ValidateArgs),
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
    #[command(about = "Export and open the local Work Recorder dashboard")]
    Open(DashboardOpenArgs),
}

#[derive(Debug, Args)]
struct DashboardExportArgs {
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 1000)]
    limit: usize,
}

#[derive(Debug, Args)]
struct DashboardOpenArgs {
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long, default_value_t = 1000)]
    limit: usize,
    #[arg(long)]
    no_browser: bool,
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
    #[command(about = "Import provider fixture JSONL")]
    ImportProvider(CaptureImportProviderArgs),
    #[command(about = "Import a Codex prompt history JSONL file")]
    ImportCodexHistory(CaptureImportCodexHistoryArgs),
    #[command(about = "Import a Pi session JSONL file")]
    ImportPiSession(CaptureImportPiSessionArgs),
    #[command(about = "Discover and safely import supported local provider history")]
    ImportLocalProviders(CaptureImportLocalProvidersArgs),
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

#[derive(Debug, Args)]
struct CaptureImportCodexHistoryArgs {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct CaptureImportPiSessionArgs {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct CaptureImportLocalProvidersArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProviderFixtureProvider {
    Codex,
    Claude,
    Pi,
    #[value(name = "opencode", alias = "open-code")]
    OpenCode,
    Antigravity,
    Gemini,
    Cursor,
}

impl ProviderFixtureProvider {
    fn capture_provider(self) -> CaptureProvider {
        match self {
            Self::Codex => CaptureProvider::Codex,
            Self::Claude => CaptureProvider::Claude,
            Self::Pi => CaptureProvider::Pi,
            Self::OpenCode => CaptureProvider::OpenCode,
            Self::Antigravity => CaptureProvider::Antigravity,
            Self::Gemini => CaptureProvider::Gemini,
            Self::Cursor => CaptureProvider::Cursor,
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
    #[command(about = "Add a reversible ctx PATH block to a shell rc file")]
    ActivateShell(ShimShellArgs),
    #[command(about = "Remove the reversible ctx PATH block from a shell rc file")]
    DeactivateShell(ShimShellArgs),
}

#[derive(Debug, Args)]
struct ShimDirArgs {
    #[arg(long)]
    dir: PathBuf,
}

#[derive(Debug, Args)]
struct ShimShellArgs {
    #[arg(long)]
    dir: PathBuf,
    #[arg(long, value_name = "FILE")]
    shell_rc: PathBuf,
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
    #[arg(long)]
    json: bool,
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
        CommandRoot::Setup(args) => {
            run_workspace_subcommand(WorkspaceSubcommand::Setup(args), data_root)
        }
        CommandRoot::Status(args) => {
            run_workspace_subcommand(WorkspaceSubcommand::Status(args), data_root)
        }
        CommandRoot::Uninstall(args) => {
            run_workspace_subcommand(WorkspaceSubcommand::Uninstall(args), data_root)
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
        CommandRoot::Validate(args) => {
            run_work_subcommand(WorkSubcommand::Validate(args), data_root)
        }
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
        if args.json {
            print_json(serde_json::json!({
                "schema_version": 1,
                "share_safe": !rendered.raw_transcript_included,
                "dry_run": true,
                "target": pull_request_target_value(&target),
                "raw_transcript_included": rendered.raw_transcript_included,
                "markdown": rendered.markdown,
            }))?;
        } else {
            print!("{}", rendered.markdown);
        }
        return Ok(());
    }

    let mut client = GhCliGitHubPrCommentClient::new();
    let outcome = upsert_github_pr_comment(
        &mut client,
        &target,
        &rendered.markdown,
        &PublishOptions { dry_run: false },
    )?;
    if args.json {
        print_json(serde_json::json!({
            "schema_version": 1,
            "share_safe": !rendered.raw_transcript_included,
            "dry_run": false,
            "target": pull_request_target_value(&target),
            "raw_transcript_included": rendered.raw_transcript_included,
            "outcome": publish_outcome_value(&outcome),
        }))?;
    } else {
        println!("{}", publish_outcome_message(&outcome, &target));
    }
    Ok(())
}

fn pull_request_target_value(target: &PullRequestTarget) -> serde_json::Value {
    serde_json::json!({
        "provider": target.provider.as_str(),
        "host": redact_share_safe_markers(&target.host),
        "owner": redact_share_safe_markers(&target.owner),
        "repo": redact_share_safe_markers(&target.repo),
        "number": target.number,
        "url": redact_share_safe_markers(&target.normalized_url),
    })
}

fn publish_outcome_value(outcome: &PublishOutcome) -> serde_json::Value {
    match outcome {
        PublishOutcome::DryRunCreated { markdown } => serde_json::json!({
            "action": "created",
            "dry_run": true,
            "markdown": markdown,
        }),
        PublishOutcome::DryRunUpdated {
            comment_id,
            markdown,
        } => serde_json::json!({
            "action": "updated",
            "dry_run": true,
            "comment_id": comment_id,
            "markdown": markdown,
        }),
        PublishOutcome::DryRunUnchanged {
            comment_id,
            markdown,
        } => serde_json::json!({
            "action": "unchanged",
            "dry_run": true,
            "comment_id": comment_id,
            "markdown": markdown,
        }),
        PublishOutcome::Created { comment_id } => serde_json::json!({
            "action": "created",
            "dry_run": false,
            "comment_id": comment_id,
        }),
        PublishOutcome::Updated { comment_id } => serde_json::json!({
            "action": "updated",
            "dry_run": false,
            "comment_id": comment_id,
        }),
        PublishOutcome::Unchanged { comment_id } => serde_json::json!({
            "action": "unchanged",
            "dry_run": false,
            "comment_id": comment_id,
        }),
    }
}

fn publish_outcome_message(outcome: &PublishOutcome, target: &PullRequestTarget) -> String {
    let action = match outcome {
        PublishOutcome::Created { .. } => "created",
        PublishOutcome::Updated { .. } => "updated",
        PublishOutcome::Unchanged { .. } => "unchanged",
        PublishOutcome::DryRunCreated { .. }
        | PublishOutcome::DryRunUpdated { .. }
        | PublishOutcome::DryRunUnchanged { .. } => "dry-run",
    };
    let comment_id = match outcome {
        PublishOutcome::Created { comment_id }
        | PublishOutcome::Updated { comment_id }
        | PublishOutcome::Unchanged { comment_id }
        | PublishOutcome::DryRunUpdated { comment_id, .. }
        | PublishOutcome::DryRunUnchanged { comment_id, .. } => Some(*comment_id),
        PublishOutcome::DryRunCreated { .. } => None,
    };
    match comment_id {
        Some(id) => format!(
            "GitHub PR comment {action}: {}/{}#{} comment {id}",
            target.owner, target.repo, target.number
        ),
        None => format!(
            "GitHub PR comment {action}: {}/{}#{}",
            target.owner, target.repo, target.number
        ),
    }
}

fn run_workspace(command: WorkspaceCommand, data_root: PathBuf) -> Result<()> {
    run_workspace_subcommand(command.command, data_root)
}

fn run_workspace_subcommand(command: WorkspaceSubcommand, data_root: PathBuf) -> Result<()> {
    match command {
        WorkspaceSubcommand::Setup(args) => {
            let db_path = database_path(data_root.clone());
            let store = Store::open(&db_path)?;
            let shim_dir = default_shim_dir(&data_root);
            install_shims(&shim_dir)?;
            if let Some(shell_rc) = args.shell_rc.as_ref() {
                activate_shell_rc(shell_rc, &shim_dir)?;
            }
            println!("Work Recorder workspace ready");
            println!("database: {}", store.path().display());
            println!("passive_capture_shims: {}", shim_dir.display());
            println!(
                "activate: export PATH={}:$PATH",
                shell_escape_path(&shim_dir)
            );
            if let Some(shell_rc) = args.shell_rc.as_ref() {
                println!("shell_rc: {}", shell_rc.display());
            }
        }
        WorkspaceSubcommand::Status(args) => {
            let db_path = database_path(data_root.clone());
            let capture_inbox = capture_inbox_dir(&data_root);
            let shim_dir = default_shim_dir(&data_root);
            let counts = spool_counts(&capture_inbox)?;
            let mut shim_statuses = Vec::new();
            if !args.json {
                println!("data_root: {}", data_root.display());
                println!(
                    "work_record_dir: {}",
                    work_record_dir(data_root.clone()).display()
                );
                println!("shim_dir: {}", shim_dir.display());
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
            let mut active = 0;
            for tool in ShimTool::ALL {
                let status = passive_shim_status(tool, &shim_dir)?;
                if matches!(status, PassiveShimStatus::Active(_)) {
                    active += 1;
                }
                if !args.json {
                    println!("shim_{}: {}", tool.as_str(), status.display());
                }
                shim_statuses.push((tool, status));
            }
            if !args.json {
                println!(
                    "passive_capture_active_on_path: {active}/{}",
                    ShimTool::ALL.len()
                );
            }
            if args.json {
                print_json(serde_json::json!({
                    "schema_version": 1,
                    "share_safe": false,
                    "initialized": db_path.exists(),
                    "local_only": true,
                    "paths": {
                        "data_root": data_root.display().to_string(),
                        "work_record_dir": work_record_dir(data_root.clone()).display().to_string(),
                        "shim_dir": shim_dir.display().to_string(),
                        "blob_dir": blob_dir(data_root.clone()).display().to_string(),
                        "inbox_dir": inbox_dir(data_root.clone()).display().to_string(),
                        "device_path": device_path(data_root.clone()).display().to_string(),
                        "database": db_path.display().to_string(),
                    },
                    "spool": {
                        "pending": counts.pending,
                        "tmp": counts.tmp,
                        "processing": counts.processing,
                        "done": counts.done,
                        "failed": counts.failed,
                    },
                    "passive_capture": {
                        "active_on_path": active,
                        "expected_shims": ShimTool::ALL.len(),
                        "shims": shim_statuses.iter().map(|(tool, status)| {
                            serde_json::json!({
                                "tool": tool.as_str(),
                                "state": status.state(),
                                "path": status.path().map(|path| path.display().to_string()),
                                "display": status.display(),
                            })
                        }).collect::<Vec<_>>(),
                    },
                }))?;
            }
        }
        WorkspaceSubcommand::Uninstall(args) => {
            if !args.yes {
                return Err(anyhow!("refusing to uninstall without --yes"));
            }
            if let Some(shell_rc) = args.shell_rc.as_ref() {
                deactivate_shell_rc(shell_rc)?;
            }
            let shim_dir = default_shim_dir(&data_root);
            let _ = uninstall_shims(&shim_dir);
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
                print_share_safe_value(serde_json::to_value(packet)?)?;
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
                print_share_safe_value(serde_json::to_value(packet)?)?;
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
            persist_typed_pr_link(&store, record.id, &args.pr_url)?;
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
        WorkSubcommand::Validate(args) => print_doctor_findings(&store, &data_root, args.json)?,
        WorkSubcommand::Doctor(args) => {
            if args.privacy {
                print_privacy_doctor(&store, &data_root)?;
            } else {
                print_doctor_findings(&store, &data_root, false)?;
            }
        }
        WorkSubcommand::Repair(args) => run_repair(args, &mut store, &data_root)?,
    }
    Ok(())
}

fn run_dashboard(command: DashboardCommand, data_root: PathBuf) -> Result<()> {
    match command.command {
        DashboardSubcommand::Export(args) => {
            let index = export_dashboard(&data_root, &args.output, args.limit)?;
            println!("dashboard: {}", index.display());
        }
        DashboardSubcommand::Open(args) => {
            let output = args
                .output
                .unwrap_or_else(|| work_record_dir(data_root.clone()).join("dashboard"));
            let index = export_dashboard(&data_root, &output, args.limit)?;
            println!("dashboard: {}", index.display());
            if !args.no_browser {
                open_dashboard_file(&index)?;
            }
        }
    }
    Ok(())
}

fn export_dashboard(data_root: &Path, output: &Path, limit: usize) -> Result<PathBuf> {
    let mut store = Store::open(database_path(data_root.to_path_buf()))?;
    auto_import_pending_spool(data_root, &mut store)?;
    let data = load_dashboard_data(&store, limit)?;
    let report = data.report();
    let html = work_record_report::render_dashboard_html_report(&report);
    fs::create_dir_all(output)?;
    let index = output.join("index.html");
    fs::write(&index, html)?;
    for (relative_path, contents) in work_record_report::dashboard_static_assets() {
        let path = output.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
    }
    Ok(index)
}

fn open_dashboard_file(index: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(index);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.arg("/C").arg("start").arg("").arg(index);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(index);
        command
    };

    command
        .spawn()
        .with_context(|| format!("open dashboard {}", index.display()))?;
    Ok(())
}

fn persist_typed_pr_link(store: &Store, record_id: Uuid, pr_url: &str) -> Result<()> {
    let parsed = match parse_pull_request_url(pr_url) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(()),
    };
    let now = Utc::now();
    let pr = PullRequest {
        id: new_id(),
        vcs_workspace_id: None,
        provider: parsed.provider,
        url: parsed.normalized_url,
        number: Some(parsed.number),
        owner: Some(parsed.owner),
        repo: Some(parsed.repo),
        title: None,
        state: None,
        head_ref: None,
        base_ref: None,
        head_sha: None,
        confidence: parsed.confidence,
        link_source: parsed.link_source,
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        source_id: None,
        sync: SyncMetadata {
            visibility: Visibility::Reportable,
            fidelity: Fidelity::Partial,
            ..SyncMetadata::default()
        },
    };
    let pr_id = store.upsert_pull_request(&pr)?;
    let link = WorkRecordLink {
        id: new_id(),
        work_record_id: record_id,
        target_type: WorkRecordLinkTargetType::PullRequest,
        target_id: pr_id,
        link_type: WorkRecordLinkType::References,
        confidence: parsed.confidence,
        source_id: None,
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        sync: SyncMetadata {
            visibility: Visibility::Reportable,
            fidelity: Fidelity::Partial,
            ..SyncMetadata::default()
        },
    };
    store.upsert_work_record_link(&link)?;
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
    let mut evidence_metadata = Vec::new();
    let mut files_touched = Vec::new();
    let mut summaries = Vec::new();

    for record in &records {
        sessions.extend(store.sessions_for_record(record.id)?);
        runs.extend(store.runs_for_record(record.id)?);
        events.extend(store.events_for_record(record.id)?);
        vcs_changes.extend(store.vcs_changes_for_record(record.id)?);
        pull_requests.extend(store.pull_requests_for_record(record.id)?);
        artifacts.extend(store.artifacts_for_record(record.id)?);
        evidence_metadata.extend(store.evidence_metadata_for_record(record.id)?);
        files_touched.extend(store.files_touched_for_record(record.id)?);
        summaries.extend(store.summaries_for_record(record.id)?);
    }
    if evidence_metadata.is_empty() {
        evidence_metadata = store.recent_evidence_metadata(limit)?;
    }
    let archive = store.export_archive()?;
    reclassify_evidence_freshness_for_current_vcs(
        &mut evidence_metadata,
        &archive.vcs_changes,
        &archive.vcs_workspaces,
    );
    let mut workspace_ids = BTreeSet::new();
    for change in &vcs_changes {
        workspace_ids.insert(change.vcs_workspace_id);
    }
    for pr in &pull_requests {
        if let Some(id) = pr.vcs_workspace_id {
            workspace_ids.insert(id);
        }
    }
    for file in &files_touched {
        if let Some(id) = file.vcs_workspace_id {
            workspace_ids.insert(id);
        }
    }
    let vcs_workspaces = archive
        .vcs_workspaces
        .into_iter()
        .filter(|workspace| workspace_ids.is_empty() || workspace_ids.contains(&workspace.id))
        .take(limit)
        .collect();

    Ok(DashboardData {
        records,
        evidence,
        sessions,
        runs,
        events,
        vcs_workspaces,
        vcs_changes,
        pull_requests,
        artifacts,
        evidence_metadata,
        files_touched,
        summaries,
    })
}

fn reclassify_evidence_freshness_for_current_vcs(
    evidence_metadata: &mut [EvidenceMetadata],
    vcs_changes: &[VcsChange],
    vcs_workspaces: &[VcsWorkspace],
) {
    let changes_by_id: std::collections::BTreeMap<Uuid, &VcsChange> = vcs_changes
        .iter()
        .map(|change| (change.id, change))
        .collect();
    let workspaces_by_id: std::collections::BTreeMap<Uuid, &VcsWorkspace> = vcs_workspaces
        .iter()
        .map(|workspace| (workspace.id, workspace))
        .collect();
    for metadata in evidence_metadata.iter_mut() {
        let (freshness, reason) =
            current_freshness_for_metadata(metadata, &changes_by_id, &workspaces_by_id);
        metadata.freshness = freshness;
        metadata.stale_reason = reason;
    }
}

fn current_freshness_for_metadata(
    metadata: &EvidenceMetadata,
    changes_by_id: &std::collections::BTreeMap<Uuid, &VcsChange>,
    workspaces_by_id: &std::collections::BTreeMap<Uuid, &VcsWorkspace>,
) -> (EvidenceFreshness, Option<String>) {
    let Some(change_id) = metadata.vcs_change_id else {
        return (
            classify_evidence_freshness(metadata, None, None, false),
            None,
        );
    };
    let Some(change) = changes_by_id.get(&change_id).copied() else {
        return (
            EvidenceFreshness::ProbablyFresh,
            Some(
                "current VCS state unavailable: recorded change is not in the local archive".into(),
            ),
        );
    };
    let Some(workspace) = workspaces_by_id.get(&change.vcs_workspace_id).copied() else {
        return (
            EvidenceFreshness::ProbablyFresh,
            Some(
                "current VCS state unavailable: recorded workspace is not in the local archive"
                    .into(),
            ),
        );
    };

    let inspection = match inspect_path(Path::new(&workspace.root_path)) {
        Ok(inspection) => inspection,
        Err(error) => {
            return (
                EvidenceFreshness::ProbablyFresh,
                Some(format!("current VCS state unavailable: {error}")),
            );
        }
    };

    match workspace.kind {
        VcsKind::Git => {
            let Some(git) = inspection.git.workspace.as_ref() else {
                return (
                    EvidenceFreshness::ProbablyFresh,
                    Some("current VCS state unavailable: git workspace not found".into()),
                );
            };
            if git.repo_fingerprint.value != workspace.repo_fingerprint {
                return (
                    EvidenceFreshness::ProbablyFresh,
                    Some("current VCS state unavailable: workspace fingerprint changed".into()),
                );
            }
            let freshness = classify_evidence_freshness(
                metadata,
                git.head_sha.as_deref(),
                git.tree_hash.as_deref(),
                git.status.dirty,
            );
            let reason = match freshness {
                EvidenceFreshness::Stale => {
                    Some("current VCS HEAD or tree differs from the evidence capture point".into())
                }
                EvidenceFreshness::ProbablyFresh if git.status.dirty => {
                    Some("current VCS state has uncommitted changes".into())
                }
                EvidenceFreshness::ProbablyFresh => {
                    Some("current VCS state could not prove tree freshness".into())
                }
                _ => None,
            };
            (freshness, reason)
        }
        VcsKind::Jj => {
            let Some(jj) = inspection.jj.workspace.as_ref() else {
                return (
                    EvidenceFreshness::ProbablyFresh,
                    Some("current VCS state unavailable: jj workspace not found".into()),
                );
            };
            let Some(working_copy) = jj.working_copy.as_ref() else {
                return (
                    EvidenceFreshness::ProbablyFresh,
                    Some("current VCS state unavailable: jj working copy not found".into()),
                );
            };
            let freshness = classify_evidence_freshness(
                metadata,
                Some(working_copy.change_id.as_str()),
                Some(working_copy.commit_id.as_str()),
                true,
            );
            let reason = match freshness {
                EvidenceFreshness::Stale => {
                    Some("current jj change differs from the evidence capture point".into())
                }
                EvidenceFreshness::ProbablyFresh => {
                    Some("current jj working-copy freshness is partial".into())
                }
                _ => None,
            };
            (freshness, reason)
        }
    }
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

fn print_doctor_findings(store: &Store, data_root: &Path, json: bool) -> Result<()> {
    let findings = doctor_findings(store, data_root)?;
    if json {
        let counts = spool_counts(capture_inbox_dir(data_root))?;
        print_json(serde_json::json!({
            "schema_version": 1,
            "share_safe": false,
            "valid": findings.is_empty(),
            "findings": findings,
            "spool": {
                "pending": counts.pending,
                "tmp": counts.tmp,
                "processing": counts.processing,
                "done": counts.done,
                "failed": counts.failed,
            },
            "local_only": true,
        }))?;
    } else if findings.is_empty() {
        println!("valid");
    } else {
        for finding in findings {
            println!("{finding}");
        }
    }
    Ok(())
}

fn doctor_findings(store: &Store, data_root: &Path) -> Result<Vec<String>> {
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
    Ok(findings)
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
            let import_record_id = provider_import_record_id(args.provider, &args.input, &fixture);
            let provisional_record = provider_import_record(
                import_record_id,
                args.provider,
                &args.input,
                None,
                &fixture,
            );
            match store.get_record(import_record_id) {
                Ok(_) => {}
                Err(StoreError::NotFound(_)) => store.upsert_record(&provisional_record)?,
                Err(err) => return Err(err.into()),
            }
            let summary = import_provider_fixture_jsonl(
                &args.input,
                &mut store,
                ProviderFixtureImportOptions {
                    source_path: Some(args.input.clone()),
                    work_record_id: Some(import_record_id),
                    expected_provider: Some(args.provider.capture_provider()),
                    ..ProviderFixtureImportOptions::default()
                },
            )?;
            let record = if summary.imported_sessions > 0 || summary.imported_events > 0 {
                Some(upsert_provider_import_summary_record(
                    &store,
                    import_record_id,
                    args.provider,
                    &args.input,
                    &summary,
                    &fixture,
                )?)
            } else {
                None
            };
            if args.json {
                let mut import = serde_json::to_value(&summary)?;
                redact_json_strings(&mut import);
                print_json(serde_json::json!({
                    "schema_version": 1,
                    "share_safe": true,
                    "provider": args.provider.as_str(),
                    "input": redact_share_safe_markers(&args.input.display().to_string()),
                    "import": import,
                    "record": record.as_ref().map(share_safe_record_value),
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
        CaptureSubcommand::ImportCodexHistory(args) => {
            let mut store = Store::open(database_path(data_root.clone()))?;
            auto_import_pending_spool(&data_root, &mut store)?;
            let (summary, record) = import_codex_history_with_record(&mut store, &args.input)?;
            if args.json {
                let mut import = serde_json::to_value(&summary)?;
                redact_json_strings(&mut import);
                print_json(serde_json::json!({
                    "schema_version": 1,
                    "share_safe": true,
                    "provider": "codex",
                    "source_format": "codex_history_jsonl",
                    "fidelity": "summary_only",
                    "input": redact_share_safe_markers(&args.input.display().to_string()),
                    "import": import,
                    "record": record.as_ref().map(share_safe_record_value),
                    "limitations": [
                        "imports Codex prompt history only",
                        "does not include assistant replies",
                        "does not include tool calls or command output",
                        "does not infer child sessions"
                    ],
                }))?;
            } else {
                println!(
                    "imported {} codex history item(s), skipped {}, failed {}",
                    summary.imported, summary.skipped, summary.failed
                );
                println!(
                    "fidelity: summary_only; source_format: codex_history_jsonl; prompts only"
                );
                if let Some(record) = record {
                    println!("record: {}", record.id);
                }
            }
            if summary.failed > 0 {
                return Err(anyhow!(
                    "failed to import {} codex history line(s)",
                    summary.failed
                ));
            }
        }
        CaptureSubcommand::ImportPiSession(args) => {
            let mut store = Store::open(database_path(data_root.clone()))?;
            auto_import_pending_spool(&data_root, &mut store)?;
            let (summary, record) = import_pi_session_with_record(&mut store, &args.input)?;
            if args.json {
                let mut import = serde_json::to_value(&summary)?;
                redact_json_strings(&mut import);
                print_json(serde_json::json!({
                    "schema_version": 1,
                    "share_safe": true,
                    "provider": "pi",
                    "source_format": "pi_session_jsonl",
                    "fidelity": "imported",
                    "input": redact_share_safe_markers(&args.input.display().to_string()),
                    "import": import,
                    "record": record.as_ref().map(share_safe_record_value),
                    "limitations": [
                        "preserves Pi message tree entry ids and parent ids as event metadata",
                        "does not map Pi message branches to ctx subagent session edges",
                        "does not expand image blocks into ctx artifacts"
                    ],
                }))?;
            } else {
                println!(
                    "imported {} pi session item(s), skipped {}, failed {}",
                    summary.imported, summary.skipped, summary.failed
                );
                println!("fidelity: imported; source_format: pi_session_jsonl");
                if let Some(record) = record {
                    println!("record: {}", record.id);
                }
            }
            if summary.failed > 0 {
                return Err(anyhow!(
                    "failed to import {} pi session line(s)",
                    summary.failed
                ));
            }
        }
        CaptureSubcommand::ImportLocalProviders(args) => {
            let mut store = Store::open(database_path(data_root.clone()))?;
            auto_import_pending_spool(&data_root, &mut store)?;
            let report = import_local_providers(&mut store)?;
            if args.json {
                print_json(report.to_json())?;
            } else {
                report.print_human();
            }
        }
    }
    Ok(())
}

struct LocalProviderImportReport {
    entries: Vec<LocalProviderEntry>,
}

struct LocalProviderEntry {
    provider: &'static str,
    status: &'static str,
    support_status: &'static str,
    path: Option<PathBuf>,
    source_format: Option<&'static str>,
    fidelity: Option<&'static str>,
    imported_sessions: usize,
    imported_events: usize,
    skipped: usize,
    failed: usize,
    blocker: Option<String>,
}

impl LocalProviderImportReport {
    fn to_json(&self) -> serde_json::Value {
        let providers: Vec<_> = self
            .entries
            .iter()
            .map(|entry| {
                let mut value = serde_json::json!({
                    "provider": entry.provider,
                    "status": entry.status,
                    "support_status": entry.support_status,
                    "source_format": entry.source_format,
                    "fidelity": entry.fidelity,
                    "imported_sessions": entry.imported_sessions,
                    "imported_events": entry.imported_events,
                    "skipped": entry.skipped,
                    "failed": entry.failed,
                    "blocker": entry.blocker,
                });
                if let Some(path) = entry.path.as_ref() {
                    value["path"] = serde_json::Value::String(redact_share_safe_markers(
                        &path.display().to_string(),
                    ));
                }
                value
            })
            .collect();
        serde_json::json!({
            "schema_version": 1,
            "share_safe": true,
            "providers": providers,
        })
    }

    fn print_human(&self) {
        for entry in &self.entries {
            match entry.path.as_ref() {
                Some(path) => println!("{}: {} {}", entry.provider, entry.status, path.display()),
                None => println!("{}: {}", entry.provider, entry.status),
            }
            println!(
                "{}_support_status: {}",
                entry.provider, entry.support_status
            );
            if let Some(format) = entry.source_format {
                println!("{}_source_format: {}", entry.provider, format);
            }
            if let Some(fidelity) = entry.fidelity {
                println!("{}_fidelity: {}", entry.provider, fidelity);
            }
            if entry.imported_sessions > 0
                || entry.imported_events > 0
                || entry.skipped > 0
                || entry.failed > 0
            {
                println!(
                    "{}_imported: sessions={} events={} skipped={} failed={}",
                    entry.provider,
                    entry.imported_sessions,
                    entry.imported_events,
                    entry.skipped,
                    entry.failed
                );
            }
            if let Some(blocker) = &entry.blocker {
                println!("{}_blocker: {}", entry.provider, blocker);
            }
        }
    }
}

fn import_local_providers(store: &mut Store) -> Result<LocalProviderImportReport> {
    let mut entries = Vec::new();

    if let Some(path) = discover_codex_history_path() {
        match import_codex_history_with_record(store, &path) {
            Ok((summary, _)) => entries.push(LocalProviderEntry {
                provider: "codex",
                status: "imported",
                support_status: "supported-import",
                path: Some(path),
                source_format: Some("codex_history_jsonl"),
                fidelity: Some("summary_only"),
                imported_sessions: summary.imported_sessions,
                imported_events: summary.imported_events,
                skipped: summary.skipped,
                failed: summary.failed,
                blocker: Some(
                    "prompt history only; no assistant replies, tool calls, command output, artifacts, or child sessions"
                        .to_owned(),
                ),
            }),
            Err(err) => entries.push(LocalProviderEntry {
                provider: "codex",
                status: "failed",
                support_status: "supported-import",
                path: Some(path),
                source_format: Some("codex_history_jsonl"),
                fidelity: Some("summary_only"),
                imported_sessions: 0,
                imported_events: 0,
                skipped: 0,
                failed: 1,
                blocker: Some(err.to_string()),
            }),
        }
    } else {
        entries.push(LocalProviderEntry {
            provider: "codex",
            status: "missing",
            support_status: "supported-import",
            path: default_home_path(&[".codex", "history.jsonl"]),
            source_format: Some("codex_history_jsonl"),
            fidelity: Some("summary_only"),
            imported_sessions: 0,
            imported_events: 0,
            skipped: 0,
            failed: 0,
            blocker: Some("known Codex prompt history file was not found".to_owned()),
        });
    }

    entries.push(provider_unsupported_entry(
        "claude",
        "fixture-only",
        discover_first_existing(&[&[".claude", "projects"], &[".claude"]]),
        "Claude Code native transcript import is not implemented in this branch; hooks and transcript paths are documented, but ctx has not installed a safe hook adapter or parser; use normalized fixture import only",
    ));
    let pi_paths = discover_pi_session_paths();
    if pi_paths.is_empty() {
        entries.push(provider_unsupported_entry(
            "pi",
            "supported-import",
            discover_first_existing(&[&[".pi", "agent"], &[".pi"]]),
            "Pi session import is implemented for pi_session_jsonl, but no Pi session JSONL files were found under ~/.pi/agent/sessions; use import-pi-session with an explicit file or normalized fixture import",
        ));
    } else {
        let mut merged = ProviderImportSummary::default();
        let mut failed = 0;
        let mut blocker = None;
        for path in &pi_paths {
            match import_pi_session_with_record(store, path) {
                Ok((summary, _)) => merge_provider_import_summary(&mut merged, summary),
                Err(err) => {
                    failed += 1;
                    blocker = Some(err.to_string());
                }
            }
        }
        entries.push(LocalProviderEntry {
            provider: "pi",
            status: if failed == 0 { "imported" } else { "failed" },
            support_status: "supported-import",
            path: discover_pi_session_dir(),
            source_format: Some("pi_session_jsonl"),
            fidelity: Some("imported"),
            imported_sessions: merged.imported_sessions,
            imported_events: merged.imported_events,
            skipped: merged.skipped,
            failed: merged.failed + failed,
            blocker: blocker.or_else(|| {
                Some(format!(
                    "imported up to {} discovered Pi session JSONL file(s); message branch parentId values are preserved as event metadata",
                    pi_paths.len()
                ))
            }),
        });
    }
    entries.push(provider_unsupported_entry(
        "opencode",
        "fixture-only",
        discover_opencode_surface(),
        "OpenCode session/export, DB path, plugins, and ACP are documented, but ctx has no native OpenCode DB/export parser in this branch; use normalized fixture import only",
    ));
    entries.push(provider_unsupported_entry(
        "antigravity",
        "fixture-only",
        discover_provider_surface(
            &["agy", "antigravity"],
            &[&[".antigravity"], &[".config", "antigravity"]],
        ),
        "Antigravity CLI native transcript import is blocked until a stable local transcript path/schema is proven; hook capture is not installed by ctx in this branch",
    ));
    entries.push(provider_unsupported_entry(
        "gemini",
        "fixture-only",
        discover_provider_surface(&["gemini"], &[&[".gemini"]]),
        "Gemini CLI exposes sessions, hooks, and telemetry in current docs, but ctx has no native session/telemetry importer or installed hook capture in this branch; use normalized fixture import only",
    ));
    entries.push(provider_unsupported_entry(
        "cursor",
        "fixture-only",
        discover_provider_surface(
            &["cursor-agent", "cursor"],
            &[&[".cursor"], &[".config", "Cursor"], &[".config", "cursor"]],
        ),
        "Cursor CLI/editor local transcript formats are not parsed by ctx in this branch; use normalized fixture import only",
    ));
    entries.push(provider_unsupported_entry(
        "copilot_cli",
        "detected-unsupported",
        discover_first_existing(&[
            &[".copilot"],
            &[".config", "gh", "extensions", "gh-copilot"],
        ]),
        "Copilot CLI local configuration/session data can be detected, but no ctx Copilot session parser or hook adapter is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "factory_droid",
        "detected-unsupported",
        discover_first_existing_any(&[&[".factory"]], &[&[".factory"]]),
        "Factory Droid configuration can be detected, but no ctx Droid session parser, hook adapter, or droid exec JSON-RPC adapter is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "goose",
        "detected-unsupported",
        discover_first_existing(&[
            &[".config", "goose", "config.yaml"],
            &[".local", "share", "goose", "sessions", "sessions.db"],
            &[".local", "share", "goose", "sessions"],
        ]),
        "Goose local sessions can be detected, but no ctx sessions.db or legacy JSONL parser is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "amp",
        "detected-unsupported",
        discover_first_existing_any(
            &[
                &[".config", "amp", "settings.json"],
                &[".config", "amp", "settings.jsonc"],
            ],
            &[&[".amp", "settings.json"], &[".amp", "settings.jsonc"]],
        ),
        "Amp settings can be detected, but no ctx Amp thread importer, SDK adapter, or wrapper is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "openhands",
        "detected-unsupported",
        discover_first_existing(&[
            &[".openhands", "conversations"],
            &[".openhands", "settings.json"],
            &[".openhands", "agent_settings.json"],
        ]),
        "OpenHands local configuration/conversations can be detected, but no ctx ConversationState/EventLog parser is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "qwen",
        "detected-unsupported",
        discover_first_existing_any(
            &[&[".qwen", "settings.json"], &[".qwen", "tmp"], &[".qwen"]],
            &[&[".qwen"]],
        ),
        "Qwen Code configuration can be detected, but no ctx transcript parser is implemented and documented shell_history is not a full transcript",
    ));
    entries.push(provider_unsupported_entry(
        "mistral",
        "detected-unsupported",
        discover_first_existing_any(
            &[&[".vibe", "config.toml"], &[".vibe"]],
            &[&[".vibe", "config.toml"], &[".vibe"]],
        ),
        "Mistral Vibe configuration can be detected, but no ctx Vibe transcript parser or ACP adapter is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "kimi",
        "detected-unsupported",
        discover_first_existing(&[&[".kimi-code", "config.toml"], &[".kimi-code"]]),
        "Kimi Code local data can be detected, but no ctx Kimi session-record parser, hook adapter, or ACP adapter is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "cagent",
        "detected-unsupported",
        discover_first_existing(&[&[".cagent", "cagent.debug.log"], &[".cagent"]]),
        "Docker cagent local state/log paths can be detected, but debug logs are not treated as a stable transcript import contract",
    ));
    entries.push(provider_unsupported_entry(
        "aider",
        "detected-unsupported",
        discover_first_existing_any(
            &[&[".aider.conf.yml"]],
            &[
                &[".aider.chat.history.md"],
                &[".aider.input.history"],
                &[".aider.conf.yml"],
            ],
        ),
        "Aider chat/config files can be detected, but no ctx markdown chat-history parser is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "cline_roo",
        "detected-unsupported",
        discover_first_existing(&[
            &[
                ".config",
                "Code",
                "User",
                "globalStorage",
                "saoudrizwan.claude-dev",
            ],
            &[".config", "Code", "User", "globalStorage", "cline.cline"],
            &[
                ".config",
                "Code",
                "User",
                "globalStorage",
                "rooveterinaryinc.roo-cline",
            ],
            &[
                ".config",
                "Code",
                "User",
                "globalStorage",
                "roocode.roo-code",
            ],
        ]),
        "Cline/Roo VS Code storage can be detected, but no ctx task-directory parser or extension adapter is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "continue_cody",
        "detected-unsupported",
        discover_first_existing(&[
            &[".continue", "config.yaml"],
            &[".continue", "logs"],
            &[
                ".config",
                "Code",
                "User",
                "globalStorage",
                "continue.continue",
            ],
            &[
                ".config",
                "Code",
                "User",
                "globalStorage",
                "sourcegraph.cody-ai",
            ],
        ]),
        "Continue/Cody local config or extension storage can be detected, but no ctx chat-history importer is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "auggie",
        "detected-unsupported",
        discover_first_existing_any(
            &[&[".augment", "settings.json"], &[".augment"]],
            &[&[".augment", "settings.json"], &[".augment"]],
        ),
        "Auggie settings can be detected, but no ctx session-list, structured print-mode, or ACP adapter is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "junie",
        "detected-unsupported",
        discover_first_existing(&[&[".junie", "allowlist.json"], &[".junie"]]),
        "Junie local configuration can be detected, but no ctx prompt/session-history parser is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "kilo",
        "detected-unsupported",
        discover_first_existing(&[
            &[
                ".config",
                "Code",
                "User",
                "globalStorage",
                "kilocode.kilo-code",
            ],
            &[
                ".config",
                "Code",
                "User",
                "globalStorage",
                "kilo-org.kilocode",
            ],
        ]),
        "Kilo VS Code storage can be detected, but no ctx task-history parser or extension adapter is implemented",
    ));
    entries.push(provider_unsupported_entry(
        "swe_agent",
        "detected-unsupported",
        discover_first_existing_any(&[&[".swe-agent"]], &[&["trajectories"]]),
        "SWE-agent trajectory output can be detected, but no ctx .traj parser is implemented",
    ));

    Ok(LocalProviderImportReport { entries })
}

fn import_codex_history_with_record(
    store: &mut Store,
    input: &Path,
) -> Result<(ProviderImportSummary, Option<WorkRecord>)> {
    let import_record_id = codex_history_import_record_id(input);
    let summary = import_codex_history_jsonl(
        input,
        store,
        CodexHistoryImportOptions {
            source_path: Some(input.to_path_buf()),
            ..CodexHistoryImportOptions::default()
        },
    )?;
    let record = if summary.imported_sessions > 0 || summary.imported_events > 0 {
        Some(upsert_codex_history_import_summary_record(
            store,
            import_record_id,
            input,
            &summary,
        )?)
    } else {
        None
    };
    Ok((summary, record))
}

fn import_pi_session_with_record(
    store: &mut Store,
    input: &Path,
) -> Result<(ProviderImportSummary, Option<WorkRecord>)> {
    let import_record_id = pi_session_import_record_id(input);
    let provisional_record =
        pi_session_import_record(import_record_id, input, &ProviderImportSummary::default());
    match store.get_record(import_record_id) {
        Ok(_) => {}
        Err(StoreError::NotFound(_)) => store.upsert_record(&provisional_record)?,
        Err(err) => return Err(err.into()),
    }
    let summary = import_pi_session_jsonl(
        input,
        store,
        PiSessionImportOptions {
            source_path: Some(input.to_path_buf()),
            work_record_id: Some(import_record_id),
            ..PiSessionImportOptions::default()
        },
    )?;
    let record = if summary.imported_sessions > 0 || summary.imported_events > 0 {
        Some(upsert_pi_session_import_summary_record(
            store,
            import_record_id,
            input,
            &summary,
        )?)
    } else {
        None
    };
    Ok((summary, record))
}

fn merge_provider_import_summary(target: &mut ProviderImportSummary, other: ProviderImportSummary) {
    target.imported += other.imported;
    target.skipped += other.skipped;
    target.failed += other.failed;
    target.redacted += other.redacted;
    target.imported_sessions += other.imported_sessions;
    target.skipped_sessions += other.skipped_sessions;
    target.imported_events += other.imported_events;
    target.skipped_events += other.skipped_events;
    target.imported_edges += other.imported_edges;
    target.skipped_edges += other.skipped_edges;
    target.failures.extend(other.failures);
}

fn provider_unsupported_entry(
    provider: &'static str,
    support_status: &'static str,
    path: Option<PathBuf>,
    blocker: &'static str,
) -> LocalProviderEntry {
    LocalProviderEntry {
        provider,
        status: if path.is_some() {
            "discovered_unsupported"
        } else {
            "missing"
        },
        support_status,
        path,
        source_format: None,
        fidelity: None,
        imported_sessions: 0,
        imported_events: 0,
        skipped: 0,
        failed: 0,
        blocker: Some(blocker.to_owned()),
    }
}

fn discover_codex_history_path() -> Option<PathBuf> {
    default_home_path(&[".codex", "history.jsonl"]).filter(|path| path.is_file())
}

fn discover_pi_session_dir() -> Option<PathBuf> {
    default_home_path(&[".pi", "agent", "sessions"]).filter(|path| path.is_dir())
}

fn discover_pi_session_paths() -> Vec<PathBuf> {
    let Some(root) = discover_pi_session_dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_jsonl_files_bounded(&root, 4, 100, &mut out);
    out.sort();
    out
}

fn collect_jsonl_files_bounded(
    root: &Path,
    depth: usize,
    max_files: usize,
    out: &mut Vec<PathBuf>,
) {
    if depth == 0 || out.len() >= max_files {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    for path in paths {
        if out.len() >= max_files {
            return;
        }
        if path.is_dir() {
            collect_jsonl_files_bounded(&path, depth - 1, max_files, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

fn discover_opencode_surface() -> Option<PathBuf> {
    discover_provider_surface(
        &["opencode"],
        &[&[".local", "share", "opencode"], &[".config", "opencode"]],
    )
}

fn discover_provider_surface(commands: &[&str], paths: &[&[&str]]) -> Option<PathBuf> {
    discover_command(commands).or_else(|| discover_first_existing(paths))
}

fn discover_command(candidates: &[&str]) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        for candidate in candidates {
            let command = dir.join(candidate);
            if command.is_file() {
                return Some(command);
            }
            #[cfg(windows)]
            {
                for extension in ["exe", "cmd", "bat"] {
                    let command = dir.join(format!("{candidate}.{extension}"));
                    if command.is_file() {
                        return Some(command);
                    }
                }
            }
        }
    }
    None
}

fn discover_first_existing(candidates: &[&[&str]]) -> Option<PathBuf> {
    candidates
        .iter()
        .filter_map(|segments| default_home_path(segments))
        .find(|path| path.exists())
}

fn discover_first_existing_any(
    home_candidates: &[&[&str]],
    cwd_candidates: &[&[&str]],
) -> Option<PathBuf> {
    discover_first_existing(home_candidates)
        .or_else(|| discover_first_existing_under_current_dir(cwd_candidates))
}

fn discover_first_existing_under_current_dir(candidates: &[&[&str]]) -> Option<PathBuf> {
    let cwd = env::current_dir().ok()?;
    candidates
        .iter()
        .map(|segments| path_from_segments(cwd.clone(), segments))
        .find(|path| path.exists())
}

fn default_home_path(segments: &[&str]) -> Option<PathBuf> {
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"))?;
    Some(path_from_segments(PathBuf::from(home), segments))
}

fn path_from_segments(mut path: PathBuf, segments: &[&str]) -> PathBuf {
    for segment in segments {
        path.push(segment);
    }
    path
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
        let provider = value["provider"].as_str().ok_or_else(|| {
            anyhow!("provider fixture line {line_number} is missing a string provider")
        })?;
        if provider != expected {
            return Err(anyhow!(
                "provider fixture line {} has provider `{}` but --provider is `{}`",
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

fn provider_import_record_id(
    provider: ProviderFixtureProvider,
    input: &Path,
    fixture: &ProviderFixtureSummary,
) -> Uuid {
    let key = if fixture.sessions.is_empty() {
        input.display().to_string()
    } else {
        fixture.sessions.join(",")
    };
    stable_capture_uuid(
        &format!("provider-import:{}:{key}", provider.as_str()),
        "record",
    )
}

fn provider_import_record(
    record_id: Uuid,
    provider: ProviderFixtureProvider,
    input: &Path,
    import: Option<&ProviderImportSummary>,
    fixture: &ProviderFixtureSummary,
) -> WorkRecord {
    let provider_name = provider.as_str();
    let mut body = String::new();
    body.push_str(&format!(
        "Provider fixture import for {provider_name} from {}.\n",
        input.display()
    ));
    if let Some(import) = import {
        body.push_str(&format!(
            "Imported {} sessions, {} events, {} edges; skipped {} items; redacted {} fields.",
            import.imported_sessions,
            import.imported_events,
            import.imported_edges,
            import.skipped,
            import.redacted
        ));
        if import.failed > 0 {
            body.push_str(&format!(" Failed {} line(s).", import.failed));
        }
    } else {
        body.push_str("Provider sessions and events are linked to this Work Record.");
    }
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

    let mut record = WorkRecord::new(
        format!("Imported {provider_name} provider fixture"),
        body,
        vec!["provider-import".to_owned(), provider_name.to_owned()],
        "provider-import",
        input.parent().map(|path| path.display().to_string()),
    );
    record.id = record_id;
    record
}

fn upsert_provider_import_summary_record(
    store: &Store,
    record_id: Uuid,
    provider: ProviderFixtureProvider,
    input: &Path,
    import: &ProviderImportSummary,
    fixture: &ProviderFixtureSummary,
) -> Result<WorkRecord> {
    let provider_name = provider.as_str();
    let record = provider_import_record(record_id, provider, input, Some(import), fixture);
    store.upsert_record(&record)?;
    let summary_text = record.body.clone();

    let now = Utc::now();
    store.upsert_summary(&Summary {
        id: new_id(),
        work_record_id: Some(record.id),
        session_id: None,
        kind: SummaryKind::ImportedProviderSummary,
        model_or_source: Some(format!("{provider_name}-fixture")),
        text: summary_text,
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

fn codex_history_import_record_id(input: &Path) -> Uuid {
    stable_capture_uuid(
        &format!("provider-import:codex-history:{}", input.display()),
        "record",
    )
}

fn pi_session_import_record_id(input: &Path) -> Uuid {
    stable_capture_uuid(
        &format!("provider-import:pi-session:{}", input.display()),
        "record",
    )
}

fn codex_history_import_record(
    record_id: Uuid,
    input: &Path,
    import: &ProviderImportSummary,
) -> WorkRecord {
    let mut body = String::new();
    body.push_str(&format!(
        "Codex prompt history import from {}.\n",
        input.display()
    ));
    body.push_str(&format!(
        "Imported {} sessions and {} prompt events; skipped {} items; redacted {} fields.",
        import.imported_sessions, import.imported_events, import.skipped, import.redacted
    ));
    if import.failed > 0 {
        body.push_str(&format!(" Failed {} line(s).", import.failed));
    }
    body.push_str(
        "\n\nFidelity: summary_only. Source format: codex_history_jsonl. This path imports user prompts from Codex history only; it does not include assistant replies, tool calls, command output, or child session relationships.",
    );

    let mut record = WorkRecord::new(
        "Imported Codex prompt history",
        body,
        vec![
            "provider-import".to_owned(),
            "codex".to_owned(),
            "summary-only".to_owned(),
        ],
        "provider-import",
        input.parent().map(|path| path.display().to_string()),
    );
    record.id = record_id;
    record
}

fn upsert_codex_history_import_summary_record(
    store: &Store,
    record_id: Uuid,
    input: &Path,
    import: &ProviderImportSummary,
) -> Result<WorkRecord> {
    let record = codex_history_import_record(record_id, input, import);
    store.upsert_record(&record)?;

    let now = Utc::now();
    store.upsert_summary(&Summary {
        id: new_id(),
        work_record_id: Some(record.id),
        session_id: None,
        kind: SummaryKind::ImportedProviderSummary,
        model_or_source: Some("codex-history".to_owned()),
        text: record.body.clone(),
        citations: Vec::new(),
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        source_id: None,
        sync: SyncMetadata {
            fidelity: Fidelity::SummaryOnly,
            metadata: serde_json::json!({
                "provider": "codex",
                "source_format": "codex_history_jsonl",
                "source_fidelity": "prompt_log_only",
                "input": input.display().to_string(),
                "imported_sessions": import.imported_sessions,
                "imported_events": import.imported_events,
            }),
            ..SyncMetadata::default()
        },
    })?;

    Ok(record)
}

fn pi_session_import_record(
    record_id: Uuid,
    input: &Path,
    import: &ProviderImportSummary,
) -> WorkRecord {
    let mut body = String::new();
    body.push_str(&format!("Pi session import from {}.\n", input.display()));
    body.push_str(&format!(
        "Imported {} sessions and {} events; skipped {} items; redacted {} fields.",
        import.imported_sessions, import.imported_events, import.skipped, import.redacted
    ));
    if import.failed > 0 {
        body.push_str(&format!(" Failed {} line(s).", import.failed));
    }
    body.push_str(
        "\n\nFidelity: imported. Source format: pi_session_jsonl. Pi message tree entry ids and parent ids are preserved in event metadata; branch edges are not converted into ctx subagent session edges.",
    );

    let mut record = WorkRecord::new(
        "Imported Pi session",
        body,
        vec![
            "provider-import".to_owned(),
            "pi".to_owned(),
            "session-jsonl".to_owned(),
        ],
        "provider-import",
        input.parent().map(|path| path.display().to_string()),
    );
    record.id = record_id;
    record
}

fn upsert_pi_session_import_summary_record(
    store: &Store,
    record_id: Uuid,
    input: &Path,
    import: &ProviderImportSummary,
) -> Result<WorkRecord> {
    let record = pi_session_import_record(record_id, input, import);
    store.upsert_record(&record)?;

    let now = Utc::now();
    store.upsert_summary(&Summary {
        id: new_id(),
        work_record_id: Some(record.id),
        session_id: None,
        kind: SummaryKind::ImportedProviderSummary,
        model_or_source: Some("pi-session".to_owned()),
        text: record.body.clone(),
        citations: Vec::new(),
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        source_id: None,
        sync: SyncMetadata {
            fidelity: Fidelity::Imported,
            metadata: serde_json::json!({
                "provider": "pi",
                "source_format": "pi_session_jsonl",
                "source_fidelity": "documented_session_jsonl",
                "input": input.display().to_string(),
                "imported_sessions": import.imported_sessions,
                "imported_events": import.imported_events,
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
        ShimSubcommand::ActivateShell(args) => activate_shell_rc(&args.shell_rc, &args.dir),
        ShimSubcommand::DeactivateShell(args) => deactivate_shell_rc(&args.shell_rc),
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

fn activate_shell_rc(shell_rc: &Path, shim_dir: &Path) -> Result<()> {
    if let Some(parent) = shell_rc.parent() {
        fs::create_dir_all(parent)?;
    }
    let original = read_optional_shell_rc(shell_rc)?;
    let cleaned = remove_shell_rc_block(&original);
    let mut updated = cleaned;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(SHELL_RC_BEGIN);
    updated.push('\n');
    updated.push_str(&format!(
        "export PATH={}:$PATH\n",
        shell_escape_path(shim_dir)
    ));
    updated.push_str(SHELL_RC_END);
    updated.push('\n');
    if shell_rc.exists() {
        let backup = shell_rc_backup_path(shell_rc);
        fs::copy(shell_rc, &backup).with_context(|| {
            format!(
                "backup shell rc {} to {}",
                shell_rc.display(),
                backup.display()
            )
        })?;
        println!("backup: {}", backup.display());
    }
    fs::write(shell_rc, updated)
        .with_context(|| format!("write shell rc {}", shell_rc.display()))?;
    println!("activated {}", shell_rc.display());
    Ok(())
}

fn read_optional_shell_rc(shell_rc: &Path) -> Result<String> {
    match fs::read_to_string(shell_rc) {
        Ok(contents) => Ok(contents),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("read shell rc {}", shell_rc.display())),
    }
}

fn deactivate_shell_rc(shell_rc: &Path) -> Result<()> {
    let original = fs::read_to_string(shell_rc)
        .with_context(|| format!("read shell rc {}", shell_rc.display()))?;
    let updated = remove_shell_rc_block(&original);
    if updated != original {
        let backup = shell_rc_backup_path(shell_rc);
        fs::copy(shell_rc, &backup).with_context(|| {
            format!(
                "backup shell rc {} to {}",
                shell_rc.display(),
                backup.display()
            )
        })?;
        fs::write(shell_rc, updated)
            .with_context(|| format!("write shell rc {}", shell_rc.display()))?;
    }
    println!("deactivated {}", shell_rc.display());
    Ok(())
}

fn remove_shell_rc_block(input: &str) -> String {
    let mut output = String::new();
    let mut in_block = false;
    for line in input.lines() {
        if line == SHELL_RC_BEGIN {
            in_block = true;
            continue;
        }
        if line == SHELL_RC_END {
            in_block = false;
            continue;
        }
        if !in_block {
            output.push_str(line);
            output.push('\n');
        }
    }
    output
}

fn shell_rc_backup_path(shell_rc: &Path) -> PathBuf {
    let mut backup = shell_rc.as_os_str().to_os_string();
    backup.push(".ctxbak");
    PathBuf::from(backup)
}

fn is_ctx_shim(path: &Path) -> Result<bool> {
    let contents = fs::read(path).with_context(|| format!("read shim {}", path.display()))?;
    Ok(contents
        .windows(b"CTX_WORK_RECORD_SHIM=1".len())
        .any(|window| window == b"CTX_WORK_RECORD_SHIM=1"))
}

enum PassiveShimStatus {
    Active(PathBuf),
    InstalledNotActive(PathBuf),
    External(PathBuf),
    Unreadable(PathBuf, String),
    Missing,
}

impl PassiveShimStatus {
    fn state(&self) -> &'static str {
        match self {
            Self::Active(_) => "active",
            Self::InstalledNotActive(_) => "installed_not_active",
            Self::External(_) => "external",
            Self::Unreadable(_, _) => "unreadable",
            Self::Missing => "missing",
        }
    }

    fn path(&self) -> Option<&Path> {
        match self {
            Self::Active(path)
            | Self::InstalledNotActive(path)
            | Self::External(path)
            | Self::Unreadable(path, _) => Some(path),
            Self::Missing => None,
        }
    }

    fn display(&self) -> String {
        match self {
            Self::Active(path) => format!("installed {}", path.display()),
            Self::InstalledNotActive(path) => format!("installed_not_active {}", path.display()),
            Self::External(path) => format!("external {}", path.display()),
            Self::Unreadable(path, error) => {
                format!("unreadable {} ({error})", path.display())
            }
            Self::Missing => "missing".to_owned(),
        }
    }
}

enum ShimFileStatus {
    CtxShim,
    Other,
    Unreadable(String),
}

fn classify_shim_file(path: &Path) -> ShimFileStatus {
    match is_ctx_shim(path) {
        Ok(true) => ShimFileStatus::CtxShim,
        Ok(false) => ShimFileStatus::Other,
        Err(error) => ShimFileStatus::Unreadable(error.to_string()),
    }
}

fn passive_shim_status(tool: ShimTool, configured_dir: &Path) -> Result<PassiveShimStatus> {
    let configured = configured_dir.join(tool.as_str());
    let configured_status = configured
        .is_file()
        .then(|| classify_shim_file(&configured));
    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join(tool.as_str());
            if candidate.is_file() {
                match classify_shim_file(&candidate) {
                    ShimFileStatus::CtxShim => return Ok(PassiveShimStatus::Active(candidate)),
                    ShimFileStatus::Other => {
                        if matches!(&configured_status, Some(ShimFileStatus::CtxShim)) {
                            return Ok(PassiveShimStatus::InstalledNotActive(configured));
                        }
                        return Ok(PassiveShimStatus::External(candidate));
                    }
                    ShimFileStatus::Unreadable(error) => {
                        return Ok(PassiveShimStatus::Unreadable(candidate, error));
                    }
                }
            }
        }
    }

    match configured_status {
        Some(ShimFileStatus::CtxShim) => {
            return Ok(PassiveShimStatus::InstalledNotActive(configured));
        }
        Some(ShimFileStatus::Unreadable(error)) => {
            return Ok(PassiveShimStatus::Unreadable(configured, error));
        }
        Some(ShimFileStatus::Other) | None => {}
    }

    Ok(PassiveShimStatus::Missing)
}

fn wrapper_script(tool: ShimTool, ctx_bin: &Path) -> Result<String> {
    let tool_name = tool.as_str();
    let ctx_bin = shell_escape_path(ctx_bin);
    Ok(format!(
r#"#!/bin/sh
# CTX_WORK_RECORD_SHIM=1
tool="{tool_name}"
case "$0" in
    */*) shim_script_dir=${{0%/*}} ;;
    *) shim_script_dir=. ;;
esac
shim_dir=$(CDPATH= cd -- "$shim_script_dir" && pwd)
ctx_bin="${{CTX_SHIM_CTX_BIN:-{ctx_bin}}}"
ctx_cat=$(command -p -v cat 2>/dev/null || command -v cat 2>/dev/null || printf '%s\n' cat)
ctx_date=$(command -p -v date 2>/dev/null || command -v date 2>/dev/null || printf '%s\n' date)
ctx_mkdir=$(command -p -v mkdir 2>/dev/null || command -v mkdir 2>/dev/null || printf '%s\n' mkdir)
ctx_mktemp=$(command -p -v mktemp 2>/dev/null || command -v mktemp 2>/dev/null || printf '%s\n' mktemp)
ctx_rm=$(command -p -v rm 2>/dev/null || command -v rm 2>/dev/null || printf '%s\n' rm)
ctx_now_ms() {{
    seconds=$("$ctx_date" +%s 2>/dev/null || printf '%s\n' 0)
    millis=$("$ctx_date" +%s%3N 2>/dev/null || printf '%s\n' "")
    case "$millis" in
        ""|*[!0-9]*) printf '%s000\n' "$seconds" ;;
        *) printf '%s\n' "$millis" ;;
    esac
}}
old_ifs=$IFS
IFS=:
clean_path=
for entry in $PATH; do
    if [ -n "$entry" ]; then
        entry_dir=$(CDPATH= cd -- "$entry" 2>/dev/null && pwd)
    else
        entry_dir=$(pwd)
    fi
    if [ "$entry_dir" = "$shim_dir" ]; then
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
tmpdir=
configured_tmp=0
for tmpbase in "${{CTX_SHIM_TMPDIR:-}}" "${{CTX_DATA_ROOT:-}}" "${{TMPDIR:-}}"; do
    if [ -z "$tmpbase" ]; then
        continue
    fi
    configured_tmp=1
    "$ctx_mkdir" -p "$tmpbase" 2>/dev/null || continue
    tmpdir=$("$ctx_mktemp" -d "$tmpbase/ctx-shim-$tool.XXXXXX" 2>/dev/null) && break
done
if [ -z "$tmpdir" ] && [ "$configured_tmp" = 0 ]; then
    for tmpbase in /tmp .; do
        "$ctx_mkdir" -p "$tmpbase" 2>/dev/null || continue
        tmpdir=$("$ctx_mktemp" -d "$tmpbase/ctx-shim-$tool.XXXXXX" 2>/dev/null) && break
    done
fi
if [ -z "$tmpdir" ]; then
    exec "$real_cmd" "$@"
fi
stdout_file=$tmpdir/stdout
stderr_file=$tmpdir/stderr
started_at=$("$ctx_date" -u +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null || printf '%s\n' "1970-01-01T00:00:00Z")
start_ms=$(ctx_now_ms)
"$real_cmd" "$@" >"$stdout_file" 2>"$stderr_file"
status=$?
end_ms=$(ctx_now_ms)
duration_ms=$((end_ms - start_ms))
"$ctx_cat" "$stdout_file"
"$ctx_cat" "$stderr_file" >&2
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
"$ctx_rm" -rf "$tmpdir" >/dev/null 2>&1 || true
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

fn default_shim_dir(data_root: &Path) -> PathBuf {
    work_record_dir(data_root.to_path_buf()).join("shims")
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
            bind_evidence_to_observed_vcs(store, &evidence, started_at)?;
            let persisted_evidence = store.get_evidence(evidence.id)?;
            print_json(serde_json::json!({
                "schema_version": 1,
                "evidence": persisted_evidence,
                "metadata": store.get_evidence_metadata(evidence.id)?,
            }))?;
            if output.exit_code == 0 {
                Ok(())
            } else {
                Err(anyhow!("evidence command exited with {}", output.exit_code))
            }
        }
    }
}

fn bind_evidence_to_observed_vcs(
    store: &Store,
    evidence: &Evidence,
    observed_at: DateTime<Utc>,
) -> Result<()> {
    let Some(work_record_id) = evidence.record_id else {
        return Ok(());
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => return Ok(()),
    };
    let inspection = match inspect_path(&cwd) {
        Ok(inspection) => inspection,
        Err(_) => return Ok(()),
    };
    if let Some(git) = inspection.git.workspace.as_ref() {
        let metadata =
            evidence_metadata_for_git(store, evidence, work_record_id, git, observed_at)?;
        store.update_evidence_metadata(&metadata)?;
        return Ok(());
    }
    if let Some(jj) = inspection.jj.workspace.as_ref() {
        let metadata = evidence_metadata_for_jj(store, evidence, work_record_id, jj, observed_at)?;
        store.update_evidence_metadata(&metadata)?;
    }
    Ok(())
}

fn evidence_metadata_for_git(
    store: &Store,
    evidence: &Evidence,
    work_record_id: Uuid,
    git: &GitWorkspace,
    observed_at: DateTime<Utc>,
) -> Result<EvidenceMetadata> {
    let now = Utc::now();
    let workspace = VcsWorkspace {
        id: new_id(),
        kind: VcsKind::Git,
        root_path: git.root_path.clone(),
        repo_fingerprint: git.repo_fingerprint.value.clone(),
        primary_remote_url_normalized: git
            .primary_remote
            .as_ref()
            .map(|remote| remote.normalized_url.clone()),
        host: git
            .primary_remote
            .as_ref()
            .map(|remote| remote.host)
            .unwrap_or_default(),
        owner: git
            .primary_remote
            .as_ref()
            .and_then(|remote| remote.owner.clone()),
        name: git
            .primary_remote
            .as_ref()
            .and_then(|remote| remote.repo.clone()),
        monorepo_subpath: None,
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        source_id: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Full,
            ..SyncMetadata::default()
        },
    };
    let workspace_id = store.upsert_vcs_workspace(&workspace)?;
    store.register_local_workspace(
        &git.root_path,
        &git.repo_fingerprint.value,
        Some(workspace_id),
    )?;

    let change_id = git
        .head_sha
        .clone()
        .unwrap_or_else(|| format!("working-copy:{}", git.repo_fingerprint.value));
    let change = VcsChange {
        id: new_id(),
        vcs_workspace_id: workspace_id,
        kind: if git.head_sha.is_some() {
            VcsChangeKind::GitCommit
        } else {
            VcsChangeKind::WorkingCopy
        },
        change_id: change_id.clone(),
        parent_change_ids: Vec::new(),
        branch_or_bookmark: git.branch.clone(),
        tree_hash: git.tree_hash.clone(),
        author_time: None,
        confidence: Confidence::Explicit,
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        source_id: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Full,
            metadata: serde_json::json!({
                "observed_dirty": git.status.dirty,
            }),
            ..SyncMetadata::default()
        },
    };
    let change_id = store.upsert_vcs_change(&change)?;
    let mut metadata = store.get_evidence_metadata(evidence.id)?;
    metadata.work_record_id = work_record_id;
    metadata.vcs_change_id = Some(change_id);
    metadata.kind = evidence_kind_from_command(&evidence.command);
    metadata.status = evidence_status_from_exit(evidence.exit_code);
    metadata.freshness = if git.status.dirty {
        EvidenceFreshness::ProbablyFresh
    } else {
        EvidenceFreshness::Fresh
    };
    metadata.observed_head_sha = git.head_sha.clone();
    metadata.observed_tree_hash = git.tree_hash.clone();
    metadata.stale_reason = None;
    metadata.timestamps.updated_at = now;
    metadata.sync.fidelity = Fidelity::Full;
    metadata.sync.metadata = serde_json::json!({
        "observed_at": observed_at.to_rfc3339(),
        "vcs_kind": "git",
        "repo_fingerprint": git.repo_fingerprint.value,
        "repo_root": redacted_root_label(&git.root_path),
        "branch_or_bookmark": git.branch,
        "dirty": git.status.dirty,
        "staged": git.status.staged,
        "unstaged": git.status.unstaged,
        "untracked": git.status.untracked,
    });
    Ok(metadata)
}

fn evidence_metadata_for_jj(
    store: &Store,
    evidence: &Evidence,
    work_record_id: Uuid,
    jj: &JjWorkspace,
    observed_at: DateTime<Utc>,
) -> Result<EvidenceMetadata> {
    let now = Utc::now();
    let fingerprint = format!("jj:{}", redact_share_safe_markers(&jj.root_path));
    let workspace = VcsWorkspace {
        id: new_id(),
        kind: VcsKind::Jj,
        root_path: jj.root_path.clone(),
        repo_fingerprint: fingerprint.clone(),
        primary_remote_url_normalized: None,
        host: Default::default(),
        owner: None,
        name: None,
        monorepo_subpath: None,
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        source_id: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Partial,
            ..SyncMetadata::default()
        },
    };
    let workspace_id = store.upsert_vcs_workspace(&workspace)?;
    store.register_local_workspace(&jj.root_path, &fingerprint, Some(workspace_id))?;
    let working_copy = jj.working_copy.as_ref();
    let change = VcsChange {
        id: new_id(),
        vcs_workspace_id: workspace_id,
        kind: VcsChangeKind::JjChange,
        change_id: working_copy
            .map(|change| change.change_id.clone())
            .unwrap_or_else(|| "working-copy".to_owned()),
        parent_change_ids: working_copy
            .map(|change| change.parent_change_ids.clone())
            .unwrap_or_default(),
        branch_or_bookmark: working_copy
            .and_then(|change| change.bookmarks.first().cloned())
            .or_else(|| {
                jj.bookmarks
                    .iter()
                    .find(|bookmark| !bookmark.remote)
                    .map(|bookmark| bookmark.name.clone())
            }),
        tree_hash: working_copy.map(|change| change.commit_id.clone()),
        author_time: None,
        confidence: Confidence::Explicit,
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        source_id: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Partial,
            ..SyncMetadata::default()
        },
    };
    let change_id = store.upsert_vcs_change(&change)?;
    let mut metadata = store.get_evidence_metadata(evidence.id)?;
    metadata.work_record_id = work_record_id;
    metadata.vcs_change_id = Some(change_id);
    metadata.kind = evidence_kind_from_command(&evidence.command);
    metadata.status = evidence_status_from_exit(evidence.exit_code);
    metadata.freshness = EvidenceFreshness::ProbablyFresh;
    metadata.observed_head_sha = working_copy.map(|change| change.change_id.clone());
    metadata.observed_tree_hash = working_copy.map(|change| change.commit_id.clone());
    metadata.timestamps.updated_at = now;
    metadata.sync.fidelity = Fidelity::Partial;
    metadata.sync.metadata = serde_json::json!({
        "observed_at": observed_at.to_rfc3339(),
        "vcs_kind": "jj",
        "repo_fingerprint": fingerprint,
        "repo_root": redacted_root_label(&jj.root_path),
        "branch_or_bookmark": change.branch_or_bookmark,
        "dirty": true,
    });
    Ok(metadata)
}

fn evidence_status_from_exit(exit_code: i32) -> EvidenceStatus {
    if exit_code == 0 {
        EvidenceStatus::Passed
    } else {
        EvidenceStatus::Failed
    }
}

fn evidence_kind_from_command(command: &str) -> EvidenceKind {
    let lower = command.to_ascii_lowercase();
    if lower.contains("test") {
        EvidenceKind::Test
    } else if lower.contains("clippy") || lower.contains("lint") {
        EvidenceKind::Lint
    } else if lower.contains("build") {
        EvidenceKind::Build
    } else if lower.contains("check") || lower.contains("typecheck") {
        EvidenceKind::Typecheck
    } else {
        EvidenceKind::Manual
    }
}

fn redacted_root_label(root_path: &str) -> String {
    Path::new(root_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(|name| format!("[REDACTED_ROOT]/{name}"))
        .unwrap_or_else(|| "[REDACTED_ROOT]".to_owned())
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
            "share_safe": true,
            "record": share_safe_record_value(record),
        }))?;
    } else {
        let title = redact_share_safe_markers(&record.title);
        let body = redact_share_safe_markers(&record.body);
        println!("{} {}", record.id, title);
        if !body.is_empty() {
            println!("{body}");
        }
        if !record.tags.is_empty() {
            let tags = record
                .tags
                .iter()
                .map(|tag| redact_share_safe_markers(tag))
                .collect::<Vec<_>>();
            println!("tags: {}", tags.join(", "));
        }
        if let Some(pr_url) = &record.pr_url {
            println!("pr: {}", redact_share_safe_markers(pr_url));
        }
    }
    Ok(())
}

fn print_records(records: &[WorkRecord], json: bool) -> Result<()> {
    if json {
        let records = records
            .iter()
            .map(share_safe_record_value)
            .collect::<Vec<_>>();
        print_json(serde_json::json!({
            "schema_version": 1,
            "share_safe": true,
            "records": records,
        }))?;
    } else {
        for record in records {
            println!(
                "{} [{}] {}",
                record.id,
                redact_share_safe_markers(&record.kind),
                redact_share_safe_markers(&record.title)
            );
        }
    }
    Ok(())
}

fn share_safe_record_value(record: &WorkRecord) -> serde_json::Value {
    serde_json::json!({
        "id": record.id,
        "title": redact_share_safe_markers(&record.title),
        "body": redact_share_safe_markers(&record.body),
        "tags": record
            .tags
            .iter()
            .map(|tag| redact_share_safe_markers(tag))
            .collect::<Vec<_>>(),
        "kind": redact_share_safe_markers(&record.kind),
        "workspace": record.workspace.as_deref().map(redact_share_safe_markers),
        "pr_url": record.pr_url.as_deref().map(redact_share_safe_markers),
        "created_at": record.created_at,
        "updated_at": record.updated_at,
    })
}

fn print_share_safe_value(mut value: serde_json::Value) -> Result<()> {
    redact_json_string_field(&mut value, "query");
    print_json(value)
}

fn redact_json_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            *text = redact_share_safe_markers(text);
        }
        serde_json::Value::Object(object) => {
            for child in object.values_mut() {
                redact_json_strings(child);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                redact_json_strings(child);
            }
        }
        _ => {}
    }
}

fn redact_json_string_field(value: &mut serde_json::Value, field: &str) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::String(text)) = object.get_mut(field) {
                *text = redact_share_safe_markers(text);
            }
            for child in object.values_mut() {
                redact_json_string_field(child, field);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                redact_json_string_field(child, field);
            }
        }
        _ => {}
    }
}

fn print_json(value: serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

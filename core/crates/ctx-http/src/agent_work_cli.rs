use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand, ValueEnum};
use ctx_core::ids::{
    ChangeSetId, ContributionId, SessionId, TaskId, WorkEventId, WorkEvidenceId, WorkRecordId,
    WorkRecordLinkId, WorkSearchDocId, WorkSummaryClaimId, WorkSummaryId, WorkspaceId,
};
use ctx_core::models::PluginManifest;
use ctx_core::models::{
    ChangeSet, Contribution, ContributionEndpoint, ContributionRole, GitFingerprint,
    PullRequestLink, PullRequestLinkKind, PullRequestRef, RecordFidelity, RecordOrigin,
    RecordSource, RecordTrust, Sha256DigestValue, WorkActorKind, WorkEvent, WorkEventType,
    WorkEvidence, WorkEvidenceFreshness, WorkEvidenceKind, WorkEvidenceStatus, WorkLifecycle,
    WorkLinkRole, WorkLinkTargetKind, WorkRecord, WorkRecordLink, WorkRedactionClass,
    WorkSearchDoc, WorkSummary, WorkSummaryAudience, WorkSummaryClaim, WorkSummaryFreshness,
    WorkSummaryGenerationMethod, WorkSummaryKind, WorkTrustVerdict, Workspace,
};
use ctx_store::{Store, StoreManager, WorkSearchQuery};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use url::Url;

#[derive(Debug, Args)]
pub(crate) struct AgentWorkCommand {
    #[command(subcommand)]
    pub(crate) command: AgentWorkSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentWorkSubcommand {
    /// Print or list public ctx Work schemas.
    Schema(AgentWorkSchemaArgs),
    /// Validate a local JSON file against known Work shapes.
    Validate(AgentWorkValidateArgs),
    /// Print a safe metadata summary for a local Work JSON file.
    Inspect(AgentWorkFileArgs),
    /// Show local redaction decisions for a Work JSON fixture.
    RedactionPreview(AgentWorkFileArgs),
    /// List local Work records.
    List(AgentWorkListArgs),
    /// Show a local Work record.
    Show(AgentWorkShowArgs),
    /// Project ADE session/task state into durable Work records.
    Project(AgentWorkProjectArgs),
    /// Capture local Work records.
    Capture(AgentWorkCaptureArgs),
    /// Link a pull request URL to a local Work change set.
    LinkPr(AgentWorkLinkPrArgs),
    /// Add a local note to the Work graph.
    Note(AgentWorkNoteArgs),
    /// Show recent local Work context.
    Recent(AgentWorkRecentArgs),
    /// Search redacted local Work records.
    Search(AgentWorkSearchArgs),
    /// Build a bounded agent context pack for a Work record.
    Context(AgentWorkContextArgs),
    /// Render a reviewer-facing Work report.
    Report(AgentWorkReportArgs),
    /// Show the redacted Work timeline.
    Timeline(AgentWorkTimelineArgs),
    /// Add or inspect Work evidence.
    Evidence(AgentWorkEvidenceArgs),
    /// Generate a deterministic local Work summary.
    Summarize(AgentWorkSummarizeArgs),
    /// Link a commit SHA to a durable Work record.
    LinkCommit(AgentWorkLinkCommitArgs),
    /// Maintain the redacted local Work search index.
    Index(AgentWorkIndexArgs),
    /// Export local Work records.
    Export(AgentWorkExportArgs),
    /// Import local Work records.
    Import(AgentWorkImportArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkSchemaArgs {
    /// Schema to print. Omit to list the known local schemas.
    #[arg(long, value_enum)]
    pub(crate) kind: Option<AgentWorkSchemaKind>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkValidateArgs {
    /// Expected schema kind. If omitted, ctx infers from the JSON shape where possible.
    #[arg(long, value_enum)]
    pub(crate) kind: Option<AgentWorkSchemaKind>,
    /// JSON file to validate.
    pub(crate) file: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkFileArgs {
    /// JSON file to inspect.
    pub(crate) file: PathBuf,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct AgentWorkStoreArgs {
    /// ctx data root. Defaults to CTX_DATA_ROOT, then ~/.ctx.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
    /// Workspace id to read or write. If omitted, ctx uses the only registered workspace.
    #[arg(long)]
    pub(crate) workspace_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkListArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Record class to list.
    #[arg(long, value_enum, default_value = "all")]
    pub(crate) kind: AgentWorkRecordKind,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkShowArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Record class. Omit to infer from the id prefix, then search both stores.
    #[arg(long, value_enum)]
    pub(crate) kind: Option<AgentWorkRecordKind>,
    /// Change set or contribution id.
    pub(crate) id: String,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkProjectArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    #[command(subcommand)]
    pub(crate) command: AgentWorkProjectSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentWorkProjectSubcommand {
    /// Project one ADE session into the Work graph.
    Session(AgentWorkProjectSessionArgs),
    /// Project all ADE sessions for one task into the Work graph.
    Task(AgentWorkProjectTaskArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkProjectSessionArgs {
    /// ADE session id.
    pub(crate) session_id: String,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkProjectTaskArgs {
    /// ADE task id.
    pub(crate) task_id: String,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkExportArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// File to write. Omit to write JSON to stdout.
    #[arg(long)]
    pub(crate) output: Option<PathBuf>,
    /// Redaction policy for exported records.
    #[arg(long, value_enum, default_value = "safe-summary")]
    pub(crate) redaction_profile: AgentWorkRedactionProfile,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkImportArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// AgentWork JSON file produced by `ctx work export` or matching the public schema.
    pub(crate) file: PathBuf,
    /// Validate and report counts without writing records.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkCaptureArgs {
    #[command(subcommand)]
    pub(crate) command: AgentWorkCaptureSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentWorkCaptureSubcommand {
    /// Record an already-forwarded git or gh command invocation.
    Command(AgentWorkCaptureCommandArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkCaptureCommandArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Captured command.
    #[arg(long, value_enum)]
    pub(crate) tool: AgentWorkCaptureTool,
    /// Exit code from the real command.
    #[arg(long, default_value_t = 0)]
    pub(crate) exit_code: i32,
    /// Working directory for the captured command. Defaults to the current directory.
    #[arg(long)]
    pub(crate) cwd: Option<PathBuf>,
    /// Read original command arguments as NUL-delimited values from stdin.
    #[arg(long)]
    pub(crate) argv0_stdin: bool,
    /// Original command arguments after `--`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) argv: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkLinkPrArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Existing change set id to link. If omitted, ctx records a new local change set from cwd.
    #[arg(long)]
    pub(crate) change_set_id: Option<String>,
    /// Pull request URL, for example https://github.com/owner/repo/pull/123.
    pub(crate) url: String,
    /// Optional PR title.
    #[arg(long)]
    pub(crate) title: Option<String>,
    /// Optional PR state.
    #[arg(long)]
    pub(crate) state: Option<String>,
    /// Working directory used when creating a new local change set.
    #[arg(long)]
    pub(crate) cwd: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkNoteArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Attach the note to this change set.
    #[arg(long)]
    pub(crate) change_set_id: Option<String>,
    /// Note text.
    pub(crate) summary: String,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkRecentArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Maximum records per kind.
    #[arg(long, default_value_t = 5)]
    pub(crate) limit: usize,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkSearchArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Search query. Omit when using exact filters such as --pr or --commit.
    pub(crate) query: Vec<String>,
    /// Workspace-relative path filter.
    #[arg(long)]
    pub(crate) path: Option<String>,
    /// Pull request URL filter.
    #[arg(long)]
    pub(crate) pr: Option<String>,
    /// Commit SHA filter.
    #[arg(long)]
    pub(crate) commit: Option<String>,
    /// Maximum search results.
    #[arg(long, default_value_t = 20)]
    pub(crate) limit: usize,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkContextArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Work record id.
    pub(crate) work_id: String,
    /// Approximate token budget for the returned context.
    #[arg(long, default_value_t = 12_000)]
    pub(crate) budget: usize,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkReportArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Work record id.
    pub(crate) work_id: String,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
    /// Emit Markdown.
    #[arg(long)]
    pub(crate) markdown: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkTimelineArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Work record id.
    pub(crate) work_id: String,
    /// Maximum timeline events.
    #[arg(long, default_value_t = 100)]
    pub(crate) limit: usize,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkEvidenceArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Work record id.
    pub(crate) work_id: String,
    #[command(subcommand)]
    pub(crate) command: AgentWorkEvidenceSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentWorkEvidenceSubcommand {
    /// List evidence for a Work record.
    List(AgentWorkEvidenceListArgs),
    /// Attach a local artifact as evidence.
    Add(AgentWorkEvidenceAddArgs),
    /// Run a command and record evidence with a Git fingerprint.
    Run(AgentWorkEvidenceRunArgs),
    /// Recompute evidence freshness against the current workspace state.
    Freshness(AgentWorkEvidenceFreshnessArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkEvidenceListArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkEvidenceAddArgs {
    /// Evidence kind.
    #[arg(long, value_enum, default_value = "log")]
    pub(crate) kind: AgentWorkEvidenceKindArg,
    /// Local file to attach. Must be under the workspace root and not a symlink.
    #[arg(long)]
    pub(crate) file: PathBuf,
    /// Optional claim for this evidence.
    #[arg(long)]
    pub(crate) claim: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkEvidenceRunArgs {
    /// Evidence kind.
    #[arg(long, value_enum, default_value = "command")]
    pub(crate) kind: AgentWorkEvidenceKindArg,
    /// Working directory for the command. Defaults to the current directory.
    #[arg(long)]
    pub(crate) cwd: Option<PathBuf>,
    /// Command and arguments after `--`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkEvidenceFreshnessArgs {
    /// Working directory used for current Git fingerprint.
    #[arg(long)]
    pub(crate) cwd: Option<PathBuf>,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkSummarizeArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Work record id.
    pub(crate) work_id: String,
    /// Summary kind.
    #[arg(long, value_enum, default_value = "context")]
    pub(crate) kind: AgentWorkSummaryKindArg,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkLinkCommitArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    /// Commit SHA to link.
    pub(crate) sha: String,
    /// Working directory used for repo/branch context.
    #[arg(long)]
    pub(crate) cwd: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkIndexArgs {
    #[command(flatten)]
    pub(crate) store: AgentWorkStoreArgs,
    #[command(subcommand)]
    pub(crate) command: AgentWorkIndexSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentWorkIndexSubcommand {
    /// Rebuild redacted search docs from local Work records.
    Rebuild(AgentWorkIndexRebuildArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkIndexRebuildArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AgentWorkEvidenceKindArg {
    Command,
    Test,
    Lint,
    Format,
    Typecheck,
    Build,
    Screenshot,
    Recording,
    Log,
    ManualReview,
    AgentReview,
    CiResult,
    ArtifactInspection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AgentWorkSummaryKindArg {
    Live,
    Context,
    Report,
    DecisionLog,
    Evidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AgentWorkSchemaKind {
    WorkBundle,
    AgentWork,
    ChangeSet,
    Contribution,
    Events,
    ToolCall,
    Transcripts,
    PluginManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AgentWorkRecordKind {
    All,
    ChangeSet,
    Contribution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentWorkCaptureTool {
    Git,
    Gh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentWorkRedactionProfile {
    /// Redact obvious secrets, host paths, and transcript-like payloads.
    #[serde(alias = "safe-summary")]
    SafeSummary,
    /// Preserve full local records. Use only for trusted local imports/exports.
    #[serde(alias = "full-local")]
    FullLocal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentWorkExport {
    change_sets: Vec<ChangeSet>,
    contributions: Vec<Contribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentWorkExportEnvelope {
    kind: String,
    schema_version: i64,
    agent_work_schema_version: i64,
    provenance: AgentWorkExportProvenance,
    redaction: AgentWorkExportRedaction,
    agent_work: AgentWorkExport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentWorkExportProvenance {
    source_kind: String,
    workspace_id: WorkspaceId,
    exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentWorkExportRedaction {
    profile: AgentWorkRedactionProfile,
    import_safe: bool,
    #[serde(default)]
    stats: ctx_core::models::RunArchiveNormalizationStats,
}

const AGENT_WORK_EXPORT_ENVELOPE_KIND: &str = "ctx.agent_work.export";
const AGENT_WORK_EXPORT_ENVELOPE_SCHEMA_VERSION: i64 = 1;
const AGENT_WORK_EXPORT_SOURCE_KIND: &str = "ctx.work.cli";
const AGENT_WORK_SCHEMA_VERSION: i64 = 1;
const COMMAND_OUTPUT_PREVIEW_LIMIT: usize = 4 * 1024;

impl AgentWorkRecordKind {
    fn includes_change_sets(self) -> bool {
        matches!(self, Self::All | Self::ChangeSet)
    }

    fn includes_contributions(self) -> bool {
        matches!(self, Self::All | Self::Contribution)
    }
}

impl AgentWorkCaptureTool {
    fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Gh => "gh",
        }
    }
}

impl AgentWorkRedactionProfile {
    fn as_str(self) -> &'static str {
        match self {
            Self::SafeSummary => "safe_summary",
            Self::FullLocal => "full_local",
        }
    }

    fn default_import_safe(self) -> bool {
        matches!(self, Self::FullLocal)
    }
}

pub(crate) async fn run(command: AgentWorkCommand) -> Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    run_with_writer(command, &mut stdout).await
}

async fn run_with_writer(command: AgentWorkCommand, writer: &mut dyn Write) -> Result<()> {
    match command.command {
        AgentWorkSubcommand::Schema(args) => {
            write_schema(args, writer)?;
        }
        AgentWorkSubcommand::Validate(args) => {
            let value = read_json_file(&args.file).with_context(|| {
                durable_diagnostic(
                    DiagnosticSeverity::Error,
                    "ctx.work.validate.invalid_json",
                    &format!("failed to parse {}", args.file.display()),
                )
            })?;
            let kind = args
                .kind
                .map(Ok)
                .unwrap_or_else(|| infer_schema_kind(&value))
                .with_context(|| {
                    durable_diagnostic(
                        DiagnosticSeverity::Error,
                        "ctx.work.validate.unknown_schema",
                        &format!("failed to identify {}", args.file.display()),
                    )
                })?;
            validate_value(kind, &value).with_context(|| {
                durable_diagnostic(
                    DiagnosticSeverity::Error,
                    "ctx.work.validate.failed",
                    &format!("{} failed structural validation", args.file.display()),
                )
            })?;
            writeln!(
                writer,
                "ok: {} matches {} (strict local validation)",
                args.file.display(),
                kind.as_str()
            )?;
            write_diagnostic(
                writer,
                DiagnosticSeverity::Info,
                "ctx.work.validate.ok",
                &format!("{} passed strict local validation", args.file.display()),
            )?;
        }
        AgentWorkSubcommand::Inspect(args) => {
            let value = read_json_file(&args.file).with_context(|| {
                durable_diagnostic(
                    DiagnosticSeverity::Error,
                    "ctx.work.inspect.invalid_json",
                    &format!("failed to parse {}", args.file.display()),
                )
            })?;
            write_inspection(&args.file, &value, writer)?;
        }
        AgentWorkSubcommand::RedactionPreview(args) => {
            let value = read_json_file(&args.file).with_context(|| {
                durable_diagnostic(
                    DiagnosticSeverity::Error,
                    "ctx.work.redaction_preview.invalid_json",
                    &format!("failed to parse {}", args.file.display()),
                )
            })?;
            write_redaction_preview(&args.file, &value, writer)?;
        }
        AgentWorkSubcommand::List(args) => {
            list_work_records(args, writer).await?;
        }
        AgentWorkSubcommand::Show(args) => {
            show_work_record(args, writer).await?;
        }
        AgentWorkSubcommand::Project(args) => {
            project_work(args, writer).await?;
        }
        AgentWorkSubcommand::Capture(args) => {
            capture_work(args, writer).await?;
        }
        AgentWorkSubcommand::LinkPr(args) => {
            link_pull_request(args, writer).await?;
        }
        AgentWorkSubcommand::Note(args) => {
            add_work_note(args, writer).await?;
        }
        AgentWorkSubcommand::Recent(args) => {
            show_recent_work(args, writer).await?;
        }
        AgentWorkSubcommand::Search(args) => {
            search_work(args, writer).await?;
        }
        AgentWorkSubcommand::Context(args) => {
            show_work_context(args, writer).await?;
        }
        AgentWorkSubcommand::Report(args) => {
            show_work_report(args, writer).await?;
        }
        AgentWorkSubcommand::Timeline(args) => {
            show_work_timeline(args, writer).await?;
        }
        AgentWorkSubcommand::Evidence(args) => {
            handle_work_evidence(args, writer).await?;
        }
        AgentWorkSubcommand::Summarize(args) => {
            summarize_work(args, writer).await?;
        }
        AgentWorkSubcommand::LinkCommit(args) => {
            link_commit(args, writer).await?;
        }
        AgentWorkSubcommand::Index(args) => {
            handle_work_index(args, writer).await?;
        }
        AgentWorkSubcommand::Export(args) => {
            export_work_records(args, writer).await?;
        }
        AgentWorkSubcommand::Import(args) => {
            import_work_records(args, writer).await?;
        }
    }
    Ok(())
}

async fn project_work(args: AgentWorkProjectArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    match args.command {
        AgentWorkProjectSubcommand::Session(project_args) => {
            let session_id = parse_session_id(&project_args.session_id)?;
            let result = context.store.project_session_to_work(session_id).await?;
            write_work_projection_result(
                writer,
                project_args.json,
                "session",
                &project_args.session_id,
                result,
            )
        }
        AgentWorkProjectSubcommand::Task(project_args) => {
            let task_id = parse_task_id(&project_args.task_id)?;
            let result = context.store.project_task_sessions_to_work(task_id).await?;
            write_work_projection_result(
                writer,
                project_args.json,
                "task",
                &project_args.task_id,
                result,
            )
        }
    }
}

fn write_work_projection_result(
    writer: &mut dyn Write,
    json_output: bool,
    source_kind: &str,
    source_id: &str,
    result: ctx_store::store::WorkProjectionResult,
) -> Result<()> {
    if json_output {
        serde_json::to_writer_pretty(
            &mut *writer,
            &json!({
                "source_kind": source_kind,
                "source_id": source_id,
                "work_records": result.work_records,
                "links": result.links,
                "events": result.events,
            }),
        )?;
        writeln!(writer)?;
    } else {
        writeln!(
            writer,
            "projected {source_kind} {source_id}: {} work records, {} links, {} events",
            result.work_records, result.links, result.events
        )?;
    }
    Ok(())
}

fn parse_session_id(value: &str) -> Result<SessionId> {
    uuid::Uuid::parse_str(value)
        .map(SessionId::from)
        .with_context(|| format!("invalid ADE session id `{value}`"))
}

fn parse_task_id(value: &str) -> Result<TaskId> {
    uuid::Uuid::parse_str(value)
        .map(TaskId::from)
        .with_context(|| format!("invalid ADE task id `{value}`"))
}

async fn capture_work(args: AgentWorkCaptureArgs, writer: &mut dyn Write) -> Result<()> {
    match args.command {
        AgentWorkCaptureSubcommand::Command(args) => capture_command(args, writer).await,
    }
}

async fn capture_command(args: AgentWorkCaptureCommandArgs, writer: &mut dyn Write) -> Result<()> {
    let AgentWorkCaptureCommandArgs {
        store,
        tool,
        exit_code,
        cwd,
        argv0_stdin,
        argv,
    } = args;
    let argv = if argv0_stdin {
        read_nul_delimited_argv_from_stdin()?
    } else {
        argv
    };
    if let Some(reason) = captured_command_denial_reason(tool, &argv) {
        bail!("refusing to capture {} command: {reason}", tool.as_str());
    }
    if let Some(reason) = captured_command_allowlist_denial_reason(tool, &argv, exit_code) {
        bail!("refusing to capture {} command: {reason}", tool.as_str());
    }
    let cwd = cwd.unwrap_or(std::env::current_dir()?);
    let context = open_work_store_for_path(&store, Some(&cwd)).await?;
    let facts = git_facts(&cwd);
    let pr = if tool == AgentWorkCaptureTool::Gh {
        find_pull_request_ref(&argv)
    } else {
        None
    };
    let git_command = git_subcommand(&argv);
    let work = if tool == AgentWorkCaptureTool::Git && git_command == Some("commit") {
        if let Some(head_sha) = facts.head_sha.as_deref() {
            ensure_work_record_for_commit(&context, head_sha, &cwd).await?
        } else {
            ensure_ambient_work_record(&context, &cwd).await?
        }
    } else if let Some(pr) = pr.as_ref() {
        ensure_work_record_for_pr(&context, pr, &cwd, pr.title.as_deref()).await?
    } else {
        ensure_ambient_work_record(&context, &cwd).await?
    };
    let classification = classify_captured_command(tool, &argv);
    let metadata = json!({
        "kind": "ctx.work.command_capture",
        "tool": tool.as_str(),
        "argv": redact_argv(&argv),
        "exit_code": exit_code,
        "cwd": cwd.to_string_lossy(),
        "repo_root": facts.repo_root,
        "branch": facts.branch,
        "head_sha": facts.head_sha,
        "classification": classification,
        "pull_request": pr.as_ref(),
    });
    let target = pr
        .clone()
        .map(|pull_request| ContributionEndpoint::PullRequest { pull_request })
        .unwrap_or(ContributionEndpoint::Workspace {
            workspace_id: context.workspace_id,
        });
    let contribution = Contribution {
        id: ContributionId::new(),
        workspace_id: context.workspace_id,
        change_set_id: None,
        subject: ContributionEndpoint::System {
            label: Some(args.tool.as_str().to_string()),
        },
        target,
        role: ContributionRole::Context,
        source: if tool == AgentWorkCaptureTool::Gh {
            RecordSource::PullRequest
        } else {
            RecordSource::External
        },
        origin: RecordOrigin::System,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Low,
        summary: Some(format!(
            "{} {} exited {}",
            tool.as_str(),
            argv.first().map(String::as_str).unwrap_or("(no args)"),
            exit_code
        )),
        fingerprint: None,
        issuer: Some("ctx work capture command".to_string()),
        metadata_json: Some(metadata),
        source_records: Vec::new(),
        created_at: None,
        updated_at: None,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let contribution = context.store.upsert_contribution(&contribution).await?;
    upsert_work_link(
        &context,
        &work.work_id,
        WorkLinkTargetKind::Contribution,
        Some(contribution.id.0.clone()),
        Some(serde_json::to_value(&contribution)?),
        WorkLinkRole::Context,
        contribution.source,
        contribution.fidelity,
        contribution.trust,
    )
    .await?;
    if tool == AgentWorkCaptureTool::Git {
        if let Some(head_sha) = facts.head_sha.as_deref() {
            if matches!(git_command, Some("commit" | "push")) {
                upsert_work_link(
                    &context,
                    &work.work_id,
                    WorkLinkTargetKind::Commit,
                    Some(head_sha.to_string()),
                    Some(json!({
                        "sha": head_sha,
                        "captured_by": "ctx work capture command",
                        "git_command": git_command,
                    })),
                    WorkLinkRole::Result,
                    RecordSource::Worktree,
                    RecordFidelity::Commit,
                    RecordTrust::Low,
                )
                .await?;
            }
        }
    }
    append_capture_work_event(
        &context,
        &work.work_id,
        tool,
        &argv,
        exit_code,
        &cwd,
        pr.as_ref(),
        Some(&contribution.id),
    )
    .await?;
    if let Some(kind) = evidence_kind_for_captured_command(&classification) {
        let now = Utc::now();
        let fingerprint = git_fingerprint(&cwd);
        let evidence = WorkEvidence {
            evidence_id: WorkEvidenceId::new(),
            work_id: work.work_id.clone(),
            workspace_id: context.workspace_id,
            kind,
            status: if exit_code == 0 {
                WorkEvidenceStatus::ObservedPass
            } else {
                WorkEvidenceStatus::ObservedFail
            },
            freshness: if fingerprint.is_some() {
                WorkEvidenceFreshness::Fresh
            } else {
                WorkEvidenceFreshness::Unknown
            },
            claim: contribution.summary.clone(),
            command: Some(redact_argv(&argv).join(" ")),
            argv: redact_argv(&argv),
            cwd: Some(redact_work_text(&context, &cwd.to_string_lossy())),
            exit_code: Some(exit_code),
            repo_root: git_facts(&cwd).repo_root,
            head_sha: git_facts(&cwd).head_sha,
            branch: git_facts(&cwd).branch,
            fingerprint: fingerprint.clone(),
            current_fingerprint: fingerprint,
            output_ref: None,
            artifact_ref: None,
            source: RecordSource::External,
            fidelity: RecordFidelity::Declared,
            trust: RecordTrust::Low,
            started_at: now,
            finished_at: now,
            created_at: now,
            updated_at: now,
            schema_version: AGENT_WORK_SCHEMA_VERSION,
        };
        let evidence = context.store.upsert_work_evidence(&evidence).await?;
        append_evidence_event_and_index(&context, &work.work_id, &evidence).await?;
    }
    writeln!(writer, "captured: {}", contribution.id)?;
    writeln!(writer, "work: {}", work.work_id)?;
    write_diagnostic(
        writer,
        DiagnosticSeverity::Info,
        "ctx.work.capture.command.completed",
        &format!(
            "captured {} command for workspace {}",
            tool.as_str(),
            context.workspace_id.0
        ),
    )?;
    Ok(())
}

async fn link_pull_request(args: AgentWorkLinkPrArgs, writer: &mut dyn Write) -> Result<()> {
    let cwd = args.cwd.unwrap_or(std::env::current_dir()?);
    let context = open_work_store_for_path(&args.store, Some(&cwd)).await?;
    let pr = parse_github_pull_request_url(&args.url)?;
    let mut change_set = match args.change_set_id {
        Some(id) => context
            .store
            .get_workspace_change_set(context.workspace_id, ChangeSetId::from_id(id.clone()))
            .await?
            .with_context(|| format!("change set {id} not found"))?,
        None => {
            if let Some(change_set) =
                find_change_set_for_pull_request(&context.store, context.workspace_id, &pr).await?
            {
                change_set
            } else {
                build_change_set_from_cwd(context.workspace_id, &cwd, Some(&pr)).await
            }
        }
    };
    let link = PullRequestLink {
        kind: PullRequestLinkKind::Result,
        pull_request: pr.clone(),
        url: Some(args.url.clone()),
        title: args.title.clone(),
        state: args.state.clone(),
    };
    upsert_pull_request_link(&mut change_set, link);
    let change_set = context.store.upsert_change_set(&change_set).await?;
    let contribution =
        upsert_pr_link_contribution(&context.store, context.workspace_id, &change_set, &pr).await?;
    let work = ensure_work_record_for_pr(&context, &pr, &cwd, args.title.as_deref()).await?;
    upsert_work_link(
        &context,
        &work.work_id,
        WorkLinkTargetKind::ChangeSet,
        Some(change_set.id.0.clone()),
        Some(serde_json::to_value(&change_set)?),
        WorkLinkRole::Result,
        change_set.source,
        change_set.fidelity,
        change_set.trust,
    )
    .await?;
    upsert_work_link(
        &context,
        &work.work_id,
        WorkLinkTargetKind::Contribution,
        Some(contribution.id.0.clone()),
        Some(serde_json::to_value(&contribution)?),
        WorkLinkRole::Result,
        contribution.source,
        contribution.fidelity,
        contribution.trust,
    )
    .await?;
    let now = Utc::now();
    let event = WorkEvent {
        event_id: WorkEventId::new(),
        work_id: work.work_id.clone(),
        workspace_id: context.workspace_id,
        sequence: 0,
        source_kind: Some("pull_request".to_string()),
        source_id: Some(pull_request_target_id(&pr)),
        event_type: WorkEventType::PullRequestLinked,
        event_time: now,
        actor_kind: WorkActorKind::System,
        provider: Some("github".to_string()),
        harness: None,
        model: None,
        redaction_class: WorkRedactionClass::LocalRedacted,
        source: RecordSource::PullRequest,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Medium,
        payload_json: Some(ctx_core::redaction::redact_json_value(json!({
            "pull_request": &pr,
            "change_set_id": &change_set.id,
            "contribution_id": &contribution.id,
        }))),
        redacted_text: Some(format!("Linked PR {}/{}#{}", pr.owner, pr.repo, pr.number)),
        artifact_ref: None,
        created_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let event = context.store.append_work_event(&event).await?;
    index_work_event(&context, &event).await?;

    writeln!(writer, "change_set: {}", change_set.id)?;
    writeln!(writer, "work: {}", work.work_id)?;
    writeln!(
        writer,
        "pull_request: {}/{}/#{}",
        pr.owner, pr.repo, pr.number
    )?;
    writeln!(writer, "contribution: {}", contribution.id)?;
    write_diagnostic(
        writer,
        DiagnosticSeverity::Info,
        "ctx.work.link_pr.completed",
        &format!("linked PR {} to change set {}", args.url, change_set.id),
    )?;
    Ok(())
}

async fn add_work_note(args: AgentWorkNoteArgs, writer: &mut dyn Write) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let context = open_work_store_for_path(&args.store, Some(&cwd)).await?;
    let target = if let Some(change_set_id) = args.change_set_id {
        ContributionEndpoint::ChangeSet {
            change_set_id: ChangeSetId::from_id(change_set_id),
        }
    } else {
        ContributionEndpoint::Workspace {
            workspace_id: context.workspace_id,
        }
    };
    let contribution = Contribution {
        id: ContributionId::new(),
        workspace_id: context.workspace_id,
        change_set_id: match &target {
            ContributionEndpoint::ChangeSet { change_set_id } => Some(change_set_id.clone()),
            _ => None,
        },
        subject: ContributionEndpoint::Agent {
            session_id: None,
            run_id: None,
            label: Some("agent".to_string()),
        },
        target,
        role: ContributionRole::Context,
        source: RecordSource::Manual,
        origin: RecordOrigin::Agent,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Medium,
        summary: Some(ctx_core::redaction::redact_sensitive(&args.summary)),
        fingerprint: None,
        issuer: Some("ctx work note".to_string()),
        metadata_json: Some(json!({
            "kind": "ctx.work.note",
        })),
        source_records: Vec::new(),
        created_at: None,
        updated_at: None,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let contribution = context.store.upsert_contribution(&contribution).await?;
    let work = match &contribution.target {
        ContributionEndpoint::ChangeSet { change_set_id } => {
            if let Some(existing) = context
                .store
                .find_work_record_by_link(
                    context.workspace_id,
                    WorkLinkTargetKind::ChangeSet,
                    &change_set_id.0,
                )
                .await?
            {
                existing
            } else {
                ensure_ambient_work_record(&context, &cwd).await?
            }
        }
        _ => ensure_ambient_work_record(&context, &cwd).await?,
    };
    upsert_work_link(
        &context,
        &work.work_id,
        WorkLinkTargetKind::Contribution,
        Some(contribution.id.0.clone()),
        Some(serde_json::to_value(&contribution)?),
        WorkLinkRole::Context,
        contribution.source,
        contribution.fidelity,
        contribution.trust,
    )
    .await?;
    let now = Utc::now();
    let event = WorkEvent {
        event_id: WorkEventId::new(),
        work_id: work.work_id.clone(),
        workspace_id: context.workspace_id,
        sequence: 0,
        source_kind: Some("note".to_string()),
        source_id: Some(contribution.id.0.clone()),
        event_type: WorkEventType::Note,
        event_time: now,
        actor_kind: WorkActorKind::Agent,
        provider: None,
        harness: None,
        model: None,
        redaction_class: WorkRedactionClass::LocalRedacted,
        source: RecordSource::Manual,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Medium,
        payload_json: Some(ctx_core::redaction::redact_json_value(json!({
            "contribution_id": &contribution.id,
        }))),
        redacted_text: contribution.summary.clone(),
        artifact_ref: None,
        created_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let event = context.store.append_work_event(&event).await?;
    index_work_event(&context, &event).await?;
    writeln!(writer, "contribution: {}", contribution.id)?;
    writeln!(writer, "work: {}", work.work_id)?;
    write_diagnostic(
        writer,
        DiagnosticSeverity::Info,
        "ctx.work.note.completed",
        &format!("added Work note {}", contribution.id),
    )?;
    Ok(())
}

async fn show_recent_work(args: AgentWorkRecentArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    let bundle = load_work_export(&context.store, context.workspace_id).await?;
    let change_sets = bundle
        .change_sets
        .into_iter()
        .rev()
        .take(args.limit)
        .collect::<Vec<_>>();
    let contributions = bundle
        .contributions
        .into_iter()
        .rev()
        .take(args.limit)
        .collect::<Vec<_>>();
    if args.json {
        let change_sets = change_sets
            .iter()
            .map(|item| redact_work_serializable(&context, item))
            .collect::<Vec<_>>();
        let contributions = contributions
            .iter()
            .map(|item| redact_work_serializable(&context, item))
            .collect::<Vec<_>>();
        serde_json::to_writer_pretty(
            &mut *writer,
            &json!({
                "workspace_id": context.workspace_id,
                "change_sets": change_sets,
                "contributions": contributions,
            }),
        )?;
        writeln!(writer)?;
    } else {
        writeln!(writer, "workspace: {}", context.workspace_id.0)?;
        writeln!(writer, "recent_change_sets: {}", change_sets.len())?;
        for change_set in &change_sets {
            writeln!(
                writer,
                "- {}{}",
                change_set.id,
                optional_title_suffix(change_set.title.as_deref())
            )?;
        }
        writeln!(writer, "recent_contributions: {}", contributions.len())?;
        for contribution in &contributions {
            writeln!(
                writer,
                "- {}{}",
                contribution.id,
                optional_title_suffix(contribution.summary.as_deref())
            )?;
        }
    }
    Ok(())
}

async fn search_work(args: AgentWorkSearchArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    let pr = args
        .pr
        .as_deref()
        .map(parse_github_pull_request_url)
        .transpose()?;
    let query_text = (!args.query.is_empty()).then(|| args.query.join(" "));
    let hits = context
        .store
        .search_work_docs(
            context.workspace_id,
            WorkSearchQuery {
                text: query_text.clone(),
                path: args.path.clone(),
                pr_owner: pr.as_ref().map(|pr| pr.owner.clone()),
                pr_repo: pr.as_ref().map(|pr| pr.repo.clone()),
                pr_number: pr.as_ref().map(|pr| pr.number),
                commit_sha: args.commit.clone(),
                freshness: None,
                limit: Some(args.limit),
            },
        )
        .await?;

    let mut results = Vec::new();
    for hit in hits {
        let work = context
            .store
            .get_workspace_work_record(context.workspace_id, hit.doc.work_id.clone())
            .await?;
        let linked_prs = linked_pr_urls_for_work(
            &context.store,
            context.workspace_id,
            hit.doc.work_id.clone(),
        )
        .await?;
        let title = hit
            .doc
            .title
            .or_else(|| work.as_ref().and_then(|work| work.title.clone()))
            .map(|title| redact_work_text(&context, &title));
        let trust_verdict = if let Some(work) = work.as_ref() {
            let evidence = context
                .store
                .list_work_evidence(context.workspace_id, work.work_id.clone())
                .await?;
            Some(computed_work_trust_verdict(work, &evidence))
        } else {
            None
        };
        results.push(json!({
            "work_id": hit.doc.work_id,
            "title": title,
            "score": hit.score,
            "matched_fields": [hit.doc.doc_type],
            "workspace_id": hit.doc.workspace_id,
            "repo_root_redacted": hit.doc.repo_root.as_deref().map(|root| redact_work_text(&context, root)),
            "state": work.as_ref().map(|work| work.lifecycle),
            "trust_verdict": trust_verdict,
            "summary_freshness": work.as_ref().map(|work| work.summary_freshness),
            "linked_prs": linked_prs,
            "citations": [{
                "source_kind": hit.doc.source_kind,
                "source_id": hit.doc.source_id,
                "freshness": hit.doc.freshness
            }],
        }));
    }

    if args.json {
        serde_json::to_writer_pretty(
            &mut *writer,
            &json!({
                "query": query_text,
                "results": results,
                "suggested_next_commands": if results.is_empty() {
                    vec![
                        "ctx work index rebuild --json",
                        "ctx work link-pr <url>",
                        "ctx work evidence <work-id> run -- <command>",
                    ]
                } else {
                    Vec::<&str>::new()
                },
            }),
        )?;
        writeln!(writer)?;
    } else if results.is_empty() {
        writeln!(writer, "no matching Work records")?;
        writeln!(writer, "next: ctx work index rebuild --json")?;
    } else {
        for result in results {
            writeln!(
                writer,
                "{} - {}",
                result
                    .get("work_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                result
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Untitled Work")
            )?;
        }
    }
    Ok(())
}

async fn show_work_context(args: AgentWorkContextArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    let value = build_work_context_value(
        &context,
        WorkRecordId::from_id(args.work_id.clone()),
        args.budget,
    )
    .await?;
    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &value)?;
        writeln!(writer)?;
    } else {
        writeln!(writer, "work: {}", args.work_id)?;
        if let Some(objective) = value.pointer("/context/objective").and_then(Value::as_str) {
            writeln!(writer, "objective: {objective}")?;
        }
        if let Some(result) = value
            .pointer("/context/current_result")
            .and_then(Value::as_str)
        {
            writeln!(writer, "current_result: {result}")?;
        }
    }
    Ok(())
}

async fn show_work_report(args: AgentWorkReportArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    let value =
        build_work_report_value(&context, WorkRecordId::from_id(args.work_id.clone())).await?;
    if args.markdown {
        write_work_report_markdown(&value, writer)?;
    } else if args.json {
        serde_json::to_writer_pretty(&mut *writer, &value)?;
        writeln!(writer)?;
    } else {
        let work = value.get("work").and_then(Value::as_object);
        writeln!(
            writer,
            "work: {}",
            work.and_then(|work| work.get("work_id"))
                .and_then(Value::as_str)
                .unwrap_or(args.work_id.as_str())
        )?;
        writeln!(
            writer,
            "title: {}",
            work.and_then(|work| work.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("Untitled Work")
        )?;
        writeln!(
            writer,
            "trust: {}",
            value
                .pointer("/trust/verdict")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        )?;
        writeln!(
            writer,
            "next: {}",
            value
                .pointer("/trust/recommended_next_action")
                .and_then(Value::as_str)
                .unwrap_or("Review the linked evidence.")
        )?;
    }
    Ok(())
}

async fn show_work_timeline(args: AgentWorkTimelineArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    let events = context
        .store
        .list_work_events(
            context.workspace_id,
            WorkRecordId::from_id(args.work_id.clone()),
            Some(args.limit),
        )
        .await?;
    if args.json {
        let events = events
            .iter()
            .map(|event| redact_work_timeline_event(&context, event))
            .collect::<Vec<_>>();
        serde_json::to_writer_pretty(
            &mut *writer,
            &json!({
                "work_id": args.work_id,
                "events": events,
                "raw_transcript_included": false,
            }),
        )?;
        writeln!(writer)?;
    } else {
        for event in events {
            writeln!(
                writer,
                "{} {} {}",
                event.sequence,
                serde_json::to_string(&event.event_type)?.trim_matches('"'),
                event.redacted_text.unwrap_or_default()
            )?;
        }
    }
    Ok(())
}

async fn handle_work_evidence(args: AgentWorkEvidenceArgs, writer: &mut dyn Write) -> Result<()> {
    match args.command {
        AgentWorkEvidenceSubcommand::List(list_args) => {
            list_work_evidence(args.store, args.work_id, list_args, writer).await
        }
        AgentWorkEvidenceSubcommand::Add(add_args) => {
            add_work_evidence(args.store, args.work_id, add_args, writer).await
        }
        AgentWorkEvidenceSubcommand::Run(run_args) => {
            run_work_evidence(args.store, args.work_id, run_args, writer).await
        }
        AgentWorkEvidenceSubcommand::Freshness(freshness_args) => {
            refresh_work_evidence(args.store, args.work_id, freshness_args, writer).await
        }
    }
}

async fn list_work_evidence(
    store_args: AgentWorkStoreArgs,
    work_id: String,
    args: AgentWorkEvidenceListArgs,
    writer: &mut dyn Write,
) -> Result<()> {
    let context = open_work_store(&store_args).await?;
    let evidence = context
        .store
        .list_work_evidence(context.workspace_id, WorkRecordId::from_id(work_id.clone()))
        .await?;
    if args.json {
        let evidence = evidence
            .iter()
            .map(|item| redact_work_serializable(&context, item))
            .collect::<Vec<_>>();
        serde_json::to_writer_pretty(
            &mut *writer,
            &json!({
                "work_id": work_id,
                "evidence": evidence,
            }),
        )?;
        writeln!(writer)?;
    } else {
        writeln!(writer, "evidence: {}", evidence.len())?;
        for item in evidence {
            writeln!(
                writer,
                "- {} {} {}",
                item.evidence_id,
                serde_json::to_string(&item.status)?.trim_matches('"'),
                item.claim.unwrap_or_default()
            )?;
        }
    }
    Ok(())
}

async fn add_work_evidence(
    store_args: AgentWorkStoreArgs,
    work_id: String,
    args: AgentWorkEvidenceAddArgs,
    writer: &mut dyn Write,
) -> Result<()> {
    let context = open_work_store(&store_args).await?;
    let work_id = WorkRecordId::from_id(work_id);
    let artifact = inspect_evidence_file(&context, &args.file)?;
    let now = Utc::now();
    let evidence = WorkEvidence {
        evidence_id: WorkEvidenceId::new(),
        work_id: work_id.clone(),
        workspace_id: context.workspace_id,
        kind: args.kind.into_work_kind(),
        status: WorkEvidenceStatus::ObservedPass,
        freshness: WorkEvidenceFreshness::Fresh,
        claim: args.claim.map(|claim| redact_work_text(&context, &claim)),
        command: None,
        argv: Vec::new(),
        cwd: None,
        exit_code: None,
        repo_root: None,
        head_sha: None,
        branch: None,
        fingerprint: None,
        current_fingerprint: None,
        output_ref: None,
        artifact_ref: Some(artifact),
        source: RecordSource::Manual,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Medium,
        started_at: now,
        finished_at: now,
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let evidence = context.store.upsert_work_evidence(&evidence).await?;
    append_evidence_event_and_index(&context, &work_id, &evidence).await?;
    writeln!(writer, "evidence: {}", evidence.evidence_id)?;
    Ok(())
}

async fn run_work_evidence(
    store_args: AgentWorkStoreArgs,
    work_id: String,
    args: AgentWorkEvidenceRunArgs,
    writer: &mut dyn Write,
) -> Result<()> {
    let cwd = args.cwd.unwrap_or(std::env::current_dir()?);
    let context = open_work_store_for_path(&store_args, Some(&cwd)).await?;
    let work_id = WorkRecordId::from_id(work_id);
    context
        .store
        .get_workspace_work_record(context.workspace_id, work_id.clone())
        .await?
        .with_context(|| format!("work record {} not found", work_id.0))?;
    let Some((program, argv)) = args.command.split_first() else {
        bail!("evidence run requires a command after --");
    };
    let started_at = Utc::now();
    let output = Command::new(program)
        .args(argv)
        .current_dir(&cwd)
        .output()
        .with_context(|| format!("running evidence command `{program}`"))?;
    let finished_at = Utc::now();
    let exit_code = output.status.code().unwrap_or(-1);
    let fingerprint = git_fingerprint(&cwd);
    let facts = git_facts(&cwd);
    let status = if output.status.success() {
        WorkEvidenceStatus::ObservedPass
    } else {
        WorkEvidenceStatus::ObservedFail
    };
    let freshness = if fingerprint.is_some() {
        WorkEvidenceFreshness::Fresh
    } else {
        WorkEvidenceFreshness::Unknown
    };
    let mut full_argv = vec![program.clone()];
    full_argv.extend(argv.iter().cloned());
    let redacted_argv = redact_argv(&full_argv);
    let output_ref = command_output_preview_ref(&context, &output.stdout, &output.stderr);
    let now = Utc::now();
    let evidence = WorkEvidence {
        evidence_id: WorkEvidenceId::new(),
        work_id: work_id.clone(),
        workspace_id: context.workspace_id,
        kind: args.kind.into_work_kind(),
        status,
        freshness,
        claim: Some(format!(
            "Observed `{}` exited {}",
            redacted_argv.join(" "),
            exit_code
        )),
        command: Some(redacted_argv.join(" ")),
        argv: redacted_argv,
        cwd: Some(redact_work_text(&context, &cwd.to_string_lossy())),
        exit_code: Some(exit_code),
        repo_root: facts.repo_root,
        head_sha: facts.head_sha,
        branch: facts.branch,
        fingerprint: fingerprint.clone(),
        current_fingerprint: fingerprint,
        output_ref: Some(output_ref),
        artifact_ref: None,
        source: RecordSource::Worktree,
        fidelity: RecordFidelity::Exact,
        trust: RecordTrust::Medium,
        started_at,
        finished_at,
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let evidence = context.store.upsert_work_evidence(&evidence).await?;
    let evidence_set = context
        .store
        .list_work_evidence(context.workspace_id, work_id.clone())
        .await?;
    refresh_work_trust_from_evidence_set(
        &context.store,
        context.workspace_id,
        &work_id,
        &evidence_set,
    )
    .await?;
    append_evidence_event_and_index(&context, &work_id, &evidence).await?;
    writeln!(writer, "evidence: {}", evidence.evidence_id)?;
    writeln!(
        writer,
        "status: {}",
        serde_json::to_string(&evidence.status)?.trim_matches('"')
    )?;
    Ok(())
}

async fn refresh_work_evidence(
    store_args: AgentWorkStoreArgs,
    work_id: String,
    args: AgentWorkEvidenceFreshnessArgs,
    writer: &mut dyn Write,
) -> Result<()> {
    let cwd = args.cwd.unwrap_or(std::env::current_dir()?);
    let context = open_work_store_for_path(&store_args, Some(&cwd)).await?;
    let work_id = WorkRecordId::from_id(work_id);
    let current = git_fingerprint(&cwd);
    let mut evidence = context
        .store
        .list_work_evidence(context.workspace_id, work_id.clone())
        .await?;
    for item in &mut evidence {
        item.current_fingerprint = current.clone();
        item.freshness = evidence_freshness(item.fingerprint.as_ref(), current.as_ref());
        item.updated_at = Utc::now();
        context.store.upsert_work_evidence(item).await?;
        index_work_evidence(&context, item).await?;
    }
    refresh_work_trust_from_evidence_set(&context.store, context.workspace_id, &work_id, &evidence)
        .await?;
    if args.json {
        let evidence = evidence
            .iter()
            .map(|item| redact_work_serializable(&context, item))
            .collect::<Vec<_>>();
        serde_json::to_writer_pretty(
            &mut *writer,
            &json!({
                "work_id": work_id,
                "evidence": evidence,
            }),
        )?;
        writeln!(writer)?;
    } else {
        writeln!(writer, "refreshed_evidence: {}", evidence.len())?;
    }
    Ok(())
}

async fn summarize_work(args: AgentWorkSummarizeArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    let work_id = WorkRecordId::from_id(args.work_id.clone());
    let report = build_work_report_value(&context, work_id.clone()).await?;
    let text = deterministic_summary_text(&report);
    let now = Utc::now();
    let summary = WorkSummary {
        summary_id: WorkSummaryId::new(),
        work_id: work_id.clone(),
        workspace_id: context.workspace_id,
        kind: args.kind.into_work_kind(),
        audience: match args.kind {
            AgentWorkSummaryKindArg::Context => WorkSummaryAudience::Agent,
            AgentWorkSummaryKindArg::Report => WorkSummaryAudience::Reviewer,
            _ => WorkSummaryAudience::Human,
        },
        text,
        structured_json: Some(json!({
            "source": "deterministic_local",
            "raw_transcript_included": false,
        })),
        generation_method: WorkSummaryGenerationMethod::Deterministic,
        provider: None,
        model: None,
        template: Some("ctx.work.deterministic.v1".to_string()),
        source_material_left_machine: false,
        freshness: WorkSummaryFreshness::Fresh,
        source_revision_key: Some(report_revision_key(&report)),
        generated_at: now,
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let summary = context.store.upsert_work_summary(&summary).await?;
    let claim = WorkSummaryClaim {
        claim_id: WorkSummaryClaimId::new(),
        summary_id: summary.summary_id.clone(),
        work_id: work_id.clone(),
        workspace_id: context.workspace_id,
        claim_text: summary
            .text
            .lines()
            .next()
            .unwrap_or("Work summary generated")
            .to_string(),
        claim_kind: Some("summary".to_string()),
        source_kind: "work_report".to_string(),
        source_id: work_id.0.clone(),
        record_hash: Some(report_revision_key(&report)),
        freshness: WorkSummaryFreshness::Fresh,
        redaction_class: WorkRedactionClass::LocalRedacted,
        created_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    context.store.upsert_work_summary_claim(&claim).await?;
    index_work_summary(&context, &summary).await?;
    if let Some(mut work) = context
        .store
        .get_workspace_work_record(context.workspace_id, work_id.clone())
        .await?
    {
        work.summary_freshness = WorkSummaryFreshness::Fresh;
        work.updated_at = Utc::now();
        context.store.upsert_work_record(&work).await?;
    }
    if args.json {
        serde_json::to_writer_pretty(
            &mut *writer,
            &json!({
                "work_id": work_id,
                "summary": summary,
                "claims": [claim],
            }),
        )?;
        writeln!(writer)?;
    } else {
        writeln!(writer, "summary: {}", summary.summary_id)?;
    }
    Ok(())
}

async fn link_commit(args: AgentWorkLinkCommitArgs, writer: &mut dyn Write) -> Result<()> {
    let cwd = args.cwd.unwrap_or(std::env::current_dir()?);
    let context = open_work_store_for_path(&args.store, Some(&cwd)).await?;
    let work = ensure_work_record_for_commit(&context, &args.sha, &cwd).await?;
    let now = Utc::now();
    let event = WorkEvent {
        event_id: WorkEventId::new(),
        work_id: work.work_id.clone(),
        workspace_id: context.workspace_id,
        sequence: 0,
        source_kind: Some("commit".to_string()),
        source_id: Some(args.sha.clone()),
        event_type: WorkEventType::CommitLinked,
        event_time: now,
        actor_kind: WorkActorKind::System,
        provider: Some("git".to_string()),
        harness: None,
        model: None,
        redaction_class: WorkRedactionClass::LocalRedacted,
        source: RecordSource::Worktree,
        fidelity: RecordFidelity::Commit,
        trust: RecordTrust::Medium,
        payload_json: Some(json!({ "commit": args.sha })),
        redacted_text: Some(format!("Linked commit {}", args.sha)),
        artifact_ref: None,
        created_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let event = context.store.append_work_event(&event).await?;
    index_work_event(&context, &event).await?;
    writeln!(writer, "work: {}", work.work_id)?;
    writeln!(writer, "commit: {}", args.sha)?;
    Ok(())
}

async fn handle_work_index(args: AgentWorkIndexArgs, writer: &mut dyn Write) -> Result<()> {
    match args.command {
        AgentWorkIndexSubcommand::Rebuild(rebuild) => {
            let context = open_work_store(&args.store).await?;
            let deleted = context
                .store
                .delete_workspace_work_search_docs(context.workspace_id)
                .await?;
            let mut inserted = 0usize;
            for work in context
                .store
                .list_workspace_work_records(context.workspace_id, Some(5_000))
                .await?
            {
                index_work_record(&context, &work).await?;
                inserted += 1;
                for link in context
                    .store
                    .list_work_record_links(context.workspace_id, work.work_id.clone())
                    .await?
                {
                    if let Some(pr) = pull_request_ref_from_work_link(&link) {
                        index_work_pull_request_link(&context, &work.work_id, &pr).await?;
                        inserted += 1;
                    }
                }
                for event in context
                    .store
                    .list_work_events(context.workspace_id, work.work_id.clone(), Some(5_000))
                    .await?
                {
                    index_work_event(&context, &event).await?;
                    inserted += 1;
                }
                for evidence in context
                    .store
                    .list_work_evidence(context.workspace_id, work.work_id.clone())
                    .await?
                {
                    index_work_evidence(&context, &evidence).await?;
                    inserted += 1;
                }
                for summary in context
                    .store
                    .list_work_summaries(context.workspace_id, work.work_id.clone())
                    .await?
                {
                    index_work_summary(&context, &summary).await?;
                    inserted += 1;
                }
            }
            if rebuild.json {
                serde_json::to_writer_pretty(
                    &mut *writer,
                    &json!({
                        "deleted": deleted,
                        "inserted": inserted,
                    }),
                )?;
                writeln!(writer)?;
            } else {
                writeln!(writer, "rebuilt Work search index: {inserted} docs")?;
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Default)]
struct GitFacts {
    repo_root: Option<String>,
    branch: Option<String>,
    head_sha: Option<String>,
}

async fn build_change_set_from_cwd(
    workspace_id: WorkspaceId,
    cwd: &Path,
    pr: Option<&PullRequestRef>,
) -> ChangeSet {
    let facts = git_facts(cwd);
    let fingerprint = git_fingerprint(cwd);
    let title = pr
        .map(|pr| format!("GitHub PR {}/{}#{}", pr.owner, pr.repo, pr.number))
        .or_else(|| {
            facts
                .branch
                .as_ref()
                .map(|branch| format!("Local changes on {branch}"))
        })
        .unwrap_or_else(|| "Local Work change set".to_string());
    let mut change_set = ChangeSet {
        id: ChangeSetId::new(),
        workspace_id,
        source_worktree_id: None,
        source: RecordSource::Worktree,
        origin: RecordOrigin::System,
        fidelity: if fingerprint.is_some() {
            RecordFidelity::Diff
        } else {
            RecordFidelity::Declared
        },
        trust: RecordTrust::Low,
        title: Some(title),
        summary: Some("Created by local ctx Work CLI linking.".to_string()),
        description: None,
        fingerprint,
        base_revision: None,
        head_revision: facts.head_sha,
        target_branch: facts.branch,
        pull_requests: Vec::new(),
        source_records: Vec::new(),
        issuer: Some("ctx work link-pr".to_string()),
        created_at: None,
        updated_at: None,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    if let Some(pr) = pr {
        upsert_pull_request_link(
            &mut change_set,
            PullRequestLink {
                kind: PullRequestLinkKind::Result,
                pull_request: pr.clone(),
                url: pr.url.clone(),
                title: pr.title.clone(),
                state: None,
            },
        );
    }
    change_set
}

fn git_facts(cwd: &Path) -> GitFacts {
    GitFacts {
        repo_root: git_output(cwd, &["rev-parse", "--show-toplevel"]),
        branch: git_output(cwd, &["branch", "--show-current"]),
        head_sha: git_output(cwd, &["rev-parse", "HEAD"]),
    }
}

fn git_fingerprint(cwd: &Path) -> Option<GitFingerprint> {
    let facts = git_facts(cwd);
    facts.repo_root.as_ref()?;
    let patch = git_output_lossy(cwd, &["diff", "--binary"]).unwrap_or_default();
    let status =
        git_output_lossy(cwd, &["status", "--porcelain=v1", "--branch"]).unwrap_or_default();
    let untracked =
        git_output_lossy(cwd, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
    let mut changed_paths = Vec::new();
    for args in [
        ["diff", "--name-only"].as_slice(),
        ["diff", "--cached", "--name-only"].as_slice(),
    ] {
        if let Some(output) = git_output_lossy(cwd, args) {
            changed_paths.extend(output.lines().map(ToOwned::to_owned));
        }
    }
    changed_paths.extend(untracked.lines().map(ToOwned::to_owned));
    changed_paths.sort();
    changed_paths.dedup();
    let changed_paths = changed_paths.join("\n");
    let dirty = status.lines().any(|line| !line.starts_with("##"));

    Some(GitFingerprint {
        repo_root: facts.repo_root,
        head_sha: facts.head_sha,
        branch: facts.branch,
        patch_sha256: Sha256DigestValue::from_bytes(patch.as_bytes()),
        status_sha256: Sha256DigestValue::from_bytes(status.as_bytes()),
        untracked_sha256: Sha256DigestValue::from_bytes(untracked.as_bytes()),
        changed_paths_sha256: Sha256DigestValue::from_bytes(changed_paths.as_bytes()),
        dirty,
    })
}

impl AgentWorkEvidenceKindArg {
    fn into_work_kind(self) -> WorkEvidenceKind {
        match self {
            Self::Command => WorkEvidenceKind::Command,
            Self::Test => WorkEvidenceKind::Test,
            Self::Lint => WorkEvidenceKind::Lint,
            Self::Format => WorkEvidenceKind::Format,
            Self::Typecheck => WorkEvidenceKind::Typecheck,
            Self::Build => WorkEvidenceKind::Build,
            Self::Screenshot => WorkEvidenceKind::Screenshot,
            Self::Recording => WorkEvidenceKind::Recording,
            Self::Log => WorkEvidenceKind::Log,
            Self::ManualReview => WorkEvidenceKind::ManualReview,
            Self::AgentReview => WorkEvidenceKind::AgentReview,
            Self::CiResult => WorkEvidenceKind::CiResult,
            Self::ArtifactInspection => WorkEvidenceKind::ArtifactInspection,
        }
    }
}

impl AgentWorkSummaryKindArg {
    fn into_work_kind(self) -> WorkSummaryKind {
        match self {
            Self::Live => WorkSummaryKind::LiveSummary,
            Self::Context => WorkSummaryKind::ContextSummary,
            Self::Report => WorkSummaryKind::ReportSummary,
            Self::DecisionLog => WorkSummaryKind::DecisionLog,
            Self::Evidence => WorkSummaryKind::EvidenceSummary,
        }
    }
}

async fn ensure_work_record_for_pr(
    context: &WorkStoreContext,
    pr: &PullRequestRef,
    cwd: &Path,
    title: Option<&str>,
) -> Result<WorkRecord> {
    let target_id = pull_request_target_id(pr);
    if let Some(existing) = context
        .store
        .find_work_record_by_link(
            context.workspace_id,
            WorkLinkTargetKind::PullRequest,
            &target_id,
        )
        .await?
    {
        return Ok(existing);
    }

    let facts = git_facts(cwd);
    let now = Utc::now();
    let record = WorkRecord {
        work_id: WorkRecordId::new(),
        workspace_id: context.workspace_id,
        title: Some(
            title
                .map(ToOwned::to_owned)
                .or_else(|| pr.title.clone())
                .unwrap_or_else(|| format!("PR {}/{}#{}", pr.owner, pr.repo, pr.number)),
        ),
        objective: None,
        lifecycle: WorkLifecycle::Active,
        primary_repo_root: facts.repo_root.clone(),
        primary_branch: facts.branch.clone(),
        base_commit: None,
        head_commit: facts.head_sha.clone(),
        current_diff_fingerprint: git_fingerprint(cwd),
        trust_verdict: WorkTrustVerdict::UntrustedLocalCapture,
        summary_freshness: WorkSummaryFreshness::Missing,
        metadata_json: Some(json!({
            "created_by": "ctx work",
            "grouping": "pull_request",
        })),
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let record = context.store.upsert_work_record(&record).await?;
    upsert_work_link(
        context,
        &record.work_id,
        WorkLinkTargetKind::PullRequest,
        Some(target_id),
        Some(serde_json::to_value(pr)?),
        WorkLinkRole::Result,
        RecordSource::PullRequest,
        RecordFidelity::Declared,
        RecordTrust::Medium,
    )
    .await?;
    index_work_record(context, &record).await?;
    index_work_pull_request_link(context, &record.work_id, pr).await?;
    Ok(record)
}

async fn ensure_work_record_for_commit(
    context: &WorkStoreContext,
    sha: &str,
    cwd: &Path,
) -> Result<WorkRecord> {
    let sha = sha.trim();
    if sha.is_empty() {
        bail!("commit SHA must not be empty");
    }
    if let Some(existing) = context
        .store
        .find_work_record_by_link(context.workspace_id, WorkLinkTargetKind::Commit, sha)
        .await?
    {
        return Ok(existing);
    }

    let facts = git_facts(cwd);
    let now = Utc::now();
    let record = WorkRecord {
        work_id: WorkRecordId::new(),
        workspace_id: context.workspace_id,
        title: Some(format!("Commit {sha}")),
        objective: None,
        lifecycle: WorkLifecycle::Active,
        primary_repo_root: facts.repo_root.clone(),
        primary_branch: facts.branch.clone(),
        base_commit: None,
        head_commit: Some(sha.to_string()),
        current_diff_fingerprint: git_fingerprint(cwd),
        trust_verdict: WorkTrustVerdict::Partial,
        summary_freshness: WorkSummaryFreshness::Missing,
        metadata_json: Some(json!({
            "created_by": "ctx work link-commit",
            "grouping": "commit",
        })),
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let record = context.store.upsert_work_record(&record).await?;
    upsert_work_link(
        context,
        &record.work_id,
        WorkLinkTargetKind::Commit,
        Some(sha.to_string()),
        Some(json!({ "sha": sha })),
        WorkLinkRole::Result,
        RecordSource::Worktree,
        RecordFidelity::Commit,
        RecordTrust::Medium,
    )
    .await?;
    index_work_record(context, &record).await?;
    Ok(record)
}

async fn ensure_ambient_work_record(context: &WorkStoreContext, cwd: &Path) -> Result<WorkRecord> {
    let facts = git_facts(cwd);
    let target_id = ambient_work_target_id(context, &facts, cwd);
    if let Some(existing) = context
        .store
        .find_work_record_by_link(context.workspace_id, WorkLinkTargetKind::Branch, &target_id)
        .await?
    {
        return Ok(existing);
    }

    let now = Utc::now();
    let title = facts
        .branch
        .as_ref()
        .map(|branch| format!("Local Work on {branch}"))
        .unwrap_or_else(|| "Local Work".to_string());
    let record = WorkRecord {
        work_id: WorkRecordId::new(),
        workspace_id: context.workspace_id,
        title: Some(title),
        objective: None,
        lifecycle: WorkLifecycle::Active,
        primary_repo_root: facts.repo_root.clone(),
        primary_branch: facts.branch.clone(),
        base_commit: None,
        head_commit: facts.head_sha.clone(),
        current_diff_fingerprint: git_fingerprint(cwd),
        trust_verdict: WorkTrustVerdict::UntrustedLocalCapture,
        summary_freshness: WorkSummaryFreshness::Missing,
        metadata_json: Some(json!({
            "created_by": "ctx work capture",
            "grouping": "ambient_branch",
        })),
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let record = context.store.upsert_work_record(&record).await?;
    upsert_work_link(
        context,
        &record.work_id,
        WorkLinkTargetKind::Branch,
        Some(target_id),
        Some(json!({
            "repo_root": facts.repo_root,
            "branch": facts.branch,
            "head_sha": facts.head_sha,
        })),
        WorkLinkRole::Source,
        RecordSource::Worktree,
        RecordFidelity::Declared,
        RecordTrust::Low,
    )
    .await?;
    index_work_record(context, &record).await?;
    Ok(record)
}

async fn upsert_work_link(
    context: &WorkStoreContext,
    work_id: &WorkRecordId,
    target_kind: WorkLinkTargetKind,
    target_id: Option<String>,
    target_json: Option<Value>,
    role: WorkLinkRole,
    source: RecordSource,
    fidelity: RecordFidelity,
    trust: RecordTrust,
) -> Result<WorkRecordLink> {
    let now = Utc::now();
    let link_id = target_id
        .as_deref()
        .map(|target_id| stable_work_record_link_id(work_id, target_kind, target_id, role))
        .unwrap_or_else(WorkRecordLinkId::new);
    let link = WorkRecordLink {
        link_id,
        work_id: work_id.clone(),
        workspace_id: context.workspace_id,
        target_kind,
        target_id,
        target_json,
        role,
        source,
        fidelity,
        trust,
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    context.store.upsert_work_record_link(&link).await
}

fn stable_work_record_link_id(
    work_id: &WorkRecordId,
    target_kind: WorkLinkTargetKind,
    target_id: &str,
    role: WorkLinkRole,
) -> WorkRecordLinkId {
    let digest = sha2::Sha256::digest(
        format!("{}:{target_kind:?}:{target_id}:{role:?}", work_id.0).as_bytes(),
    );
    WorkRecordLinkId::from_id(format!("wln_{}", &hex::encode(digest)[..32]))
}

async fn append_capture_work_event(
    context: &WorkStoreContext,
    work_id: &WorkRecordId,
    tool: AgentWorkCaptureTool,
    argv: &[String],
    exit_code: i32,
    cwd: &Path,
    pr: Option<&PullRequestRef>,
    contribution_id: Option<&ContributionId>,
) -> Result<WorkEvent> {
    let facts = git_facts(cwd);
    let now = Utc::now();
    let redacted_argv = redact_argv(argv);
    let text = format!(
        "Observed {} {} exited {}",
        tool.as_str(),
        redacted_argv.join(" "),
        exit_code
    );
    let event = WorkEvent {
        event_id: WorkEventId::new(),
        work_id: work_id.clone(),
        workspace_id: context.workspace_id,
        sequence: 0,
        source_kind: Some("command".to_string()),
        source_id: contribution_id.map(|id| id.0.clone()),
        event_type: WorkEventType::CommandCapture,
        event_time: now,
        actor_kind: WorkActorKind::System,
        provider: Some(tool.as_str().to_string()),
        harness: None,
        model: None,
        redaction_class: WorkRedactionClass::LocalRedacted,
        source: if tool == AgentWorkCaptureTool::Gh {
            RecordSource::PullRequest
        } else {
            RecordSource::External
        },
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Low,
        payload_json: Some(ctx_core::redaction::redact_json_value(json!({
            "tool": tool.as_str(),
            "argv": redacted_argv,
            "exit_code": exit_code,
            "cwd": redact_work_text(context, &cwd.to_string_lossy()),
            "repo_root": facts.repo_root,
            "branch": facts.branch,
            "head_sha": facts.head_sha,
            "pull_request": pr,
        }))),
        redacted_text: Some(redact_work_text(context, &text)),
        artifact_ref: None,
        created_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let event = context.store.append_work_event(&event).await?;
    index_work_event(context, &event).await?;
    Ok(event)
}

async fn append_evidence_event_and_index(
    context: &WorkStoreContext,
    work_id: &WorkRecordId,
    evidence: &WorkEvidence,
) -> Result<()> {
    let now = Utc::now();
    let event = WorkEvent {
        event_id: WorkEventId::new(),
        work_id: work_id.clone(),
        workspace_id: context.workspace_id,
        sequence: 0,
        source_kind: Some("evidence".to_string()),
        source_id: Some(evidence.evidence_id.0.clone()),
        event_type: WorkEventType::EvidenceObserved,
        event_time: now,
        actor_kind: WorkActorKind::System,
        provider: None,
        harness: None,
        model: None,
        redaction_class: WorkRedactionClass::LocalRedacted,
        source: evidence.source,
        fidelity: evidence.fidelity,
        trust: evidence.trust,
        payload_json: Some(ctx_core::redaction::redact_json_value(
            serde_json::to_value(evidence)?,
        )),
        redacted_text: evidence.claim.clone(),
        artifact_ref: evidence.artifact_ref.clone(),
        created_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    let event = context.store.append_work_event(&event).await?;
    index_work_event(context, &event).await?;
    index_work_evidence(context, evidence).await
}

async fn build_work_context_value(
    context: &WorkStoreContext,
    work_id: WorkRecordId,
    budget_tokens: usize,
) -> Result<Value> {
    let report = build_work_report_value(context, work_id.clone()).await?;
    let evidence = report
        .get("evidence")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let summaries = report
        .get("summaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let work = report
        .get("work")
        .cloned()
        .context("report missing work record")?;
    let objective = work
        .get("objective")
        .or_else(|| work.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("Untitled Work");
    let current_result = report
        .pointer("/trust/reason")
        .and_then(Value::as_str)
        .unwrap_or("No trust state has been computed.");
    Ok(json!({
        "work_id": work_id,
        "budget_tokens": budget_tokens,
        "title": work.get("title").cloned().unwrap_or(Value::Null),
        "state": work.get("lifecycle").cloned().unwrap_or(Value::Null),
        "trust_verdict": work.get("trust_verdict").cloned().unwrap_or(Value::Null),
        "context": {
            "objective": objective,
            "current_result": current_result,
            "key_decisions": summaries.iter().take(3).map(|summary| {
                json!({
                    "text": summary.get("text").cloned().unwrap_or(Value::Null),
                    "citations": [{
                        "source_kind": "summary",
                        "source_id": summary.get("summary_id").cloned().unwrap_or(Value::Null),
                        "freshness": summary.get("freshness").cloned().unwrap_or(Value::Null),
                    }]
                })
            }).collect::<Vec<_>>(),
            "evidence": evidence.iter().take(8).map(|item| {
                json!({
                    "evidence_id": item.get("evidence_id").cloned().unwrap_or(Value::Null),
                    "claim": item.get("claim").cloned().unwrap_or(Value::Null),
                    "freshness": item.get("freshness").cloned().unwrap_or(Value::Null),
                    "status": item.get("status").cloned().unwrap_or(Value::Null),
                })
            }).collect::<Vec<_>>(),
            "open_risks": report.pointer("/trust/open_risks").cloned().unwrap_or_else(|| json!([])),
        },
        "raw_transcript_available": report
            .get("raw_transcript_available")
            .cloned()
            .unwrap_or(Value::Bool(false)),
        "raw_transcript_included": false,
    }))
}

async fn build_work_report_value(
    context: &WorkStoreContext,
    work_id: WorkRecordId,
) -> Result<Value> {
    let store = &context.store;
    let workspace_id = context.workspace_id;
    let work = store
        .get_workspace_work_record(workspace_id, work_id.clone())
        .await?
        .with_context(|| format!("work record {} not found", work_id.0))?;
    let links = store
        .list_work_record_links(workspace_id, work_id.clone())
        .await?;
    let events = store
        .list_work_events(workspace_id, work_id.clone(), Some(500))
        .await?;
    let evidence = store
        .list_work_evidence(workspace_id, work_id.clone())
        .await?;
    let mut summaries = store
        .list_work_summaries(workspace_id, work_id.clone())
        .await?;
    let mut claims = store
        .list_work_summary_claims(workspace_id, None, work_id.clone())
        .await?;
    let (change_sets, contributions) = linked_graph_for_work(store, workspace_id, &links).await?;
    let duplicate_strong_links = store
        .list_strong_work_link_duplicates_for_work(workspace_id, work_id.clone())
        .await?;
    let material_revision_key = material_revision_key_value(
        &work,
        &links,
        &events,
        &evidence,
        &change_sets,
        &contributions,
    );
    summaries.retain(is_default_local_summary);
    for summary in &mut summaries {
        summary.freshness = effective_summary_freshness(
            summary.freshness,
            summary.source_revision_key.as_deref(),
            &material_revision_key,
        );
    }
    claims.retain(|claim| {
        summaries
            .iter()
            .any(|summary| summary.summary_id == claim.summary_id)
    });
    for claim in &mut claims {
        claim.freshness = effective_summary_freshness(
            claim.freshness,
            claim.record_hash.as_deref(),
            &material_revision_key,
        );
    }
    let mut work = work;
    work.trust_verdict = computed_work_trust_verdict(&work, &evidence);
    work.summary_freshness = aggregate_summary_freshness(&summaries, &material_revision_key);
    let evidence_summary = evidence_summary_value(&evidence);
    let trust = trust_report_value(&work, &evidence);
    let raw_transcript_available = events.iter().any(|event| event.payload_json.is_some());
    let value = json!({
        "work": work,
        "links": links,
        "trust": trust,
        "evidence_summary": evidence_summary,
        "evidence": evidence,
        "material_revision_key": material_revision_key,
        "change_summary": {
            "change_sets": change_sets.len(),
            "contributions": contributions.len(),
            "files_changed_known": change_sets.iter().any(|change_set| change_set.fingerprint.is_some()),
            "pull_requests": pull_request_links_from_work_links(&links),
            "commits": commit_links_from_work_links(&links),
        },
        "duplicate_strong_links": duplicate_strong_links,
        "change_sets": change_sets.iter().map(|item| redact_work_serializable(context, item)).collect::<Vec<_>>(),
        "contributions": contributions.iter().map(|item| redact_work_serializable(context, item)).collect::<Vec<_>>(),
        "summaries": summaries,
        "summary_claims": claims,
        "timeline": events,
        "raw_transcript_available": raw_transcript_available,
        "raw_transcript_included": false,
    });
    Ok(redact_work_value(context, &value))
}

async fn linked_graph_for_work(
    store: &Store,
    workspace_id: WorkspaceId,
    links: &[WorkRecordLink],
) -> Result<(Vec<ChangeSet>, Vec<Contribution>)> {
    let mut change_sets = Vec::new();
    let mut contributions = Vec::new();
    for link in links {
        match (link.target_kind, link.target_id.as_deref()) {
            (WorkLinkTargetKind::ChangeSet, Some(id)) => {
                if let Some(change_set) = store
                    .get_workspace_change_set(workspace_id, ChangeSetId::from_id(id))
                    .await?
                {
                    change_sets.push(change_set);
                }
            }
            (WorkLinkTargetKind::Contribution, Some(id)) => {
                if let Some(contribution) =
                    store.get_contribution(ContributionId::from_id(id)).await?
                {
                    if contribution.workspace_id == workspace_id {
                        contributions.push(contribution);
                    }
                }
            }
            _ => {}
        }
    }
    change_sets.sort_by(|left, right| left.id.0.cmp(&right.id.0));
    change_sets.dedup_by(|left, right| left.id == right.id);
    contributions.sort_by(|left, right| left.id.0.cmp(&right.id.0));
    contributions.dedup_by(|left, right| left.id == right.id);
    Ok((change_sets, contributions))
}

fn evidence_summary_value(evidence: &[WorkEvidence]) -> Value {
    let passing = evidence
        .iter()
        .filter(|item| item.status == WorkEvidenceStatus::ObservedPass)
        .count();
    let failing = evidence
        .iter()
        .filter(|item| item.status == WorkEvidenceStatus::ObservedFail)
        .count();
    let stale = evidence
        .iter()
        .filter(|item| item.freshness == WorkEvidenceFreshness::Stale)
        .count();
    let missing = usize::from(evidence.is_empty());
    json!({
        "total": evidence.len(),
        "passing": passing,
        "failing": failing,
        "stale": stale,
        "missing": missing,
    })
}

fn trust_report_value(work: &WorkRecord, evidence: &[WorkEvidence]) -> Value {
    let verdict = computed_work_trust_verdict(work, evidence);
    let reason = match verdict {
        WorkTrustVerdict::Verified => {
            "Fresh verified-provenance evidence is present for this Work record."
        }
        WorkTrustVerdict::Stale => "Some evidence no longer matches the current Work fingerprint.",
        WorkTrustVerdict::MissingEvidence => "No evidence has been recorded for this Work record.",
        WorkTrustVerdict::Partial => {
            "Some evidence is local, incomplete, imported, or lacks verified provenance."
        }
        WorkTrustVerdict::UntrustedLocalCapture => {
            "This record includes user-space local capture; treat it as context, not proof."
        }
        WorkTrustVerdict::Failed => "At least one linked evidence item failed.",
    };
    let recommended_next_action = match verdict {
        WorkTrustVerdict::Verified => "Review the diff and citations.",
        WorkTrustVerdict::Stale => "Rerun the stale evidence commands before review.",
        WorkTrustVerdict::MissingEvidence => {
            "Add evidence with `ctx work evidence <work-id> run -- <command>`."
        }
        WorkTrustVerdict::Partial => {
            "Add verified provenance, fingerprints, artifacts, or citations."
        }
        WorkTrustVerdict::UntrustedLocalCapture => "Link a PR/commit and add fresh evidence.",
        WorkTrustVerdict::Failed => "Fix the failing evidence before marking this ready.",
    };
    let open_risks = match verdict {
        WorkTrustVerdict::Verified => Vec::<String>::new(),
        _ => vec![reason.to_string()],
    };
    json!({
        "verdict": verdict,
        "reason": reason,
        "recommended_next_action": recommended_next_action,
        "open_risks": open_risks,
    })
}

fn pull_request_links_from_work_links(links: &[WorkRecordLink]) -> Vec<Value> {
    links
        .iter()
        .filter(|link| link.target_kind == WorkLinkTargetKind::PullRequest)
        .filter_map(|link| link.target_json.clone())
        .collect()
}

fn commit_links_from_work_links(links: &[WorkRecordLink]) -> Vec<String> {
    links
        .iter()
        .filter(|link| link.target_kind == WorkLinkTargetKind::Commit)
        .filter_map(|link| link.target_id.clone())
        .collect()
}

async fn linked_pr_urls_for_work(
    store: &Store,
    workspace_id: WorkspaceId,
    work_id: WorkRecordId,
) -> Result<Vec<String>> {
    let links = store.list_work_record_links(workspace_id, work_id).await?;
    Ok(links
        .into_iter()
        .filter(|link| link.target_kind == WorkLinkTargetKind::PullRequest)
        .filter_map(|link| {
            link.target_json.and_then(|value| {
                value
                    .get("url")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
        })
        .collect())
}

fn write_work_report_markdown(value: &Value, writer: &mut dyn Write) -> Result<()> {
    let title = value
        .pointer("/work/title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled Work");
    writeln!(writer, "# {}", markdown_plain(title))?;
    writeln!(writer)?;
    writeln!(
        writer,
        "- Work: `{}`",
        value
            .pointer("/work/work_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    )?;
    writeln!(
        writer,
        "- Trust: `{}`",
        value
            .pointer("/trust/verdict")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    )?;
    writeln!(
        writer,
        "- Next: {}",
        markdown_plain(
            value
                .pointer("/trust/recommended_next_action")
                .and_then(Value::as_str)
                .unwrap_or("Review linked evidence.")
        )
    )?;
    writeln!(writer)?;
    writeln!(writer, "## Evidence")?;
    if let Some(items) = value.get("evidence").and_then(Value::as_array) {
        if items.is_empty() {
            writeln!(writer, "- No evidence recorded.")?;
        }
        for item in items {
            writeln!(
                writer,
                "- `{}` `{}` `{}` {}",
                item.get("evidence_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                item.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                item.get("freshness")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                markdown_plain(item.get("claim").and_then(Value::as_str).unwrap_or(""))
            )?;
        }
    }
    Ok(())
}

fn markdown_plain(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('(', "\\(")
        .replace(')', "\\)")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn deterministic_summary_text(report: &Value) -> String {
    let title = report
        .pointer("/work/title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled Work");
    let verdict = report
        .pointer("/trust/verdict")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let evidence_total = report
        .pointer("/evidence_summary/total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let next = report
        .pointer("/trust/recommended_next_action")
        .and_then(Value::as_str)
        .unwrap_or("Review linked evidence.");
    format!("{title}\n\nTrust verdict: {verdict}. Evidence items: {evidence_total}. Next action: {next}")
}

fn is_default_local_summary(summary: &WorkSummary) -> bool {
    summary.generation_method != WorkSummaryGenerationMethod::ProviderLlm
        && !summary.source_material_left_machine
}

fn report_revision_key(report: &Value) -> String {
    if let Some(key) = report.get("material_revision_key").and_then(Value::as_str) {
        return key.to_string();
    }
    let bytes = serde_json::to_vec(report).unwrap_or_default();
    let digest = sha2::Sha256::digest(&bytes);
    hex::encode(digest)
}

fn material_revision_key_value(
    work: &WorkRecord,
    links: &[WorkRecordLink],
    events: &[WorkEvent],
    evidence: &[WorkEvidence],
    change_sets: &[ChangeSet],
    contributions: &[Contribution],
) -> String {
    let material_events: Vec<&WorkEvent> = events
        .iter()
        .filter(|event| {
            !matches!(
                event.event_type,
                WorkEventType::EvidenceObserved | WorkEventType::SummaryGenerated
            )
        })
        .collect();
    let value = json!({
        "work": {
            "work_id": work.work_id,
            "lifecycle": work.lifecycle,
            "head_commit": work.head_commit,
        },
        "links": links,
        "events": material_events,
        "evidence": evidence,
        "change_sets": change_sets,
        "contributions": contributions,
    });
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    let digest = sha2::Sha256::digest(&bytes);
    hex::encode(digest)
}

fn computed_work_trust_verdict(work: &WorkRecord, evidence: &[WorkEvidence]) -> WorkTrustVerdict {
    if evidence
        .iter()
        .any(|item| item.status == WorkEvidenceStatus::ObservedFail)
    {
        WorkTrustVerdict::Failed
    } else if evidence.is_empty() {
        WorkTrustVerdict::MissingEvidence
    } else if evidence
        .iter()
        .any(|item| item.freshness == WorkEvidenceFreshness::Stale)
    {
        WorkTrustVerdict::Stale
    } else if evidence.iter().any(|item| {
        item.status == WorkEvidenceStatus::ObservedPass
            && item.freshness == WorkEvidenceFreshness::Fresh
            && item.trust == RecordTrust::Verified
    }) {
        WorkTrustVerdict::Verified
    } else if evidence
        .iter()
        .any(|item| item.status == WorkEvidenceStatus::ObservedPass)
    {
        WorkTrustVerdict::Partial
    } else {
        work.trust_verdict
    }
}

fn aggregate_summary_freshness(
    summaries: &[WorkSummary],
    material_revision_key: &str,
) -> WorkSummaryFreshness {
    if summaries.is_empty() {
        return WorkSummaryFreshness::Missing;
    }
    let mut saw_partial = false;
    for summary in summaries {
        match effective_summary_freshness(
            summary.freshness,
            summary.source_revision_key.as_deref(),
            material_revision_key,
        ) {
            WorkSummaryFreshness::Stale => return WorkSummaryFreshness::Stale,
            WorkSummaryFreshness::Missing | WorkSummaryFreshness::Partial => saw_partial = true,
            WorkSummaryFreshness::Fresh | WorkSummaryFreshness::Locked => {}
        }
    }
    if saw_partial {
        WorkSummaryFreshness::Partial
    } else {
        WorkSummaryFreshness::Fresh
    }
}

fn effective_summary_freshness(
    stored: WorkSummaryFreshness,
    source_revision_key: Option<&str>,
    material_revision_key: &str,
) -> WorkSummaryFreshness {
    match stored {
        WorkSummaryFreshness::Locked => WorkSummaryFreshness::Locked,
        WorkSummaryFreshness::Fresh if source_revision_key == Some(material_revision_key) => {
            WorkSummaryFreshness::Fresh
        }
        WorkSummaryFreshness::Fresh => WorkSummaryFreshness::Stale,
        other => other,
    }
}

async fn refresh_work_trust_from_evidence_set(
    store: &Store,
    workspace_id: WorkspaceId,
    work_id: &WorkRecordId,
    evidence: &[WorkEvidence],
) -> Result<()> {
    let Some(mut work) = store
        .get_workspace_work_record(workspace_id, work_id.clone())
        .await?
    else {
        return Ok(());
    };
    work.trust_verdict = computed_work_trust_verdict(&work, evidence);
    work.updated_at = Utc::now();
    store.upsert_work_record(&work).await?;
    Ok(())
}

fn evidence_freshness(
    recorded: Option<&GitFingerprint>,
    current: Option<&GitFingerprint>,
) -> WorkEvidenceFreshness {
    match (recorded, current) {
        (Some(left), Some(right)) if left == right => WorkEvidenceFreshness::Fresh,
        (Some(_), Some(_)) => WorkEvidenceFreshness::Stale,
        (Some(_), None) => WorkEvidenceFreshness::Unknown,
        (None, Some(_)) => WorkEvidenceFreshness::Partial,
        (None, None) => WorkEvidenceFreshness::Unknown,
    }
}

fn redact_work_text(context: &WorkStoreContext, value: &str) -> String {
    let redacted =
        ctx_core::models::normalize_archive_text(&ctx_core::redaction::redact_sensitive(value))
            .text;
    let root = context.workspace.root_path.as_str();
    if root.is_empty() {
        redacted
    } else {
        redacted.replace(root, "[redacted:workspace_root]")
    }
}

fn redact_work_serializable<T: Serialize>(context: &WorkStoreContext, value: &T) -> Value {
    serde_json::to_value(value)
        .map(|value| redact_work_value(context, &value))
        .unwrap_or_else(|_| Value::String("[redacted:unserializable]".to_string()))
}

fn redact_work_timeline_event(context: &WorkStoreContext, event: &WorkEvent) -> Value {
    let mut value = redact_work_serializable(context, event);
    if let Value::Object(ref mut object) = value {
        object.remove("payload_json");
        object.remove("artifact_ref");
    }
    value
}

fn redact_work_value(context: &WorkStoreContext, value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_work_text(context, text)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_work_value(context, item))
                .collect(),
        ),
        Value::Object(object) => {
            let mut redacted = serde_json::Map::new();
            for (key, value) in object {
                let key_lc = key.to_ascii_lowercase();
                if key_lc == "payload_json" {
                    continue;
                }
                if matches!(
                    key_lc.as_str(),
                    "absolute_path"
                        | "repo_root"
                        | "root_path"
                        | "primary_repo_root"
                        | "fingerprint_json"
                        | "current_fingerprint_json"
                ) || key_lc.contains("secret")
                    || key_lc.contains("token")
                    || key_lc.contains("password")
                    || key_lc == "authorization"
                    || key_lc == "api_key"
                {
                    redacted.insert(key.clone(), Value::String("[redacted:local_detail]".into()));
                } else {
                    redacted.insert(key.clone(), redact_work_value(context, value));
                }
            }
            Value::Object(redacted)
        }
        other => other.clone(),
    }
}

fn inspect_evidence_file(context: &WorkStoreContext, file: &Path) -> Result<Value> {
    const MAX_EVIDENCE_FILE_BYTES: u64 = 10 * 1024 * 1024;
    let metadata = std::fs::symlink_metadata(file)
        .with_context(|| format!("reading metadata for {}", file.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("evidence file must not be a symlink");
    }
    if !metadata.is_file() {
        bail!("evidence file must be a regular file");
    }
    if metadata.len() > MAX_EVIDENCE_FILE_BYTES {
        bail!(
            "evidence file is too large: {} bytes (max {})",
            metadata.len(),
            MAX_EVIDENCE_FILE_BYTES
        );
    }
    let workspace_root = Path::new(&context.workspace.root_path)
        .canonicalize()
        .with_context(|| {
            format!(
                "canonicalizing workspace root {}",
                context.workspace.root_path
            )
        })?;
    let canonical = file
        .canonicalize()
        .with_context(|| format!("canonicalizing evidence file {}", file.display()))?;
    if !canonical.starts_with(&workspace_root) {
        bail!("evidence file must be inside the workspace root");
    }
    let bytes = std::fs::read(&canonical)
        .with_context(|| format!("reading evidence file {}", canonical.display()))?;
    let digest = hex::encode(sha2::Sha256::digest(&bytes));
    let relative_path = canonical
        .strip_prefix(&workspace_root)
        .unwrap_or(&canonical)
        .to_string_lossy()
        .replace('\\', "/");
    validate_safe_relative_path(&relative_path, "evidence file")?;
    Ok(json!({
        "relative_path": relative_path,
        "sha256": digest,
        "size_bytes": bytes.len(),
        "mime": mime_guess::from_path(&canonical).first_raw().unwrap_or("application/octet-stream"),
        "redaction_class": "local_private",
    }))
}

fn bounded_lossy(bytes: &[u8], max: usize) -> String {
    let end = bytes.len().min(max);
    let mut text = String::from_utf8_lossy(&bytes[..end]).to_string();
    if bytes.len() > max {
        text.push_str("\n[truncated]");
    }
    text
}

fn command_output_preview_ref(context: &WorkStoreContext, stdout: &[u8], stderr: &[u8]) -> Value {
    let stdout_preview = redact_work_text(
        context,
        &bounded_lossy(stdout, COMMAND_OUTPUT_PREVIEW_LIMIT),
    );
    let stderr_preview = redact_work_text(
        context,
        &bounded_lossy(stderr, COMMAND_OUTPUT_PREVIEW_LIMIT),
    );
    let stdout_truncated = stdout.len() > COMMAND_OUTPUT_PREVIEW_LIMIT;
    let stderr_truncated = stderr.len() > COMMAND_OUTPUT_PREVIEW_LIMIT;
    json!({
        "kind": "ctx.work.command_output_preview",
        "redaction_class": "local_redacted",
        "share_safe": true,
        "raw_stdout_retained": false,
        "raw_stderr_retained": false,
        "preview_limit_bytes": COMMAND_OUTPUT_PREVIEW_LIMIT,
        "stdout_size_bytes": stdout.len(),
        "stderr_size_bytes": stderr.len(),
        "stdout_sha256": hex::encode(sha2::Sha256::digest(stdout)),
        "stderr_sha256": hex::encode(sha2::Sha256::digest(stderr)),
        "stdout_redacted": stdout_preview,
        "stderr_redacted": stderr_preview,
        "stdout_preview_bytes": stdout.len().min(COMMAND_OUTPUT_PREVIEW_LIMIT),
        "stderr_preview_bytes": stderr.len().min(COMMAND_OUTPUT_PREVIEW_LIMIT),
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
        "truncated": stdout_truncated || stderr_truncated,
    })
}

async fn index_work_record(context: &WorkStoreContext, work: &WorkRecord) -> Result<()> {
    let text = [
        work.title.as_deref().unwrap_or(""),
        work.objective.as_deref().unwrap_or(""),
        work.primary_branch.as_deref().unwrap_or(""),
        work.head_commit.as_deref().unwrap_or(""),
    ]
    .join("\n");
    let now = Utc::now();
    let doc = WorkSearchDoc {
        doc_id: stable_search_doc_id(work.workspace_id, "work_record", &work.work_id.0),
        workspace_id: work.workspace_id,
        work_id: work.work_id.clone(),
        doc_type: "work_record".to_string(),
        source_id: work.work_id.0.clone(),
        source_kind: "work_record".to_string(),
        event_time: work.updated_at,
        repo_root: work
            .primary_repo_root
            .as_deref()
            .map(|root| redact_work_text(context, root)),
        path: None,
        branch: work.primary_branch.clone(),
        commit_sha: work.head_commit.clone(),
        pr_owner: None,
        pr_repo: None,
        pr_number: None,
        agent_provider: None,
        freshness: WorkEvidenceFreshness::Unknown,
        redaction_class: WorkRedactionClass::LocalRedacted,
        title: work
            .title
            .as_deref()
            .map(|title| redact_work_text(context, title)),
        search_text_redacted: redact_work_text(context, &text),
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    context.store.upsert_work_search_doc(&doc).await?;
    Ok(())
}

async fn index_work_event(context: &WorkStoreContext, event: &WorkEvent) -> Result<()> {
    let now = Utc::now();
    let doc = WorkSearchDoc {
        doc_id: stable_search_doc_id(event.workspace_id, "work_event", &event.event_id.0),
        workspace_id: event.workspace_id,
        work_id: event.work_id.clone(),
        doc_type: "event".to_string(),
        source_id: event.event_id.0.clone(),
        source_kind: "event".to_string(),
        event_time: event.event_time,
        repo_root: None,
        path: None,
        branch: None,
        commit_sha: None,
        pr_owner: None,
        pr_repo: None,
        pr_number: None,
        agent_provider: event.provider.clone(),
        freshness: WorkEvidenceFreshness::Unknown,
        redaction_class: event.redaction_class,
        title: Some(format!("{:?}", event.event_type)),
        search_text_redacted: event.redacted_text.clone().unwrap_or_default(),
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    context.store.upsert_work_search_doc(&doc).await?;
    Ok(())
}

async fn index_work_pull_request_link(
    context: &WorkStoreContext,
    work_id: &WorkRecordId,
    pr: &PullRequestRef,
) -> Result<()> {
    let now = Utc::now();
    let target_id = pull_request_target_id(pr);
    let number = pr.number.to_string();
    let doc = WorkSearchDoc {
        doc_id: stable_search_doc_id(context.workspace_id, "work_pull_request", &target_id),
        workspace_id: context.workspace_id,
        work_id: work_id.clone(),
        doc_type: "pull_request".to_string(),
        source_id: target_id,
        source_kind: "pull_request".to_string(),
        event_time: now,
        repo_root: None,
        path: None,
        branch: None,
        commit_sha: None,
        pr_owner: Some(pr.owner.clone()),
        pr_repo: Some(pr.repo.clone()),
        pr_number: Some(pr.number),
        agent_provider: Some(pr.provider.clone()),
        freshness: WorkEvidenceFreshness::Unknown,
        redaction_class: WorkRedactionClass::LocalRedacted,
        title: pr
            .title
            .as_deref()
            .map(|title| redact_work_text(context, title))
            .or_else(|| Some(format!("PR {}/{}#{}", pr.owner, pr.repo, pr.number))),
        search_text_redacted: redact_work_text(
            context,
            &[
                pr.provider.as_str(),
                pr.owner.as_str(),
                pr.repo.as_str(),
                number.as_str(),
                pr.title.as_deref().unwrap_or(""),
                pr.url.as_deref().unwrap_or(""),
            ]
            .join("\n"),
        ),
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    context.store.upsert_work_search_doc(&doc).await?;
    Ok(())
}

async fn index_work_evidence(context: &WorkStoreContext, evidence: &WorkEvidence) -> Result<()> {
    let now = Utc::now();
    let doc = WorkSearchDoc {
        doc_id: stable_search_doc_id(
            evidence.workspace_id,
            "work_evidence",
            &evidence.evidence_id.0,
        ),
        workspace_id: evidence.workspace_id,
        work_id: evidence.work_id.clone(),
        doc_type: "evidence".to_string(),
        source_id: evidence.evidence_id.0.clone(),
        source_kind: "evidence".to_string(),
        event_time: evidence.finished_at,
        repo_root: evidence
            .repo_root
            .as_deref()
            .map(|root| redact_work_text(context, root)),
        path: None,
        branch: evidence.branch.clone(),
        commit_sha: evidence.head_sha.clone(),
        pr_owner: None,
        pr_repo: None,
        pr_number: None,
        agent_provider: None,
        freshness: evidence.freshness,
        redaction_class: WorkRedactionClass::LocalRedacted,
        title: evidence
            .claim
            .as_deref()
            .map(|claim| redact_work_text(context, claim)),
        search_text_redacted: redact_work_text(
            context,
            &[
                evidence.claim.as_deref().unwrap_or(""),
                evidence.command.as_deref().unwrap_or(""),
                &evidence.argv.join(" "),
            ]
            .join("\n"),
        ),
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    context.store.upsert_work_search_doc(&doc).await?;
    Ok(())
}

async fn index_work_summary(context: &WorkStoreContext, summary: &WorkSummary) -> Result<()> {
    let now = Utc::now();
    let doc = WorkSearchDoc {
        doc_id: stable_search_doc_id(summary.workspace_id, "work_summary", &summary.summary_id.0),
        workspace_id: summary.workspace_id,
        work_id: summary.work_id.clone(),
        doc_type: "summary".to_string(),
        source_id: summary.summary_id.0.clone(),
        source_kind: "summary".to_string(),
        event_time: summary.generated_at,
        repo_root: None,
        path: None,
        branch: None,
        commit_sha: None,
        pr_owner: None,
        pr_repo: None,
        pr_number: None,
        agent_provider: summary.provider.clone(),
        freshness: match summary.freshness {
            WorkSummaryFreshness::Fresh | WorkSummaryFreshness::Locked => {
                WorkEvidenceFreshness::Fresh
            }
            WorkSummaryFreshness::Stale => WorkEvidenceFreshness::Stale,
            WorkSummaryFreshness::Partial => WorkEvidenceFreshness::Partial,
            WorkSummaryFreshness::Missing => WorkEvidenceFreshness::Unknown,
        },
        redaction_class: WorkRedactionClass::LocalRedacted,
        title: Some(format!("{:?}", summary.kind)),
        search_text_redacted: redact_work_text(context, &summary.text),
        created_at: now,
        updated_at: now,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    context.store.upsert_work_search_doc(&doc).await?;
    Ok(())
}

fn pull_request_ref_from_work_link(link: &WorkRecordLink) -> Option<PullRequestRef> {
    if link.target_kind != WorkLinkTargetKind::PullRequest {
        return None;
    }
    link.target_json
        .as_ref()
        .and_then(|value| serde_json::from_value::<PullRequestRef>(value.clone()).ok())
}

fn stable_search_doc_id(workspace_id: WorkspaceId, kind: &str, source_id: &str) -> WorkSearchDocId {
    let digest = sha2::Sha256::digest(format!("{}:{kind}:{source_id}", workspace_id.0).as_bytes());
    let hex = hex::encode(digest);
    WorkSearchDocId::from_id(format!("wsd_{}", &hex[..32]))
}

fn pull_request_target_id(pr: &PullRequestRef) -> String {
    format!("{}:{}/{}#{}", pr.provider, pr.owner, pr.repo, pr.number)
}

fn ambient_work_target_id(context: &WorkStoreContext, facts: &GitFacts, cwd: &Path) -> String {
    format!(
        "{}|{}|{}|{}",
        facts
            .repo_root
            .as_deref()
            .unwrap_or(context.workspace.root_path.as_str()),
        facts.branch.as_deref().unwrap_or("(detached)"),
        facts.head_sha.as_deref().unwrap_or("(unknown-head)"),
        cwd.to_string_lossy()
    )
}

fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    git_output_lossy(cwd, args).map(|output| output.trim().to_string())
}

fn git_output_lossy(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

fn read_nul_delimited_argv_from_stdin() -> Result<Vec<String>> {
    let mut bytes = Vec::new();
    io::stdin()
        .read_to_end(&mut bytes)
        .context("reading captured argv from stdin")?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    parse_nul_delimited_argv(&bytes)
}

fn parse_nul_delimited_argv(bytes: &[u8]) -> Result<Vec<String>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8(part.to_vec()).context("captured argv stdin must be UTF-8"))
        .collect()
}

fn classify_captured_command(tool: AgentWorkCaptureTool, argv: &[String]) -> String {
    match tool {
        AgentWorkCaptureTool::Git => git_subcommand(argv)
            .map(|arg| format!("git.{arg}"))
            .unwrap_or_else(|| "git.unknown".to_string()),
        AgentWorkCaptureTool::Gh => match argv {
            [area, action, ..] if area == "pr" => format!("gh.pr.{action}"),
            [area, action, ..] => format!("gh.{area}.{action}"),
            [area] => format!("gh.{area}"),
            [] => "gh.unknown".to_string(),
        },
    }
}

fn captured_command_allowlist_denial_reason(
    tool: AgentWorkCaptureTool,
    argv: &[String],
    exit_code: i32,
) -> Option<&'static str> {
    if exit_code != 0 {
        return Some("only successful allowlisted metadata commands are captured");
    }
    match tool {
        AgentWorkCaptureTool::Git => match git_subcommand(argv) {
            Some("commit" | "push") => None,
            _ => Some("git command is not in the metadata capture allowlist"),
        },
        AgentWorkCaptureTool::Gh => match argv {
            [area, action, ..]
                if area == "pr" && matches!(action.as_str(), "create" | "view" | "status") =>
            {
                None
            }
            _ => Some("gh command is not in the metadata capture allowlist"),
        },
    }
}

fn git_subcommand(argv: &[String]) -> Option<&str> {
    let mut index = 0;
    while index < argv.len() {
        let arg = argv[index].as_str();
        if matches!(
            arg,
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
        ) {
            index += 2;
            continue;
        }
        if arg.starts_with("--git-dir=")
            || arg.starts_with("--work-tree=")
            || arg.starts_with("--namespace=")
            || arg.starts_with("--config-env=")
        {
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        return Some(arg);
    }
    None
}

fn captured_command_denial_reason(
    tool: AgentWorkCaptureTool,
    argv: &[String],
) -> Option<&'static str> {
    if argv.iter().any(|arg| is_secret_capture_arg(arg)) {
        return Some("arguments include auth or secret material");
    }
    match tool {
        AgentWorkCaptureTool::Gh => {
            let command = argv.first().map(|arg| arg.as_str()).unwrap_or_default();
            if matches!(command, "auth" | "secret") {
                return Some("gh auth and secret commands are not metadata-safe");
            }
        }
        AgentWorkCaptureTool::Git => {
            let command = git_subcommand(argv).unwrap_or_default();
            if matches!(
                command,
                "credential" | "credential-cache" | "credential-store"
            ) {
                return Some("git credential commands are not metadata-safe");
            }
            if command == "config" && argv.iter().any(|arg| is_sensitive_git_config_arg(arg)) {
                return Some("git config command targets auth or secret settings");
            }
            if argv.iter().any(|arg| is_sensitive_git_option_arg(arg)) {
                return Some("git command includes auth or secret options");
            }
        }
    }
    None
}

fn is_secret_capture_arg(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("passwd")
        || lower.contains("api-key")
        || lower.contains("apikey")
        || ctx_core::redaction::redact_sensitive(arg) != arg
}

fn is_sensitive_git_config_arg(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    lower.contains("credential")
        || lower.contains("extraheader")
        || lower.contains("authorization")
        || lower.contains("oauth")
        || lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
}

fn is_sensitive_git_option_arg(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    lower.starts_with("http.extraheader")
        || lower.contains("authorization:")
        || lower.contains("authorization=")
}

fn evidence_kind_for_captured_command(classification: &str) -> Option<WorkEvidenceKind> {
    let lower = classification.to_ascii_lowercase();
    if lower.contains("test") {
        Some(WorkEvidenceKind::Test)
    } else if lower.contains("lint") || lower.contains("clippy") {
        Some(WorkEvidenceKind::Lint)
    } else if lower.contains("fmt") || lower.contains("format") {
        Some(WorkEvidenceKind::Format)
    } else if lower.contains("typecheck") || lower.contains("check") {
        Some(WorkEvidenceKind::Typecheck)
    } else if lower.contains("build") {
        Some(WorkEvidenceKind::Build)
    } else {
        None
    }
}

fn redact_argv(argv: &[String]) -> Vec<String> {
    let mut redact_next = false;
    argv.iter()
        .map(|arg| {
            if redact_next {
                redact_next = false;
                return "[redacted:secret]".to_string();
            }
            let lower = arg.to_ascii_lowercase();
            let sensitive = lower.contains("token")
                || lower.contains("secret")
                || lower.contains("password")
                || lower.contains("passwd")
                || lower.contains("api-key")
                || lower.contains("apikey");
            if sensitive && arg.starts_with('-') {
                if arg.contains('=') {
                    let flag = arg.split_once('=').map(|(flag, _)| flag).unwrap_or(arg);
                    format!("{flag}=[redacted:secret]")
                } else {
                    redact_next = true;
                    arg.clone()
                }
            } else {
                ctx_core::redaction::redact_sensitive(arg)
            }
        })
        .collect()
}

fn find_pull_request_ref(argv: &[String]) -> Option<PullRequestRef> {
    for arg in argv {
        if let Ok(pr) = parse_github_pull_request_url(arg) {
            return Some(pr);
        }
        if let Some(pr) = parse_github_pull_request_refish(arg) {
            return Some(pr);
        }
    }

    if argv.first().map(String::as_str) != Some("pr") {
        return None;
    }
    let repo = find_repo_arg(argv)?;
    let number = argv
        .iter()
        .skip(2)
        .find_map(|arg| arg.parse::<i64>().ok())?;
    let (owner, repo) = parse_owner_repo(repo)?;
    Some(PullRequestRef {
        provider: "github".to_string(),
        owner,
        repo,
        number,
        id: None,
        url: None,
        title: None,
    })
}

fn find_repo_arg(argv: &[String]) -> Option<&str> {
    for (index, arg) in argv.iter().enumerate() {
        if arg == "--repo" || arg == "-R" {
            return argv.get(index + 1).map(String::as_str);
        }
        if let Some(value) = arg.strip_prefix("--repo=") {
            return Some(value);
        }
    }
    None
}

fn parse_github_pull_request_url(value: &str) -> Result<PullRequestRef> {
    let url = Url::parse(value).with_context(|| format!("`{value}` is not a URL"))?;
    if url.scheme() != "https" && url.scheme() != "http" {
        bail!("only http and https GitHub pull request URLs are supported locally today");
    }
    let host = url.host_str().unwrap_or_default();
    if host != "github.com" && host != "www.github.com" {
        bail!("only github.com pull request URLs are supported locally today");
    }
    let segments = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.len() < 4 || segments[2] != "pull" {
        bail!("expected GitHub PR URL shaped like https://github.com/owner/repo/pull/123");
    }
    let number = segments[3]
        .parse::<i64>()
        .with_context(|| format!("pull request number `{}` must be an integer", segments[3]))?;
    Ok(PullRequestRef {
        provider: "github".to_string(),
        owner: segments[0].to_string(),
        repo: segments[1].trim_end_matches(".git").to_string(),
        number,
        id: None,
        url: Some(value.to_string()),
        title: None,
    })
}

fn parse_github_pull_request_refish(value: &str) -> Option<PullRequestRef> {
    let (owner_repo, number) = value.split_once('#')?;
    let (owner, repo) = parse_owner_repo(owner_repo)?;
    Some(PullRequestRef {
        provider: "github".to_string(),
        owner,
        repo,
        number: number.parse().ok()?,
        id: None,
        url: None,
        title: None,
    })
}

fn parse_owner_repo(value: &str) -> Option<(String, String)> {
    let cleaned = value.trim().trim_end_matches(".git");
    let mut parts = cleaned.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

fn upsert_pull_request_link(change_set: &mut ChangeSet, link: PullRequestLink) {
    if let Some(existing) = change_set
        .pull_requests
        .iter_mut()
        .find(|existing| same_pull_request(&existing.pull_request, &link.pull_request))
    {
        existing.kind = link.kind;
        existing.url = link.url.or_else(|| existing.url.clone());
        existing.title = link.title.or_else(|| existing.title.clone());
        existing.state = link.state.or_else(|| existing.state.clone());
    } else {
        change_set.pull_requests.push(link);
    }
}

async fn upsert_pr_link_contribution(
    store: &Store,
    workspace_id: WorkspaceId,
    change_set: &ChangeSet,
    pr: &PullRequestRef,
) -> Result<Contribution> {
    let contributions = store.list_workspace_contributions(workspace_id).await?;
    if let Some(existing) = contributions.into_iter().find(|contribution| {
        contribution.change_set_id.as_ref() == Some(&change_set.id)
            && matches!(
                &contribution.target,
                ContributionEndpoint::PullRequest { pull_request }
                    if same_pull_request(pull_request, pr)
            )
    }) {
        return Ok(existing);
    }

    let contribution = Contribution {
        id: ContributionId::new(),
        workspace_id,
        change_set_id: Some(change_set.id.clone()),
        subject: ContributionEndpoint::ChangeSet {
            change_set_id: change_set.id.clone(),
        },
        target: ContributionEndpoint::PullRequest {
            pull_request: pr.clone(),
        },
        role: ContributionRole::Result,
        source: RecordSource::PullRequest,
        origin: RecordOrigin::Agent,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Medium,
        summary: Some(format!(
            "Linked PR {}/{}#{} to change set {}",
            pr.owner, pr.repo, pr.number, change_set.id
        )),
        fingerprint: change_set.fingerprint.clone(),
        issuer: Some("ctx work link-pr".to_string()),
        metadata_json: Some(json!({
            "kind": "ctx.work.pr_link",
        })),
        source_records: Vec::new(),
        created_at: None,
        updated_at: None,
        schema_version: AGENT_WORK_SCHEMA_VERSION,
    };
    store.upsert_contribution(&contribution).await
}

async fn find_change_set_for_pull_request(
    store: &Store,
    workspace_id: WorkspaceId,
    pr: &PullRequestRef,
) -> Result<Option<ChangeSet>> {
    let change_sets = store.list_workspace_change_sets(workspace_id).await?;
    Ok(change_sets.into_iter().find(|change_set| {
        change_set
            .pull_requests
            .iter()
            .any(|link| same_pull_request(&link.pull_request, pr))
    }))
}

fn same_pull_request(a: &PullRequestRef, b: &PullRequestRef) -> bool {
    a.provider == b.provider && a.owner == b.owner && a.repo == b.repo && a.number == b.number
}

async fn list_work_records(args: AgentWorkListArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    let bundle = load_work_export(&context.store, context.workspace_id).await?;

    if args.json {
        let value = filtered_list_value(&context, &bundle, args.kind);
        serde_json::to_writer_pretty(&mut *writer, &value)?;
        writeln!(writer)?;
    } else {
        writeln!(writer, "workspace: {}", context.workspace_id.0)?;
        if args.kind.includes_change_sets() {
            writeln!(writer, "change_sets: {}", bundle.change_sets.len())?;
            for change_set in &bundle.change_sets {
                writeln!(
                    writer,
                    "- {}{}",
                    change_set.id,
                    optional_title_suffix(change_set.title.as_deref())
                )?;
            }
        }
        if args.kind.includes_contributions() {
            writeln!(writer, "contributions: {}", bundle.contributions.len())?;
            for contribution in &bundle.contributions {
                writeln!(
                    writer,
                    "- {}{}",
                    contribution.id,
                    optional_title_suffix(contribution.summary.as_deref())
                )?;
            }
        }
    }
    if !args.json {
        write_diagnostic(
            writer,
            DiagnosticSeverity::Info,
            "ctx.work.list.completed",
            &format!(
                "listed Work records from {} for workspace {}",
                context.data_root.display(),
                context.workspace_id.0
            ),
        )?;
    }
    Ok(())
}

async fn show_work_record(args: AgentWorkShowArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    let kind = args
        .kind
        .unwrap_or_else(|| infer_record_kind_from_id(&args.id));
    let value = match kind {
        AgentWorkRecordKind::All => {
            find_work_record_value(&context.store, context.workspace_id, &args.id).await?
        }
        AgentWorkRecordKind::ChangeSet => context
            .store
            .get_workspace_change_set(context.workspace_id, ChangeSetId::from_id(args.id.clone()))
            .await?
            .map(serde_json::to_value)
            .transpose()?
            .with_context(|| format!("change set {} not found", args.id))?,
        AgentWorkRecordKind::Contribution => context
            .store
            .get_contribution(ContributionId::from_id(args.id.clone()))
            .await?
            .filter(|contribution| contribution.workspace_id == context.workspace_id)
            .map(serde_json::to_value)
            .transpose()?
            .with_context(|| format!("contribution {} not found", args.id))?,
    };

    if args.json {
        let value = redact_work_value(&context, &value);
        serde_json::to_writer_pretty(&mut *writer, &value)?;
        writeln!(writer)?;
    } else {
        writeln!(writer, "workspace: {}", context.workspace_id.0)?;
        if let Some(record_type) = record_type_for_value(&value) {
            writeln!(writer, "record_type: {record_type}")?;
        }
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            writeln!(writer, "id: {id}")?;
        }
        if let Some(title) = value
            .get("title")
            .or_else(|| value.get("summary"))
            .and_then(Value::as_str)
        {
            writeln!(
                writer,
                "summary: {}",
                ctx_core::redaction::redact_sensitive(title)
            )?;
        }
    }
    if !args.json {
        write_diagnostic(
            writer,
            DiagnosticSeverity::Info,
            "ctx.work.show.completed",
            &format!(
                "showed Work record {} from workspace {}",
                args.id, context.workspace_id.0
            ),
        )?;
    }
    Ok(())
}

async fn export_work_records(args: AgentWorkExportArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    let bundle = load_work_export(&context.store, context.workspace_id).await?;
    let value = build_agent_work_export_value(&bundle, &context, args.redaction_profile)
        .context("serializing Work export")?;
    validate_value(AgentWorkSchemaKind::AgentWork, &value)
        .context("generated Work export failed local validation")?;

    let wrote_file = if let Some(output) = args.output {
        write_json_file(&output, &value)?;
        writeln!(
            writer,
            "exported {} change sets and {} contributions to {}",
            bundle.change_sets.len(),
            bundle.contributions.len(),
            output.display()
        )?;
        true
    } else {
        serde_json::to_writer_pretty(&mut *writer, &value)?;
        writeln!(writer)?;
        false
    };
    if wrote_file {
        write_diagnostic(
            writer,
            DiagnosticSeverity::Info,
            "ctx.work.export.completed",
            &format!(
                "exported Work records from {} for workspace {} with {} redaction",
                context.data_root.display(),
                context.workspace_id.0,
                args.redaction_profile.as_str()
            ),
        )?;
    }
    Ok(())
}

async fn import_work_records(args: AgentWorkImportArgs, writer: &mut dyn Write) -> Result<()> {
    let value = read_json_file(&args.file).with_context(|| {
        durable_diagnostic(
            DiagnosticSeverity::Error,
            "ctx.work.import.invalid_json",
            &format!("failed to parse {}", args.file.display()),
        )
    })?;
    let bundle = decode_agent_work_import_value(value).with_context(|| {
        durable_diagnostic(
            DiagnosticSeverity::Error,
            "ctx.work.import.invalid_agent_work",
            &format!(
                "{} is not a valid import-safe local AgentWork export; use `ctx work export --redaction-profile full-local` for durable imports",
                args.file.display()
            ),
        )
    })?;
    let context = open_work_store(&args.store).await?;
    validate_import_workspace(context.workspace_id, &bundle)?;

    if args.dry_run {
        context
            .store
            .validate_agent_work_import_records(&bundle.change_sets, &bundle.contributions)
            .await?;
    } else {
        context
            .store
            .import_agent_work_records(&bundle.change_sets, &bundle.contributions)
            .await?;
    }

    writeln!(
        writer,
        "{} {} change sets and {} contributions from {}",
        if args.dry_run {
            "validated"
        } else {
            "imported"
        },
        bundle.change_sets.len(),
        bundle.contributions.len(),
        args.file.display()
    )?;
    write_diagnostic(
        writer,
        DiagnosticSeverity::Info,
        if args.dry_run {
            "ctx.work.import.dry_run_completed"
        } else {
            "ctx.work.import.completed"
        },
        &format!(
            "{} Work records into workspace {}; hosted/team enforcement state is not imported",
            if args.dry_run {
                "validated"
            } else {
                "imported"
            },
            context.workspace_id.0
        ),
    )?;
    Ok(())
}

fn write_schema(args: AgentWorkSchemaArgs, writer: &mut dyn Write) -> Result<()> {
    if let Some(kind) = args.kind {
        writeln!(writer, "{}", schema_for_kind(kind))?;
        return Ok(());
    }

    writeln!(writer, "known ctx work schemas:")?;
    for kind in AgentWorkSchemaKind::ALL {
        writeln!(
            writer,
            "- {} ({})",
            kind.as_str(),
            schema_id_for_kind(*kind)
        )?;
    }
    writeln!(
        writer,
        "Use `ctx work schema --kind <schema>` to print a schema."
    )?;
    Ok(())
}

fn schema_for_kind(kind: AgentWorkSchemaKind) -> &'static str {
    match kind {
        AgentWorkSchemaKind::WorkBundle => WORK_BUNDLE_SCHEMA,
        AgentWorkSchemaKind::AgentWork => {
            include_str!("../../../../schemas/agent-work/v1.schema.json")
        }
        AgentWorkSchemaKind::ChangeSet => {
            include_str!("../../../../schemas/agent-work/change-set.v1.schema.json")
        }
        AgentWorkSchemaKind::Contribution => {
            include_str!("../../../../schemas/agent-work/contribution.v1.schema.json")
        }
        AgentWorkSchemaKind::Events => include_str!("../../../../schemas/events/v1.schema.json"),
        AgentWorkSchemaKind::ToolCall => {
            include_str!("../../../../schemas/events/tool-call.v1.schema.json")
        }
        AgentWorkSchemaKind::Transcripts => {
            include_str!("../../../../schemas/transcripts/v1.schema.json")
        }
        AgentWorkSchemaKind::PluginManifest => {
            include_str!("../../../../schemas/plugins/plugin-manifest.v1.schema.json")
        }
    }
}

fn schema_id_for_kind(kind: AgentWorkSchemaKind) -> &'static str {
    match kind {
        AgentWorkSchemaKind::WorkBundle => "https://schemas.ctx.rs/work/bundle.v1.schema.json",
        AgentWorkSchemaKind::AgentWork => "https://schemas.ctx.rs/agent-work/v1.schema.json",
        AgentWorkSchemaKind::ChangeSet => {
            "https://schemas.ctx.rs/agent-work/change-set.v1.schema.json"
        }
        AgentWorkSchemaKind::Contribution => {
            "https://schemas.ctx.rs/agent-work/contribution.v1.schema.json"
        }
        AgentWorkSchemaKind::Events => "https://schemas.ctx.rs/events/v1.schema.json",
        AgentWorkSchemaKind::ToolCall => "https://schemas.ctx.rs/events/tool-call.v1.schema.json",
        AgentWorkSchemaKind::Transcripts => "https://schemas.ctx.rs/transcripts/v1.schema.json",
        AgentWorkSchemaKind::PluginManifest => {
            "https://schemas.ctx.rs/plugins/plugin-manifest.v1.schema.json"
        }
    }
}

impl AgentWorkSchemaKind {
    const ALL: &'static [Self] = &[
        Self::WorkBundle,
        Self::AgentWork,
        Self::ChangeSet,
        Self::Contribution,
        Self::Events,
        Self::ToolCall,
        Self::Transcripts,
        Self::PluginManifest,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::WorkBundle => "work-bundle",
            Self::AgentWork => "agent-work",
            Self::ChangeSet => "change-set",
            Self::Contribution => "contribution",
            Self::Events => "events",
            Self::ToolCall => "tool-call",
            Self::Transcripts => "transcripts",
            Self::PluginManifest => "plugin-manifest",
        }
    }
}

struct WorkStoreContext {
    data_root: PathBuf,
    workspace_id: WorkspaceId,
    workspace: Workspace,
    store: Store,
}

async fn open_work_store(args: &AgentWorkStoreArgs) -> Result<WorkStoreContext> {
    let cwd = std::env::current_dir().ok();
    open_work_store_for_path(args, cwd.as_deref()).await
}

async fn open_work_store_for_path(
    args: &AgentWorkStoreArgs,
    cwd: Option<&Path>,
) -> Result<WorkStoreContext> {
    let data_root = resolve_data_root(args.data_dir.as_deref())?;
    let manager = StoreManager::open(&data_root)
        .await
        .with_context(|| format!("opening ctx store at {}", data_root.display()))?;
    let workspace = resolve_workspace(&manager, args.workspace_id.as_deref(), cwd).await?;
    let workspace_id = workspace.id;
    let store = manager
        .workspace(workspace_id)
        .await
        .with_context(|| format!("opening workspace store {}", workspace_id.0))?;
    Ok(WorkStoreContext {
        data_root,
        workspace_id,
        workspace,
        store,
    })
}

fn resolve_data_root(data_dir: Option<&Path>) -> Result<PathBuf> {
    let raw = match data_dir {
        Some(path) => path.to_path_buf(),
        None => match std::env::var("CTX_DATA_ROOT") {
            Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
            _ => {
                let base = BaseDirs::new().context("resolving home dir")?;
                base.home_dir().join(".ctx")
            }
        },
    };
    ctx_http_auth::daemon::prepare_daemon_data_root(raw)
}

async fn resolve_workspace(
    manager: &StoreManager,
    workspace_id: Option<&str>,
    cwd: Option<&Path>,
) -> Result<Workspace> {
    if let Some(workspace_id) = workspace_id {
        let workspace_id = parse_workspace_id(workspace_id)?;
        return manager
            .global()
            .get_workspace(workspace_id)
            .await?
            .with_context(|| format!("workspace {} is not registered", workspace_id.0));
    }

    let workspaces = manager
        .global()
        .list_workspaces()
        .await
        .context("listing local ctx workspaces")?;
    if let Some(cwd) = cwd {
        if let Some(workspace) = workspace_for_cwd(&workspaces, cwd) {
            return Ok(workspace.clone());
        }
    }
    match workspaces.as_slice() {
        [workspace] => Ok(workspace.clone()),
        [] => bail!("no ctx workspaces are registered in the selected data root"),
        _ => {
            let available = workspaces
                .iter()
                .map(|workspace| format!("{} ({})", workspace.id.0, workspace.name))
                .collect::<Vec<_>>()
                .join(", ");
            bail!("multiple ctx workspaces are registered; pass --workspace-id. Available: {available}")
        }
    }
}

fn workspace_for_cwd<'a>(workspaces: &'a [Workspace], cwd: &Path) -> Option<&'a Workspace> {
    let cwd = normalize_existing_path(cwd);
    workspaces
        .iter()
        .filter_map(|workspace| {
            let root = normalize_existing_path(Path::new(&workspace.root_path));
            if cwd.starts_with(&root) {
                Some((root.components().count(), workspace))
            } else {
                None
            }
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, workspace)| workspace)
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn parse_workspace_id(value: &str) -> Result<WorkspaceId> {
    Ok(WorkspaceId(
        uuid::Uuid::parse_str(value.trim()).with_context(|| {
            format!("workspace id `{value}` must be a UUID from the local ctx workspace registry")
        })?,
    ))
}

async fn load_work_export(store: &Store, workspace_id: WorkspaceId) -> Result<AgentWorkExport> {
    let change_sets = store.list_workspace_change_sets(workspace_id).await?;
    let contributions = store.list_workspace_contributions(workspace_id).await?;
    Ok(AgentWorkExport {
        change_sets,
        contributions,
    })
}

fn build_agent_work_export_value(
    bundle: &AgentWorkExport,
    context: &WorkStoreContext,
    profile: AgentWorkRedactionProfile,
) -> Result<Value> {
    let raw_value = serde_json::to_value(bundle).context("serializing raw AgentWork records")?;
    validate_value(AgentWorkSchemaKind::AgentWork, &raw_value)
        .context("validating raw AgentWork records")?;

    let (agent_work, stats) = if profile == AgentWorkRedactionProfile::SafeSummary {
        let preview = redaction_preview(&raw_value);
        let agent_work =
            serde_json::from_value(preview.value).context("decoding redacted AgentWork records")?;
        (agent_work, preview.stats)
    } else {
        (
            bundle.clone(),
            ctx_core::models::RunArchiveNormalizationStats::default(),
        )
    };

    let envelope = AgentWorkExportEnvelope {
        kind: AGENT_WORK_EXPORT_ENVELOPE_KIND.to_string(),
        schema_version: AGENT_WORK_EXPORT_ENVELOPE_SCHEMA_VERSION,
        agent_work_schema_version: AGENT_WORK_SCHEMA_VERSION,
        provenance: AgentWorkExportProvenance {
            source_kind: AGENT_WORK_EXPORT_SOURCE_KIND.to_string(),
            workspace_id: context.workspace_id,
            exported_at: Utc::now(),
        },
        redaction: AgentWorkExportRedaction {
            profile,
            import_safe: profile.default_import_safe(),
            stats,
        },
        agent_work,
    };
    serde_json::to_value(envelope).context("serializing AgentWork export envelope")
}

fn decode_agent_work_import_value(value: Value) -> Result<AgentWorkExport> {
    validate_value(AgentWorkSchemaKind::AgentWork, &value)?;
    if is_agent_work_export_envelope(&value) {
        let envelope: AgentWorkExportEnvelope =
            serde_json::from_value(value).context("decoding AgentWork export envelope")?;
        validate_import_redaction(&envelope.redaction)?;
        return Ok(envelope.agent_work);
    }

    validate_legacy_import_safety(&value)?;
    serde_json::from_value(value).context("decoding legacy local AgentWork export")
}

fn validate_import_redaction(redaction: &AgentWorkExportRedaction) -> Result<()> {
    if redaction.import_safe || redaction.profile == AgentWorkRedactionProfile::FullLocal {
        return Ok(());
    }
    bail!(
        "AgentWork export uses {} redaction and is not marked import_safe",
        redaction.profile.as_str()
    )
}

fn validate_legacy_import_safety(value: &Value) -> Result<()> {
    if contains_redaction_marker(value) {
        bail!(
            "legacy AgentWork export contains redaction markers; re-export with --redaction-profile full-local or provide an import_safe envelope"
        );
    }
    Ok(())
}

fn contains_redaction_marker(value: &Value) -> bool {
    const REDACTION_MARKERS: &[&str] = &[
        "[redacted:absolute_path]",
        "[redacted:provider_ref]",
        "[redacted:pty_stream]",
        "[redacted:secret]",
        "[omitted:transcript_body]",
        "[REDACTED]",
    ];

    match value {
        Value::String(value) => REDACTION_MARKERS
            .iter()
            .any(|marker| value.contains(marker)),
        Value::Array(items) => items.iter().any(contains_redaction_marker),
        Value::Object(object) => object.values().any(contains_redaction_marker),
        _ => false,
    }
}

fn filtered_list_value(
    context: &WorkStoreContext,
    bundle: &AgentWorkExport,
    kind: AgentWorkRecordKind,
) -> Value {
    json!({
        "change_sets": if kind.includes_change_sets() {
            bundle
                .change_sets
                .iter()
                .map(|item| redact_work_serializable(context, item))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        },
        "contributions": if kind.includes_contributions() {
            bundle
                .contributions
                .iter()
                .map(|item| redact_work_serializable(context, item))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        },
    })
}

async fn find_work_record_value(
    store: &Store,
    workspace_id: WorkspaceId,
    id: &str,
) -> Result<Value> {
    if let Some(change_set) = store
        .get_workspace_change_set(workspace_id, ChangeSetId::from_id(id))
        .await?
    {
        return serde_json::to_value(change_set).context("serializing change set");
    }
    if let Some(contribution) = store.get_contribution(ContributionId::from_id(id)).await? {
        if contribution.workspace_id == workspace_id {
            return serde_json::to_value(contribution).context("serializing contribution");
        }
    }
    bail!("Work record {id} not found in workspace {}", workspace_id.0)
}

fn infer_record_kind_from_id(id: &str) -> AgentWorkRecordKind {
    if id.starts_with("chg_") {
        AgentWorkRecordKind::ChangeSet
    } else if id.starts_with("con_") {
        AgentWorkRecordKind::Contribution
    } else {
        AgentWorkRecordKind::All
    }
}

fn record_type_for_value(value: &Value) -> Option<&'static str> {
    let object = value.as_object()?;
    if object.contains_key("subject") && object.contains_key("target") {
        Some("contribution")
    } else if object.contains_key("target_branch")
        || object.contains_key("head_revision")
        || object.contains_key("base_revision")
        || object.contains_key("pull_requests")
    {
        Some("change_set")
    } else {
        None
    }
}

fn optional_title_suffix(title: Option<&str>) -> String {
    title
        .map(ctx_core::redaction::redact_sensitive)
        .filter(|title| !title.trim().is_empty())
        .map(|title| format!(" - {title}"))
        .unwrap_or_default()
}

fn validate_import_workspace(workspace_id: WorkspaceId, bundle: &AgentWorkExport) -> Result<()> {
    for change_set in &bundle.change_sets {
        if change_set.workspace_id != workspace_id {
            bail!(
                "change set {} belongs to workspace {}; selected workspace is {}",
                change_set.id,
                change_set.workspace_id.0,
                workspace_id.0
            );
        }
    }
    for contribution in &bundle.contributions {
        if contribution.workspace_id != workspace_id {
            bail!(
                "contribution {} belongs to workspace {}; selected workspace is {}",
                contribution.id,
                contribution.workspace_id.0,
                workspace_id.0
            );
        }
    }
    Ok(())
}

fn read_json_file(path: &PathBuf) -> Result<Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read JSON file {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("invalid JSON in {}", path.display()))
}

fn write_json_file(path: &Path, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).context("serializing JSON output")?;
    bytes.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| {
            format!(
                "creating {}; refusing to overwrite existing files or follow symlinks",
                path.display()
            )
        })?;
    file.write_all(&bytes)
        .with_context(|| format!("writing {}", path.display()))
}

fn infer_schema_kind(value: &Value) -> Result<AgentWorkSchemaKind> {
    let object = value
        .as_object()
        .context("expected a JSON object; pass `--kind` for a specific local schema")?;

    if let Some(kind) = object.get("kind").and_then(Value::as_str) {
        return match kind {
            "ctx.work.bundle" | "work-bundle" | "work_bundle" => {
                Ok(AgentWorkSchemaKind::WorkBundle)
            }
            AGENT_WORK_EXPORT_ENVELOPE_KIND => Ok(AgentWorkSchemaKind::AgentWork),
            other => bail!(
                "unknown Work schema kind `{other}`; pass `--kind` with one of: {}",
                AgentWorkSchemaKind::ALL
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }

    if is_agent_work_export_envelope(value)
        || object.contains_key("change_sets")
        || object.contains_key("contributions")
    {
        return Ok(AgentWorkSchemaKind::AgentWork);
    }
    if object.contains_key("subject") && object.contains_key("target") {
        return Ok(AgentWorkSchemaKind::Contribution);
    }
    if object.contains_key("workspace_id") && object.contains_key("id") {
        return Ok(AgentWorkSchemaKind::ChangeSet);
    }
    if object.contains_key("event_type") && object.contains_key("payload_json") {
        return Ok(AgentWorkSchemaKind::Events);
    }
    if object.contains_key("tool_call_id") {
        return Ok(AgentWorkSchemaKind::ToolCall);
    }
    if object.contains_key("record_type") {
        return Ok(AgentWorkSchemaKind::Transcripts);
    }
    if object.contains_key("entrypoints") || object.contains_key("contributes") {
        return Ok(AgentWorkSchemaKind::PluginManifest);
    }

    bail!(
        "could not infer a known Work schema shape; pass `--kind` with one of: {}",
        AgentWorkSchemaKind::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn validate_value(kind: AgentWorkSchemaKind, value: &Value) -> Result<()> {
    match kind {
        AgentWorkSchemaKind::WorkBundle => validate_work_bundle(value),
        AgentWorkSchemaKind::AgentWork => validate_agent_work(value),
        AgentWorkSchemaKind::ChangeSet => validate_change_set(value, "$"),
        AgentWorkSchemaKind::Contribution => validate_contribution(value, "$"),
        AgentWorkSchemaKind::Events => validate_required_fields(
            value,
            "$",
            &[
                "seq",
                "id",
                "session_id",
                "event_type",
                "payload_json",
                "created_at",
            ],
        ),
        AgentWorkSchemaKind::ToolCall => validate_required_fields(
            value,
            "$",
            &[
                "session_id",
                "tool_call_id",
                "turn_id",
                "order_seq",
                "created_at",
                "updated_at",
            ],
        ),
        AgentWorkSchemaKind::Transcripts => validate_required_fields(value, "$", &["record_type"]),
        AgentWorkSchemaKind::PluginManifest => validate_plugin_manifest(value),
    }?;
    validate_relative_path_fields(value, "$")
}

const AGENT_WORK_FIELDS: &[&str] = &["change_sets", "contributions"];
const AGENT_WORK_EXPORT_ENVELOPE_FIELDS: &[&str] = &[
    "kind",
    "schema_version",
    "agent_work_schema_version",
    "provenance",
    "redaction",
    "agent_work",
];
const EXPORT_PROVENANCE_FIELDS: &[&str] = &["source_kind", "workspace_id", "exported_at"];
const EXPORT_REDACTION_FIELDS: &[&str] = &["profile", "import_safe", "stats"];
const CHANGE_SET_FIELDS: &[&str] = &[
    "id",
    "workspace_id",
    "source_worktree_id",
    "source",
    "origin",
    "fidelity",
    "trust",
    "title",
    "summary",
    "description",
    "fingerprint",
    "base_revision",
    "head_revision",
    "target_branch",
    "pull_requests",
    "source_records",
    "issuer",
    "created_at",
    "updated_at",
    "schema_version",
];
const CONTRIBUTION_FIELDS: &[&str] = &[
    "id",
    "workspace_id",
    "change_set_id",
    "subject",
    "target",
    "role",
    "source",
    "origin",
    "fidelity",
    "trust",
    "summary",
    "fingerprint",
    "issuer",
    "metadata_json",
    "source_records",
    "created_at",
    "updated_at",
    "schema_version",
];
const SOURCE_RECORD_FIELDS: &[&str] = &[
    "schema_version",
    "record_id",
    "previous_hash",
    "payload_hash",
    "record_hash",
    "created_at",
];
const GIT_FINGERPRINT_FIELDS: &[&str] = &[
    "repo_root",
    "head_sha",
    "branch",
    "patch_sha256",
    "status_sha256",
    "untracked_sha256",
    "changed_paths_sha256",
    "dirty",
];
const PULL_REQUEST_REF_FIELDS: &[&str] =
    &["provider", "owner", "repo", "number", "id", "url", "title"];
const PULL_REQUEST_LINK_FIELDS: &[&str] = &["kind", "pull_request", "url", "title", "state"];
const RECORD_SOURCE_VALUES: &[&str] = &[
    "unknown",
    "worktree",
    "session",
    "merge_queue",
    "pull_request",
    "manual",
    "external",
];
const RECORD_ORIGIN_VALUES: &[&str] = &["unknown", "user", "agent", "system", "imported"];
const RECORD_FIDELITY_VALUES: &[&str] =
    &["unknown", "declared", "summary", "diff", "commit", "exact"];
const RECORD_TRUST_VALUES: &[&str] = &["unknown", "low", "medium", "high", "verified"];
const CONTRIBUTION_ROLE_VALUES: &[&str] = &[
    "authored",
    "validated",
    "reviewed",
    "context",
    "result",
    "related",
];
const PULL_REQUEST_LINK_KIND_VALUES: &[&str] = &["source", "target", "result", "related"];
const CONTRIBUTION_ENDPOINT_KIND_VALUES: &[&str] = &[
    "account",
    "workspace",
    "task",
    "session",
    "run",
    "agent",
    "system",
    "worktree",
    "change_set",
    "change-set",
    "pull_request",
    "pull-request",
    "artifact",
    "check",
    "evidence",
    "review_attestation",
    "review-attestation",
    "commit",
    "branch",
    "file",
    "external",
];

fn validate_no_unknown_fields(
    object: &serde_json::Map<String, Value>,
    path: &str,
    allowed_fields: &[&str],
) -> Result<()> {
    for key in object.keys() {
        if !allowed_fields.contains(&key.as_str()) {
            bail!("{path} has unknown property `{key}`");
        }
    }
    Ok(())
}

fn validate_record_metadata_fields(
    object: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<()> {
    validate_enum_field(object, path, "source", RECORD_SOURCE_VALUES, "RecordSource")?;
    validate_enum_field(object, path, "origin", RECORD_ORIGIN_VALUES, "RecordOrigin")?;
    validate_enum_field(
        object,
        path,
        "fidelity",
        RECORD_FIDELITY_VALUES,
        "RecordFidelity",
    )?;
    validate_enum_field(object, path, "trust", RECORD_TRUST_VALUES, "RecordTrust")
}

fn validate_enum_field(
    object: &serde_json::Map<String, Value>,
    path: &str,
    field: &str,
    allowed_values: &[&str],
    schema_name: &str,
) -> Result<()> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    match value.as_str() {
        Some(value) if allowed_values.contains(&value) => Ok(()),
        Some(value) => bail!(
            "{path}.{field} has invalid enum value `{value}` for {schema_name}; expected one of: {}",
            allowed_values.join(", ")
        ),
        None => bail!("{path}.{field} must be a string"),
    }
}

fn validate_optional_object_field(
    object: &serde_json::Map<String, Value>,
    path: &str,
    field: &str,
    validate: fn(&Value, &str) -> Result<()>,
) -> Result<()> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    validate(value, &format!("{path}.{field}"))
}

fn validate_optional_array_items(
    object: &serde_json::Map<String, Value>,
    path: &str,
    field: &str,
    validate: fn(&Value, &str) -> Result<()>,
) -> Result<()> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    let items = value
        .as_array()
        .with_context(|| format!("{path}.{field} must be an array"))?;
    for (index, item) in items.iter().enumerate() {
        validate(item, &format!("{path}.{field}[{index}]"))?;
    }
    Ok(())
}

fn validate_agent_work_source_record(value: &Value, path: &str) -> Result<()> {
    validate_required_fields(
        value,
        path,
        &["record_id", "payload_hash", "record_hash", "created_at"],
    )?;
    let object = value
        .as_object()
        .with_context(|| format!("{path} must be a JSON object"))?;
    validate_no_unknown_fields(object, path, SOURCE_RECORD_FIELDS)?;
    validate_schema_version(value, path)
}

fn validate_git_fingerprint(value: &Value, path: &str) -> Result<()> {
    validate_required_fields(
        value,
        path,
        &[
            "patch_sha256",
            "status_sha256",
            "untracked_sha256",
            "changed_paths_sha256",
            "dirty",
        ],
    )?;
    let object = value
        .as_object()
        .with_context(|| format!("{path} must be a JSON object"))?;
    validate_no_unknown_fields(object, path, GIT_FINGERPRINT_FIELDS)?;
    for field in [
        "patch_sha256",
        "status_sha256",
        "untracked_sha256",
        "changed_paths_sha256",
    ] {
        validate_string_field(object, path, field)?;
    }
    if object.get("dirty").and_then(Value::as_bool).is_none() {
        bail!("{path}.dirty must be a boolean");
    }
    Ok(())
}

fn validate_pull_request_ref(value: &Value, path: &str) -> Result<()> {
    validate_required_fields(value, path, &["provider", "owner", "repo", "number"])?;
    let object = value
        .as_object()
        .with_context(|| format!("{path} must be a JSON object"))?;
    validate_no_unknown_fields(object, path, PULL_REQUEST_REF_FIELDS)?;
    for field in ["provider", "owner", "repo"] {
        validate_string_field(object, path, field)?;
    }
    let Some(number) = object.get("number") else {
        bail!("{path} requires `number`");
    };
    if !(number.as_i64().is_some() || number.as_u64().is_some()) {
        bail!("{path}.number must be an integer");
    }
    Ok(())
}

fn validate_pull_request_link(value: &Value, path: &str) -> Result<()> {
    validate_required_fields(value, path, &["pull_request"])?;
    let object = value
        .as_object()
        .with_context(|| format!("{path} must be a JSON object"))?;
    validate_no_unknown_fields(object, path, PULL_REQUEST_LINK_FIELDS)?;
    validate_enum_field(
        object,
        path,
        "kind",
        PULL_REQUEST_LINK_KIND_VALUES,
        "PullRequestLinkKind",
    )?;
    validate_pull_request_ref(
        object
            .get("pull_request")
            .context("pull request link requires `pull_request`")?,
        &format!("{path}.pull_request"),
    )
}

fn validate_contribution_endpoint(value: &Value, path: &str) -> Result<()> {
    let object = value
        .as_object()
        .with_context(|| format!("{path} must be a JSON object"))?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .with_context(|| format!("{path}.kind must be a string"))?;

    match kind {
        "account" => {
            validate_no_unknown_fields(object, path, &["kind", "account_id"])?;
            validate_string_field(object, path, "account_id")
        }
        "workspace" => {
            validate_no_unknown_fields(object, path, &["kind", "workspace_id"])?;
            validate_string_field(object, path, "workspace_id")
        }
        "task" => {
            validate_no_unknown_fields(object, path, &["kind", "task_id", "id"])?;
            validate_any_string_identity(object, path, &["task_id", "id"])
        }
        "session" => {
            validate_no_unknown_fields(
                object,
                path,
                &["kind", "session_id", "provider", "id", "turn_id", "run_id"],
            )?;
            validate_any_string_identity(object, path, &["session_id", "id"])
        }
        "run" => {
            validate_no_unknown_fields(object, path, &["kind", "run_id", "id", "session_id"])?;
            validate_any_string_identity(object, path, &["run_id", "id"])
        }
        "agent" => {
            validate_no_unknown_fields(object, path, &["kind", "session_id", "run_id", "label"])
        }
        "system" => validate_no_unknown_fields(object, path, &["kind", "label"]),
        "worktree" => {
            validate_no_unknown_fields(object, path, &["kind", "worktree_id", "id"])?;
            validate_any_string_identity(object, path, &["worktree_id", "id"])
        }
        "change_set" | "change-set" => {
            validate_no_unknown_fields(object, path, &["kind", "change_set_id", "id"])?;
            validate_any_string_identity(object, path, &["change_set_id", "id"])
        }
        "pull_request" | "pull-request" => {
            validate_no_unknown_fields(object, path, &["kind", "pull_request"])?;
            validate_pull_request_ref(
                object
                    .get("pull_request")
                    .context("pull request endpoint requires `pull_request`")?,
                &format!("{path}.pull_request"),
            )
        }
        "artifact" => {
            validate_no_unknown_fields(
                object,
                path,
                &["kind", "artifact_id", "id", "digest", "relative_path"],
            )?;
            validate_any_string_identity(object, path, &["artifact_id", "id", "digest", "relative_path"])
        }
        "check" => {
            validate_no_unknown_fields(object, path, &["kind", "check_id", "id"])?;
            validate_any_string_identity(object, path, &["check_id", "id"])
        }
        "evidence" => {
            validate_no_unknown_fields(object, path, &["kind", "id"])?;
            validate_string_field(object, path, "id")
        }
        "review_attestation" | "review-attestation" => {
            validate_no_unknown_fields(object, path, &["kind", "id"])?;
            validate_string_field(object, path, "id")
        }
        "commit" => {
            validate_no_unknown_fields(object, path, &["kind", "sha"])?;
            validate_string_field(object, path, "sha")
        }
        "branch" => {
            validate_no_unknown_fields(object, path, &["kind", "name"])?;
            validate_string_field(object, path, "name")
        }
        "file" => {
            validate_no_unknown_fields(object, path, &["kind", "path", "worktree_id"])?;
            validate_string_field(object, path, "path")
        }
        "external" => {
            validate_no_unknown_fields(object, path, &["kind", "source", "identifier", "url"])?;
            validate_non_empty_string_field(object, path, "source")?;
            validate_any_string_identity(object, path, &["identifier", "url"])
        }
        other => bail!(
            "{path}.kind has invalid enum value `{other}` for ContributionEndpoint; expected one of: {}",
            CONTRIBUTION_ENDPOINT_KIND_VALUES.join(", ")
        ),
    }
}

fn validate_string_field(
    object: &serde_json::Map<String, Value>,
    path: &str,
    field: &str,
) -> Result<()> {
    match object.get(field) {
        Some(value) if value.as_str().is_some() => Ok(()),
        Some(_) => bail!("{path}.{field} must be a string"),
        None => bail!("{path} requires `{field}`"),
    }
}

fn validate_non_empty_string_field(
    object: &serde_json::Map<String, Value>,
    path: &str,
    field: &str,
) -> Result<()> {
    validate_string_field(object, path, field)?;
    if object
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(str::is_empty)
    {
        bail!("{path}.{field} must not be empty");
    }
    Ok(())
}

fn validate_any_string_identity(
    object: &serde_json::Map<String, Value>,
    path: &str,
    fields: &[&str],
) -> Result<()> {
    if fields
        .iter()
        .any(|field| object.get(*field).and_then(Value::as_str).is_some())
    {
        return Ok(());
    }
    bail!(
        "{path} requires at least one identity field with a string value: {}",
        fields.join(", ")
    )
}

fn validate_agent_work(value: &Value) -> Result<()> {
    if is_agent_work_export_envelope(value) {
        return validate_agent_work_export_envelope(value);
    }
    validate_agent_work_records(value, "$")
}

fn validate_agent_work_records(value: &Value, path: &str) -> Result<()> {
    let object = value
        .as_object()
        .with_context(|| format!("{path} must be a JSON object"))?;
    validate_no_unknown_fields(object, path, AGENT_WORK_FIELDS)?;
    let change_sets = object
        .get("change_sets")
        .and_then(Value::as_array)
        .with_context(|| format!("{path} requires `change_sets` array"))?;
    let contributions = object
        .get("contributions")
        .and_then(Value::as_array)
        .with_context(|| format!("{path} requires `contributions` array"))?;

    for (index, change_set) in change_sets.iter().enumerate() {
        validate_change_set(change_set, &format!("{path}.change_sets[{index}]"))?;
    }
    for (index, contribution) in contributions.iter().enumerate() {
        validate_contribution(contribution, &format!("{path}.contributions[{index}]"))?;
    }
    Ok(())
}

fn is_agent_work_export_envelope(value: &Value) -> bool {
    value
        .as_object()
        .and_then(|object| object.get("kind"))
        .and_then(Value::as_str)
        == Some(AGENT_WORK_EXPORT_ENVELOPE_KIND)
}

fn agent_work_records_value(value: &Value) -> &Value {
    if is_agent_work_export_envelope(value) {
        value.get("agent_work").unwrap_or(value)
    } else {
        value
    }
}

fn validate_agent_work_export_envelope(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .context("agent-work export envelope must be a JSON object")?;
    validate_no_unknown_fields(object, "$", AGENT_WORK_EXPORT_ENVELOPE_FIELDS)?;
    validate_required_fields(
        value,
        "$",
        &[
            "kind",
            "schema_version",
            "agent_work_schema_version",
            "provenance",
            "redaction",
            "agent_work",
        ],
    )?;
    match object.get("kind").and_then(Value::as_str) {
        Some(AGENT_WORK_EXPORT_ENVELOPE_KIND) => {}
        Some(other) => bail!("unknown AgentWork export kind `{other}` at $.kind"),
        None => bail!("agent-work export envelope requires `kind`"),
    }
    validate_schema_version(value, "$")?;
    match object
        .get("agent_work_schema_version")
        .and_then(Value::as_i64)
    {
        Some(AGENT_WORK_SCHEMA_VERSION) => {}
        Some(other) => bail!(
            "$.agent_work_schema_version must be {} for this local CLI slice, got {other}",
            AGENT_WORK_SCHEMA_VERSION
        ),
        None => bail!("agent-work export envelope requires `agent_work_schema_version`"),
    }

    let provenance = object
        .get("provenance")
        .context("agent-work export envelope requires `provenance`")?;
    let provenance_object = provenance
        .as_object()
        .context("$.provenance must be a JSON object")?;
    validate_no_unknown_fields(provenance_object, "$.provenance", EXPORT_PROVENANCE_FIELDS)?;
    validate_required_fields(
        provenance,
        "$.provenance",
        &["source_kind", "workspace_id", "exported_at"],
    )?;

    let redaction = object
        .get("redaction")
        .context("agent-work export envelope requires `redaction`")?;
    let redaction_object = redaction
        .as_object()
        .context("$.redaction must be a JSON object")?;
    validate_no_unknown_fields(redaction_object, "$.redaction", EXPORT_REDACTION_FIELDS)?;
    validate_required_fields(redaction, "$.redaction", &["profile", "import_safe"])?;
    validate_redaction_profile_value(redaction, "$.redaction.profile")?;

    let agent_work = object
        .get("agent_work")
        .context("agent-work export envelope requires `agent_work`")?;
    validate_agent_work_records(agent_work, "$.agent_work")
}

fn validate_redaction_profile_value(value: &Value, path: &str) -> Result<()> {
    let profile = value
        .get("profile")
        .and_then(Value::as_str)
        .with_context(|| format!("{path} must be a string"))?;
    match profile {
        "safe_summary" | "safe-summary" | "full_local" | "full-local" => Ok(()),
        other => bail!("{path} has unknown redaction profile `{other}`"),
    }
}

fn validate_change_set(value: &Value, path: &str) -> Result<()> {
    validate_required_fields(value, path, &["id", "workspace_id"])?;
    let object = value
        .as_object()
        .with_context(|| format!("{path} must be a JSON object"))?;
    validate_no_unknown_fields(object, path, CHANGE_SET_FIELDS)?;
    validate_schema_version(value, path)?;
    validate_record_metadata_fields(object, path)?;
    validate_optional_object_field(object, path, "fingerprint", validate_git_fingerprint)?;
    validate_optional_array_items(object, path, "pull_requests", validate_pull_request_link)?;
    validate_optional_array_items(
        object,
        path,
        "source_records",
        validate_agent_work_source_record,
    )
}

fn validate_contribution(value: &Value, path: &str) -> Result<()> {
    validate_required_fields(value, path, &["id", "workspace_id", "subject", "target"])?;
    let object = value
        .as_object()
        .with_context(|| format!("{path} must be a JSON object"))?;
    validate_no_unknown_fields(object, path, CONTRIBUTION_FIELDS)?;
    validate_schema_version(value, path)?;
    validate_record_metadata_fields(object, path)?;
    validate_enum_field(
        object,
        path,
        "role",
        CONTRIBUTION_ROLE_VALUES,
        "ContributionRole",
    )?;
    validate_contribution_endpoint(
        object
            .get("subject")
            .context("contribution requires `subject`")?,
        &format!("{path}.subject"),
    )?;
    validate_contribution_endpoint(
        object
            .get("target")
            .context("contribution requires `target`")?,
        &format!("{path}.target"),
    )?;
    validate_optional_object_field(object, path, "fingerprint", validate_git_fingerprint)?;
    validate_optional_array_items(
        object,
        path,
        "source_records",
        validate_agent_work_source_record,
    )
}

fn validate_work_bundle(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .context("work-bundle must be a JSON object")?;
    match object.get("kind").and_then(Value::as_str) {
        Some("ctx.work.bundle" | "work-bundle" | "work_bundle") => {}
        Some(other) => bail!("unknown Work bundle kind `{other}` at $.kind"),
        None => bail!("work-bundle requires `kind`"),
    }
    validate_schema_version(value, "$")?;
    let objects = object
        .get("objects")
        .and_then(Value::as_array)
        .context("work-bundle requires `objects` array")?;
    for (index, object) in objects.iter().enumerate() {
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .with_context(|| format!("work-bundle object at $.objects[{index}] requires `path`"))?;
        validate_safe_relative_path(path, &format!("$.objects[{index}].path"))?;
    }
    Ok(())
}

fn validate_plugin_manifest(value: &Value) -> Result<()> {
    let manifest: PluginManifest =
        serde_json::from_value(value.clone()).context("plugin-manifest failed to deserialize")?;
    manifest
        .validate()
        .map_err(|error| anyhow::anyhow!("plugin-manifest failed structural validation: {error:?}"))
}

fn validate_required_fields(value: &Value, path: &str, fields: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .with_context(|| format!("{path} must be a JSON object"))?;
    for field in fields {
        if !object.contains_key(*field) {
            bail!("{path} requires `{field}`");
        }
    }
    Ok(())
}

fn validate_schema_version(value: &Value, path: &str) -> Result<()> {
    let Some(version) = value.get("schema_version") else {
        return Ok(());
    };
    if version.as_i64() == Some(1) {
        return Ok(());
    }
    bail!("{path}.schema_version must be 1 for this local CLI slice")
}

fn validate_relative_path_fields(value: &Value, path: &str) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = format!("{path}.{key}");
                if matches!(key.as_str(), "path" | "relative_path") {
                    if let Some(path_value) = child.as_str() {
                        validate_safe_relative_path(path_value, &child_path)?;
                    }
                }
                validate_relative_path_fields(child, &child_path)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                validate_relative_path_fields(child, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_safe_relative_path(path: &str, location: &str) -> Result<()> {
    if path.is_empty() {
        bail!("{location} must not be empty");
    }
    if path.starts_with('/') || path.starts_with("\\\\") {
        bail!("{location} must be a workspace-relative path, not an absolute path");
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        bail!("{location} must be a workspace-relative path, not an absolute path");
    }
    for component in path.split(['/', '\\']) {
        if matches!(component, "." | "..") {
            bail!("{location} must not contain dot or dot-dot traversal components");
        }
    }
    Ok(())
}

fn write_inspection(path: &PathBuf, value: &Value, writer: &mut dyn Write) -> Result<()> {
    let kind = infer_schema_kind(value).ok();
    writeln!(writer, "file: {}", path.display())?;
    writeln!(
        writer,
        "schema: {}",
        kind.map_or("unknown", AgentWorkSchemaKind::as_str)
    )?;
    match kind {
        Some(AgentWorkSchemaKind::WorkBundle) => {
            let object_count = value
                .get("objects")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            writeln!(writer, "objects: {object_count}")?;
            if let Some(source) = value.get("source").and_then(Value::as_str) {
                writeln!(
                    writer,
                    "source: {}",
                    ctx_core::redaction::redact_sensitive(source)
                )?;
            }
        }
        Some(AgentWorkSchemaKind::AgentWork) => {
            let agent_work = agent_work_records_value(value);
            writeln!(
                writer,
                "change_sets: {}",
                agent_work
                    .get("change_sets")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            )?;
            writeln!(
                writer,
                "contributions: {}",
                agent_work
                    .get("contributions")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            )?;
            if let Some(profile) = value
                .get("redaction")
                .and_then(|redaction| redaction.get("profile"))
                .and_then(Value::as_str)
            {
                writeln!(writer, "redaction_profile: {profile}")?;
            }
        }
        Some(_) | None => {
            if let Some(id) = value.get("id").and_then(Value::as_str) {
                writeln!(writer, "id: {}", ctx_core::redaction::redact_sensitive(id))?;
            }
        }
    }
    writeln!(writer, "raw secret-like fields: omitted")?;
    write_diagnostic(
        writer,
        DiagnosticSeverity::Info,
        "ctx.work.inspect.summary",
        &format!("{} inspected with safe summary output", path.display()),
    )?;
    Ok(())
}

fn write_redaction_preview(path: &PathBuf, value: &Value, writer: &mut dyn Write) -> Result<()> {
    let preview = redaction_preview(value);
    writeln!(writer, "file: {}", path.display())?;
    writeln!(writer, "redaction preview:")?;
    writeln!(
        writer,
        "- secret fields redacted: {}",
        preview.stats.redacted_secret_fields
    )?;
    writeln!(
        writer,
        "- secret values redacted: {}",
        preview.stats.redacted_secret_values
    )?;
    writeln!(
        writer,
        "- absolute paths redacted: {}",
        preview.stats.redacted_absolute_paths
    )?;
    writeln!(
        writer,
        "- transcript bodies omitted: {}",
        preview.stats.omitted_content_payloads
    )?;
    writeln!(writer, "preview_json:")?;
    serde_json::to_writer_pretty(&mut *writer, &preview.value)?;
    writeln!(writer)?;
    let severity = if preview.stats.redacted_secret_fields > 0
        || preview.stats.redacted_secret_values > 0
        || preview.stats.redacted_absolute_paths > 0
        || preview.stats.omitted_content_payloads > 0
    {
        DiagnosticSeverity::Warning
    } else {
        DiagnosticSeverity::Info
    };
    write_diagnostic(
        writer,
        severity,
        "ctx.work.redaction_preview.completed",
        &format!(
            "{} redaction preview completed without exporting raw transcript bodies or obvious local secrets",
            path.display()
        ),
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

impl DiagnosticSeverity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

fn write_diagnostic(
    writer: &mut dyn Write,
    severity: DiagnosticSeverity,
    code: &str,
    message: &str,
) -> Result<()> {
    writeln!(writer, "{}", durable_diagnostic(severity, code, message))?;
    Ok(())
}

fn durable_diagnostic(severity: DiagnosticSeverity, code: &str, message: &str) -> String {
    let safe_message = message.replace(['\r', '\n'], "\\n");
    format!(
        "diagnostic:\n  source_kind: ctx.work.cli\n  severity: {}\n  code: {}\n  message: {}\n  timestamp: {}\n  redaction_export_policy: safe_summary\n  enforcement: none_local_diagnostic_only",
        severity.as_str(),
        code,
        safe_message,
        Utc::now().to_rfc3339()
    )
}

struct RedactionPreview {
    value: Value,
    stats: ctx_core::models::RunArchiveNormalizationStats,
}

fn redaction_preview(value: &Value) -> RedactionPreview {
    let mut normalized = ctx_core::models::normalize_archive_json(value);
    omit_transcript_bodies(&mut normalized.value, &mut normalized.stats);
    RedactionPreview {
        value: normalized.value,
        stats: normalized.stats,
    }
}

fn omit_transcript_bodies(
    value: &mut Value,
    stats: &mut ctx_core::models::RunArchiveNormalizationStats,
) {
    match value {
        Value::Object(object) => {
            let looks_like_message = object.contains_key("role")
                || object
                    .get("record_type")
                    .and_then(Value::as_str)
                    .is_some_and(|record_type| matches!(record_type, "message" | "event"))
                || object
                    .get("event_type")
                    .and_then(Value::as_str)
                    .is_some_and(is_transcript_like_event_type);
            let payload_json_looks_sensitive = object
                .get("payload_json")
                .is_some_and(contains_transcript_payload_key);
            for key in [
                "content",
                "content_fragment",
                "delta",
                "full_content",
                "message",
                "text",
                "body",
                "transcript",
                "payload",
                "payload_json",
            ] {
                if (looks_like_message || payload_json_looks_sensitive) && object.contains_key(key)
                {
                    object.insert(
                        key.to_string(),
                        Value::String("[omitted:transcript_body]".to_string()),
                    );
                    stats.omitted_content_payloads += 1;
                }
            }
            for child in object.values_mut() {
                omit_transcript_bodies(child, stats);
            }
        }
        Value::Array(items) => {
            for child in items {
                omit_transcript_bodies(child, stats);
            }
        }
        _ => {}
    }
}

fn is_transcript_like_event_type(event_type: &str) -> bool {
    let normalized = event_type.to_ascii_lowercase();
    ["assistant", "message", "thought", "transcript", "user"]
        .iter()
        .any(|needle| normalized.contains(needle))
}

fn contains_transcript_payload_key(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, child)| {
            is_transcript_payload_key(key) || contains_transcript_payload_key(child)
        }),
        Value::Array(items) => items.iter().any(contains_transcript_payload_key),
        _ => false,
    }
}

fn is_transcript_payload_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    [
        "body",
        "content",
        "delta",
        "fragment",
        "message",
        "text",
        "thought",
        "transcript",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

const WORK_BUNDLE_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://schemas.ctx.rs/work/bundle.v1.schema.json",
  "title": "WorkBundle",
  "description": "Local ctx Work import/export bundle manifest. This CLI slice validates the core object index structurally.",
  "type": "object",
  "required": ["kind", "schema_version", "objects"],
  "properties": {
    "kind": {
      "enum": ["ctx.work.bundle", "work-bundle", "work_bundle"]
    },
    "schema_version": {
      "const": 1
    },
    "objects": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["path"],
        "properties": {
          "path": {
            "type": "string",
            "description": "Bundle-relative object path. Absolute paths and dot traversal are rejected."
          },
          "sha256": {
            "type": "string"
          },
          "bytes": {
            "type": "integer"
          }
        }
      }
    }
  }
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use ctx_core::ids::TaskId;
    use ctx_core::models::{
        ContributionEndpoint, ContributionRole, RecordFidelity, RecordOrigin, RecordSource,
        VcsKind, Workspace,
    };
    use serde_json::json;
    use tempfile::TempDir;

    use crate::cli::{Cli, Commands};

    #[test]
    fn work_and_agent_work_commands_parse_to_same_cli_surface() {
        let work = Cli::parse_from(["ctx", "work", "schema"]);
        assert!(matches!(work.command, Commands::Work(_)));

        let agent_work = Cli::parse_from(["ctx", "agent-work", "schema"]);
        assert!(matches!(agent_work.command, Commands::Work(_)));
    }

    #[test]
    fn computed_trust_uses_aggregate_evidence_state() {
        let workspace_id = WorkspaceId::new();
        let work = test_work_record(workspace_id);
        let mut fresh_pass = test_work_evidence(
            workspace_id,
            work.work_id.clone(),
            WorkEvidenceStatus::ObservedPass,
            WorkEvidenceFreshness::Fresh,
        );
        let mut verified_pass = fresh_pass.clone();
        verified_pass.trust = RecordTrust::Verified;
        fresh_pass.trust = RecordTrust::Medium;
        let stale_pass = test_work_evidence(
            workspace_id,
            work.work_id.clone(),
            WorkEvidenceStatus::ObservedPass,
            WorkEvidenceFreshness::Stale,
        );
        let fail = test_work_evidence(
            workspace_id,
            work.work_id.clone(),
            WorkEvidenceStatus::ObservedFail,
            WorkEvidenceFreshness::Fresh,
        );

        assert_eq!(
            computed_work_trust_verdict(&work, &[verified_pass]),
            WorkTrustVerdict::Verified
        );
        assert_eq!(
            computed_work_trust_verdict(&work, &[fresh_pass]),
            WorkTrustVerdict::Partial
        );
        assert_eq!(
            computed_work_trust_verdict(&work, &[stale_pass]),
            WorkTrustVerdict::Stale
        );
        assert_eq!(
            computed_work_trust_verdict(&work, &[fail]),
            WorkTrustVerdict::Failed
        );
        assert_eq!(
            computed_work_trust_verdict(&work, &[]),
            WorkTrustVerdict::MissingEvidence
        );
    }

    #[test]
    fn summary_freshness_depends_on_material_revision_key() {
        let workspace_id = WorkspaceId::new();
        let work_id = WorkRecordId::new();
        let now = Utc::now();
        let fresh = WorkSummary {
            summary_id: WorkSummaryId::new(),
            work_id: work_id.clone(),
            workspace_id,
            kind: WorkSummaryKind::ReportSummary,
            audience: WorkSummaryAudience::Reviewer,
            text: "summary".to_string(),
            structured_json: None,
            generation_method: WorkSummaryGenerationMethod::Deterministic,
            provider: None,
            model: None,
            template: None,
            source_material_left_machine: false,
            freshness: WorkSummaryFreshness::Fresh,
            source_revision_key: Some("rev-a".to_string()),
            generated_at: now,
            created_at: now,
            updated_at: now,
            schema_version: AGENT_WORK_SCHEMA_VERSION,
        };
        let mut stale = fresh.clone();
        stale.source_revision_key = Some("rev-b".to_string());

        assert_eq!(
            aggregate_summary_freshness(&[fresh], "rev-a"),
            WorkSummaryFreshness::Fresh
        );
        assert_eq!(
            aggregate_summary_freshness(&[stale], "rev-a"),
            WorkSummaryFreshness::Stale
        );
    }

    #[test]
    fn markdown_report_escapes_untrusted_text() {
        let value = json!({
            "work": {
                "title": "<img src=x onerror=alert(1)> [link](javascript:alert(1))",
                "work_id": "wrk_test",
            },
            "trust": {
                "verdict": "partial",
                "recommended_next_action": "[rerun](javascript:alert(1))",
            },
            "evidence": [{
                "evidence_id": "wevdc_test",
                "status": "observed_pass",
                "freshness": "fresh",
                "claim": "<script>alert(1)</script> [bad](javascript:alert(1))",
            }],
        });
        let mut output = Vec::new();
        write_work_report_markdown(&value, &mut output).unwrap();
        let rendered = String::from_utf8(output).unwrap();

        assert!(!rendered.contains("<img"));
        assert!(!rendered.contains("<script>"));
        assert!(!rendered.contains("[bad](javascript:alert(1))"));
        assert!(rendered.contains("&lt;img"));
        assert!(rendered.contains("\\[bad\\]\\(javascript:alert\\(1\\)\\)"));
    }

    #[tokio::test]
    async fn schema_without_kind_lists_known_schemas() {
        let mut output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Schema(AgentWorkSchemaArgs { kind: None }),
            },
            &mut output,
        )
        .await
        .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("known ctx work schemas"));
        assert!(output.contains("work-bundle"));
        assert!(output.contains("agent-work"));
    }

    #[test]
    fn validate_accepts_structural_agent_work_json() {
        let value = json!({
            "change_sets": [
                {
                    "id": "cs-1",
                    "workspace_id": "ws-1",
                    "schema_version": 1
                }
            ],
            "contributions": [
                {
                    "id": "contrib-1",
                    "workspace_id": "ws-1",
                    "subject": {"kind": "session", "id": "session-1"},
                    "target": {"kind": "change-set", "id": "cs-1"},
                    "schema_version": 1
                }
            ]
        });

        validate_value(AgentWorkSchemaKind::AgentWork, &value).unwrap();
    }

    #[test]
    fn validate_accepts_agent_work_export_envelope() {
        let workspace_id = WorkspaceId::new();
        let value = json!({
            "kind": AGENT_WORK_EXPORT_ENVELOPE_KIND,
            "schema_version": 1,
            "agent_work_schema_version": 1,
            "provenance": {
                "source_kind": AGENT_WORK_EXPORT_SOURCE_KIND,
                "workspace_id": workspace_id.0.to_string(),
                "exported_at": "2026-01-01T00:00:00Z"
            },
            "redaction": {
                "profile": "full_local",
                "import_safe": true,
                "stats": {}
            },
            "agent_work": {
                "change_sets": [
                    {
                        "id": "cs-1",
                        "workspace_id": workspace_id.0.to_string(),
                        "schema_version": 1
                    }
                ],
                "contributions": []
            }
        });

        validate_value(AgentWorkSchemaKind::AgentWork, &value).unwrap();
        assert_eq!(
            infer_schema_kind(&value).unwrap(),
            AgentWorkSchemaKind::AgentWork
        );
    }

    #[test]
    fn validate_reports_invalid_json_from_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("invalid.json");
        std::fs::write(&path, "{not-json").unwrap();

        let error = read_json_file(&path).unwrap_err().to_string();

        assert!(error.contains("invalid JSON"));
    }

    #[test]
    fn validate_rejects_unknown_schema_version_structurally() {
        let value = json!({
            "change_sets": [
                {
                    "id": "cs-1",
                    "workspace_id": "ws-1",
                    "schema_version": 2
                }
            ],
            "contributions": []
        });

        let error = validate_value(AgentWorkSchemaKind::AgentWork, &value)
            .unwrap_err()
            .to_string();

        assert!(error.contains("schema_version must be 1"));
    }

    #[test]
    fn validate_rejects_invalid_agent_work_enum_value() {
        let value = json!({
            "change_sets": [
                {
                    "id": "cs-1",
                    "workspace_id": "ws-1",
                    "source": "future_source",
                    "schema_version": 1
                }
            ],
            "contributions": []
        });

        let error = validate_value(AgentWorkSchemaKind::AgentWork, &value)
            .unwrap_err()
            .to_string();

        assert!(error.contains("invalid enum value"), "{error}");
        assert!(error.contains("source"), "{error}");
    }

    #[test]
    fn validate_rejects_agent_work_extra_property() {
        let value = json!({
            "change_sets": [
                {
                    "id": "cs-1",
                    "workspace_id": "ws-1",
                    "unexpected": true,
                    "schema_version": 1
                }
            ],
            "contributions": []
        });

        let error = validate_value(AgentWorkSchemaKind::AgentWork, &value)
            .unwrap_err()
            .to_string();

        assert!(error.contains("unknown property `unexpected`"), "{error}");
    }

    #[test]
    fn validate_rejects_contribution_endpoint_missing_identity() {
        let value = json!({
            "id": "contrib-1",
            "workspace_id": "ws-1",
            "subject": {"kind": "task"},
            "target": {"kind": "system"},
            "schema_version": 1
        });

        let error = validate_value(AgentWorkSchemaKind::Contribution, &value)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("requires at least one identity field"),
            "{error}"
        );
        assert!(error.contains("$.subject"), "{error}");
    }

    #[test]
    fn validate_rejects_unknown_bundle_kind() {
        let value = json!({
            "kind": "ctx.work.future-bundle",
            "schema_version": 1,
            "objects": []
        });

        let error = infer_schema_kind(&value).unwrap_err().to_string();

        assert!(error.contains("unknown Work schema kind"));
    }

    #[test]
    fn validate_rejects_absolute_and_traversal_bundle_object_paths() {
        for path in [
            "/tmp/secret.json",
            "objects/../secret.json",
            "C:\\Users\\secret.json",
        ] {
            let value = json!({
                "kind": "ctx.work.bundle",
                "schema_version": 1,
                "objects": [{"path": path}]
            });

            let error = validate_value(AgentWorkSchemaKind::WorkBundle, &value)
                .unwrap_err()
                .to_string();

            assert!(
                error.contains("absolute path") || error.contains("traversal"),
                "unexpected error for {path}: {error}"
            );
        }
    }

    #[test]
    fn validate_rejects_invalid_plugin_manifest_structure() {
        let value = json!({
            "id": "example.invalid",
            "name": "Invalid",
            "version": "0.1.0",
            "entrypoints": [
                {
                    "id": "main"
                }
            ],
            "contributes": {
                "commands": [
                    {
                        "id": "example.invalid.open",
                        "entrypoint": "missing",
                        "unexpected": true
                    }
                ]
            }
        });

        let error = validate_value(AgentWorkSchemaKind::PluginManifest, &value)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("unexpected") || error.contains("plugin-manifest failed"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn validate_accepts_declarative_plugin_manifest_structure() {
        let value = json!({
            "schema_version": 1,
            "id": "example.agent-tools",
            "name": "Example Agent Tools",
            "version": "0.1.0",
            "entrypoints": [
                {
                    "id": "main",
                    "command": "node"
                }
            ],
            "contributes": {
                "commands": [
                    {
                        "id": "example.agent-tools.say_hello",
                        "title": "Say hello",
                        "entrypoint": "main"
                    }
                ],
                "templates": [
                    {
                        "id": "example.agent-tools.template",
                        "name": "Template",
                        "title": "Example template",
                        "template": "host.example-template"
                    }
                ],
                "toolbar_actions": [
                    {
                        "id": "example.agent-tools.say_hello_toolbar",
                        "name": "Say hello toolbar",
                        "title": "Say hello",
                        "command": "example.agent-tools.say_hello"
                    }
                ],
                "artifact_renderers": [
                    {
                        "id": "example.agent-tools.text_artifact",
                        "name": "Text artifact",
                        "artifact_types": ["text/plain"],
                        "renderer": "host.text-artifact"
                    }
                ],
                "card_renderers": [
                    {
                        "id": "example.agent-tools.work_summary_card",
                        "name": "Work summary card",
                        "card": "work.summary",
                        "renderer": "host.work-summary-card"
                    }
                ],
                "detail_sections": [
                    {
                        "id": "example.agent-tools.work_summary_section",
                        "name": "Work summary section",
                        "section": "work.summary",
                        "renderer": "host.work-summary-section"
                    }
                ],
                "review_sections": [
                    {
                        "id": "example.agent-tools.gate_state_section",
                        "name": "Gate state section",
                        "section": "review.gate-state",
                        "renderer": "host.gate-state-section"
                    }
                ]
            }
        });

        validate_value(AgentWorkSchemaKind::PluginManifest, &value).unwrap();
    }

    #[test]
    fn validate_rejects_plugin_manifest_unknown_entrypoint() {
        let value = json!({
            "id": "example.invalid",
            "name": "Invalid",
            "version": "0.1.0",
            "entrypoints": [
                {
                    "id": "main",
                    "command": "node"
                }
            ],
            "contributes": {
                "commands": [
                    {
                        "id": "example.invalid.open",
                        "title": "Open",
                        "entrypoint": "missing"
                    }
                ]
            }
        });

        let error = validate_value(AgentWorkSchemaKind::PluginManifest, &value)
            .unwrap_err()
            .to_string();

        assert!(error.contains("plugin-manifest failed structural validation"));
    }

    #[test]
    fn redaction_preview_omits_transcript_bodies_paths_and_secrets() {
        let value = json!({
            "record_type": "message",
            "role": "user",
            "content": "open /home/alice/project/.env with ghp_123456789012345678901234",
            "openai_api_key": "sk-12345678901234567890"
        });

        let preview = redaction_preview(&value);
        let text = serde_json::to_string(&preview.value).unwrap();

        assert!(text.contains("[omitted:transcript_body]"));
        assert!(!text.contains("/home/alice"));
        assert!(!text.contains("ghp_123456789012345678901234"));
        assert!(!text.contains("sk-12345678901234567890"));
        assert!(preview.stats.omitted_content_payloads >= 1);
        assert!(preview.stats.redacted_secret_fields >= 1);
    }

    #[test]
    fn redaction_preview_omits_transcript_like_event_payloads() {
        let value = json!({
            "seq": 1,
            "id": "event-1",
            "session_id": "session-1",
            "event_type": "assistant_chunk",
            "payload_json": {
                "content_fragment": "raw assistant text from /home/alice/project",
                "full_content": "complete raw answer"
            },
            "created_at": "2026-01-01T00:00:00Z"
        });

        let preview = redaction_preview(&value);
        let text = serde_json::to_string(&preview.value).unwrap();

        assert!(text.contains("[omitted:transcript_body]"));
        assert!(!text.contains("raw assistant text"));
        assert!(!text.contains("complete raw answer"));
        assert!(!text.contains("/home/alice"));
        assert!(preview.stats.omitted_content_payloads >= 1);
    }

    #[test]
    fn redaction_preview_omits_event_record_payload_json_with_content_keys() {
        let value = json!({
            "record_type": "event",
            "payload_json": {
                "delta": "secret transcript delta",
                "nested": {
                    "message": "nested raw message"
                }
            }
        });

        let preview = redaction_preview(&value);
        let text = serde_json::to_string(&preview.value).unwrap();

        assert!(text.contains("[omitted:transcript_body]"));
        assert!(!text.contains("secret transcript delta"));
        assert!(!text.contains("nested raw message"));
        assert!(preview.stats.omitted_content_payloads >= 1);
    }

    #[test]
    fn inspect_unknown_shape_reports_unknown_without_raw_secret_fields() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("unknown.json");
        let value = json!({
            "note": "misc local data",
            "openai_api_key": "sk-12345678901234567890"
        });
        let mut output = Vec::new();

        write_inspection(&path, &value, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("schema: unknown"));
        assert!(output.contains("raw secret-like fields: omitted"));
        assert!(!output.contains("sk-12345678901234567890"));
    }

    #[test]
    fn durable_diagnostics_escape_newlines_in_messages() {
        let diagnostic = durable_diagnostic(
            DiagnosticSeverity::Warning,
            "ctx.work.test",
            "first line\nsecond line",
        );

        assert!(diagnostic.contains("message: first line\\nsecond line"));
        assert!(!diagnostic.contains("message: first line\nsecond line"));
    }

    #[test]
    fn parse_nul_delimited_argv_preserves_secret_values_off_process_args() {
        let argv =
            parse_nul_delimited_argv(b"api\0-H\0Authorization: Bearer secret-token\0").unwrap();

        assert_eq!(
            argv,
            vec![
                "api".to_string(),
                "-H".to_string(),
                "Authorization: Bearer secret-token".to_string()
            ]
        );
    }

    #[test]
    fn write_json_file_refuses_to_overwrite_existing_output() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("work.json");
        write_json_file(&path, &json!({"ok": true})).unwrap();

        let error = write_json_file(&path, &json!({"ok": false}))
            .unwrap_err()
            .to_string();

        assert!(error.contains("refusing to overwrite"));
    }

    #[test]
    fn pull_request_parsing_accepts_urls_and_gh_repo_number_args() {
        let pr = parse_github_pull_request_url("https://github.com/ctxrs/ctx/pull/123").unwrap();
        assert_eq!(pr.provider, "github");
        assert_eq!(pr.owner, "ctxrs");
        assert_eq!(pr.repo, "ctx");
        assert_eq!(pr.number, 123);

        let argv = vec![
            "pr".to_string(),
            "view".to_string(),
            "456".to_string(),
            "--repo".to_string(),
            "ctxrs/ctx".to_string(),
        ];
        let pr = find_pull_request_ref(&argv).unwrap();
        assert_eq!(pr.owner, "ctxrs");
        assert_eq!(pr.repo, "ctx");
        assert_eq!(pr.number, 456);
    }

    #[test]
    fn pull_request_parsing_rejects_non_http_urls() {
        let error = parse_github_pull_request_url("javascript://github.com/ctxrs/ctx/pull/123")
            .unwrap_err()
            .to_string();

        assert!(error.contains("only http and https"), "{error}");
    }

    #[tokio::test]
    async fn capture_command_records_local_contribution_without_workspace_id() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let nested = repo.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let manager = StoreManager::open(data.path()).await.unwrap();
        let workspace = manager
            .global()
            .create_workspace(
                "repo".to_string(),
                repo.path()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();

        let mut output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Capture(AgentWorkCaptureArgs {
                    command: AgentWorkCaptureSubcommand::Command(AgentWorkCaptureCommandArgs {
                        store: store_args(data.path(), None),
                        tool: AgentWorkCaptureTool::Gh,
                        exit_code: 0,
                        cwd: Some(nested),
                        argv0_stdin: false,
                        argv: vec![
                            "pr".to_string(),
                            "view".to_string(),
                            "456".to_string(),
                            "--repo".to_string(),
                            "ctxrs/ctx".to_string(),
                        ],
                    }),
                }),
            },
            &mut output,
        )
        .await
        .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("captured:"));
        let store = manager.workspace(workspace.id).await.unwrap();
        let contributions = store
            .list_workspace_contributions(workspace.id)
            .await
            .unwrap();
        assert_eq!(contributions.len(), 1);
        let contribution = &contributions[0];
        assert!(matches!(
            contribution.target,
            ContributionEndpoint::PullRequest { .. }
        ));
        let metadata = contribution.metadata_json.as_ref().unwrap();
        assert_eq!(metadata["classification"], "gh.pr.view");
        assert_eq!(metadata["argv"][4], "ctxrs/ctx");
        assert_eq!(metadata["pull_request"]["number"], 456);
    }

    #[tokio::test]
    async fn capture_git_commit_links_current_head_deterministically() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        assert!(Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["config", "user.email", "ctx@example.invalid"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["config", "user.name", "ctx"])
            .status()
            .unwrap()
            .success());
        std::fs::write(repo.path().join("README.md"), "hello\n").unwrap();
        assert!(Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["add", "README.md"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["commit", "--quiet", "-m", "initial"])
            .status()
            .unwrap()
            .success());
        let head = git_output(repo.path(), &["rev-parse", "HEAD"]).unwrap();

        let manager = StoreManager::open(data.path()).await.unwrap();
        let workspace = manager
            .global()
            .create_workspace(
                "repo".to_string(),
                repo.path()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
        let store = manager.workspace(workspace.id).await.unwrap();

        for _ in 0..2 {
            run_with_writer(
                AgentWorkCommand {
                    command: AgentWorkSubcommand::Capture(AgentWorkCaptureArgs {
                        command: AgentWorkCaptureSubcommand::Command(AgentWorkCaptureCommandArgs {
                            store: store_args(data.path(), Some(workspace.id)),
                            tool: AgentWorkCaptureTool::Git,
                            exit_code: 0,
                            cwd: Some(repo.path().to_path_buf()),
                            argv0_stdin: false,
                            argv: vec![
                                "commit".to_string(),
                                "--quiet".to_string(),
                                "-m".to_string(),
                                "initial".to_string(),
                            ],
                        }),
                    }),
                },
                &mut Vec::new(),
            )
            .await
            .unwrap();
        }

        let work = store
            .find_work_record_by_link(workspace.id, WorkLinkTargetKind::Commit, &head)
            .await
            .unwrap()
            .expect("commit Work record");
        assert_eq!(work.head_commit.as_deref(), Some(head.as_str()));
        let links = store
            .list_work_record_links(workspace.id, work.work_id)
            .await
            .unwrap();
        let commit_links = links
            .iter()
            .filter(|link| {
                link.target_kind == WorkLinkTargetKind::Commit
                    && link.target_id.as_deref() == Some(head.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(commit_links.len(), 1);
        assert!(commit_links[0].link_id.0.starts_with("wln_"));
    }

    #[test]
    fn capture_command_denies_auth_and_secret_metadata() {
        let gh_auth = vec!["auth".to_string(), "token".to_string()];
        assert_eq!(
            captured_command_denial_reason(AgentWorkCaptureTool::Gh, &gh_auth),
            Some("arguments include auth or secret material")
        );

        let gh_secret = vec![
            "secret".to_string(),
            "set".to_string(),
            "API_KEY".to_string(),
        ];
        assert_eq!(
            captured_command_denial_reason(AgentWorkCaptureTool::Gh, &gh_secret),
            Some("arguments include auth or secret material")
        );

        let git_credential = vec!["credential".to_string(), "fill".to_string()];
        assert_eq!(
            captured_command_denial_reason(AgentWorkCaptureTool::Git, &git_credential),
            Some("git credential commands are not metadata-safe")
        );

        let git_config_auth = vec![
            "config".to_string(),
            "http.https://github.com/.extraHeader".to_string(),
            "AUTHORIZATION: bearer value".to_string(),
        ];
        assert_eq!(
            captured_command_denial_reason(AgentWorkCaptureTool::Git, &git_config_auth),
            Some("arguments include auth or secret material")
        );

        let gh_pr_view = vec![
            "pr".to_string(),
            "view".to_string(),
            "123".to_string(),
            "--repo".to_string(),
            "ctxrs/ctx".to_string(),
        ];
        assert_eq!(
            captured_command_denial_reason(AgentWorkCaptureTool::Gh, &gh_pr_view),
            None
        );
    }

    #[test]
    fn capture_command_requires_successful_allowlisted_metadata_commands() {
        let gh_pr_view = vec![
            "pr".to_string(),
            "view".to_string(),
            "123".to_string(),
            "--repo".to_string(),
            "ctxrs/ctx".to_string(),
        ];
        assert_eq!(
            captured_command_allowlist_denial_reason(AgentWorkCaptureTool::Gh, &gh_pr_view, 0),
            None
        );
        assert_eq!(
            captured_command_allowlist_denial_reason(AgentWorkCaptureTool::Gh, &gh_pr_view, 1),
            Some("only successful allowlisted metadata commands are captured")
        );

        let gh_issue_view = vec!["issue".to_string(), "view".to_string(), "123".to_string()];
        assert_eq!(
            captured_command_allowlist_denial_reason(AgentWorkCaptureTool::Gh, &gh_issue_view, 0),
            Some("gh command is not in the metadata capture allowlist")
        );

        let git_commit = vec!["-C".to_string(), "/repo".to_string(), "commit".to_string()];
        assert_eq!(git_subcommand(&git_commit), Some("commit"));
        assert_eq!(
            captured_command_allowlist_denial_reason(AgentWorkCaptureTool::Git, &git_commit, 0),
            None
        );

        let git_status = vec!["status".to_string(), "--short".to_string()];
        assert_eq!(
            captured_command_allowlist_denial_reason(AgentWorkCaptureTool::Git, &git_status, 0),
            Some("git command is not in the metadata capture allowlist")
        );
    }

    #[tokio::test]
    async fn link_pr_upserts_change_set_link_and_contribution_idempotently() {
        let (source_dir, workspace, change_set_id, _) = seeded_work_store().await;
        let store_args = store_args(source_dir.path(), Some(workspace.id));
        let url = "https://github.com/ctxrs/ctx/pull/789".to_string();

        for _ in 0..2 {
            run_with_writer(
                AgentWorkCommand {
                    command: AgentWorkSubcommand::LinkPr(AgentWorkLinkPrArgs {
                        store: store_args.clone(),
                        change_set_id: Some(change_set_id.0.clone()),
                        url: url.clone(),
                        title: Some("Work-first productization".to_string()),
                        state: Some("open".to_string()),
                        cwd: None,
                    }),
                },
                &mut Vec::new(),
            )
            .await
            .unwrap();
        }

        let manager = StoreManager::open(source_dir.path()).await.unwrap();
        let store = manager.workspace(workspace.id).await.unwrap();
        let change_set = store
            .get_workspace_change_set(workspace.id, change_set_id.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(change_set.pull_requests.len(), 1);
        assert_eq!(change_set.pull_requests[0].pull_request.number, 789);
        let pr_contributions = store
            .list_workspace_contributions(workspace.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|contribution| {
                contribution.change_set_id.as_ref() == Some(&change_set_id)
                    && matches!(
                        contribution.target,
                        ContributionEndpoint::PullRequest { .. }
                    )
            })
            .count();
        assert_eq!(pr_contributions, 1);
    }

    #[tokio::test]
    async fn link_pr_without_change_set_id_reuses_existing_pr_change_set() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let manager = StoreManager::open(data.path()).await.unwrap();
        let workspace = manager
            .global()
            .create_workspace(
                "repo".to_string(),
                repo.path()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
        let store_args = store_args(data.path(), Some(workspace.id));
        let url = "https://github.com/ctxrs/ctx/pull/321".to_string();

        for _ in 0..2 {
            run_with_writer(
                AgentWorkCommand {
                    command: AgentWorkSubcommand::LinkPr(AgentWorkLinkPrArgs {
                        store: store_args.clone(),
                        change_set_id: None,
                        url: url.clone(),
                        title: None,
                        state: None,
                        cwd: Some(repo.path().to_path_buf()),
                    }),
                },
                &mut Vec::new(),
            )
            .await
            .unwrap();
        }

        let store = manager.workspace(workspace.id).await.unwrap();
        let change_sets = store
            .list_workspace_change_sets(workspace.id)
            .await
            .unwrap();
        assert_eq!(change_sets.len(), 1);
        assert_eq!(change_sets[0].pull_requests.len(), 1);
        assert_eq!(change_sets[0].pull_requests[0].pull_request.number, 321);
    }

    #[tokio::test]
    async fn workspace_resolution_prefers_registered_root_containing_cwd() {
        let data = TempDir::new().unwrap();
        let root_a = TempDir::new().unwrap();
        let root_b = TempDir::new().unwrap();
        let nested_b = root_b.path().join("repo/subdir");
        std::fs::create_dir_all(&nested_b).unwrap();
        let manager = StoreManager::open(data.path()).await.unwrap();
        let workspace_a = manager
            .global()
            .create_workspace(
                "a".to_string(),
                root_a
                    .path()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
        let workspace_b = manager
            .global()
            .create_workspace(
                "b".to_string(),
                root_b
                    .path()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();

        let resolved = resolve_workspace(&manager, None, Some(&nested_b))
            .await
            .unwrap();

        assert_ne!(resolved.id, workspace_a.id);
        assert_eq!(resolved.id, workspace_b.id);
    }

    #[tokio::test]
    async fn list_show_export_and_import_round_trip_local_store() {
        let (source_dir, workspace, change_set_id, contribution_id) = seeded_work_store().await;
        let source_store = store_args(source_dir.path(), Some(workspace.id));

        let mut list_output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::List(AgentWorkListArgs {
                    store: source_store.clone(),
                    kind: AgentWorkRecordKind::All,
                    json: false,
                }),
            },
            &mut list_output,
        )
        .await
        .unwrap();
        let list_output = String::from_utf8(list_output).unwrap();
        assert!(list_output.contains(&change_set_id.0));
        assert!(list_output.contains(&contribution_id.0));
        assert!(list_output.contains("ctx.work.list.completed"));

        let mut list_json_output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::List(AgentWorkListArgs {
                    store: source_store.clone(),
                    kind: AgentWorkRecordKind::All,
                    json: true,
                }),
            },
            &mut list_json_output,
        )
        .await
        .unwrap();
        let list_json_text = String::from_utf8(list_json_output).unwrap();
        assert!(!list_json_text.contains("/tmp/test/private"));

        let mut show_output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Show(AgentWorkShowArgs {
                    store: source_store.clone(),
                    kind: None,
                    id: contribution_id.0.clone(),
                    json: true,
                }),
            },
            &mut show_output,
        )
        .await
        .unwrap();
        let show_json: Value = serde_json::from_slice(&show_output).unwrap();
        assert_eq!(show_json["id"], contribution_id.0);
        assert_eq!(show_json["workspace_id"], workspace.id.0.to_string());
        let show_json_text = serde_json::to_string(&show_json).unwrap();
        assert!(!show_json_text.contains("/tmp/test/private"));

        let manager = StoreManager::open(source_dir.path()).await.unwrap();
        let store = manager.workspace(workspace.id).await.unwrap();
        let work = test_work_record(workspace.id);
        let work_id = work.work_id.clone();
        store.upsert_work_record(&work).await.unwrap();
        let mut evidence = test_work_evidence(
            workspace.id,
            work_id.clone(),
            WorkEvidenceStatus::ObservedPass,
            WorkEvidenceFreshness::Fresh,
        );
        evidence.repo_root = Some("/tmp/test/private".to_string());
        store.upsert_work_evidence(&evidence).await.unwrap();

        let mut evidence_json_output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Evidence(AgentWorkEvidenceArgs {
                    store: source_store.clone(),
                    work_id: work_id.0.clone(),
                    command: AgentWorkEvidenceSubcommand::List(AgentWorkEvidenceListArgs {
                        json: true,
                    }),
                }),
            },
            &mut evidence_json_output,
        )
        .await
        .unwrap();
        let evidence_json_text = String::from_utf8(evidence_json_output).unwrap();
        assert!(!evidence_json_text.contains("/tmp/test/private"));

        let context = open_work_store(&source_store).await.unwrap();
        let report = build_work_report_value(&context, work_id.clone())
            .await
            .unwrap();
        let report_text = serde_json::to_string(&report).unwrap();
        assert!(!report_text.contains("/tmp/test/private"));
        assert!(!report_text.contains("payload_json"));

        let event = WorkEvent {
            event_id: WorkEventId::new(),
            work_id: work_id.clone(),
            workspace_id: workspace.id,
            sequence: 0,
            source_kind: Some("session".to_string()),
            source_id: Some("session-1".to_string()),
            event_type: WorkEventType::AssistantMessage,
            event_time: Utc::now(),
            actor_kind: WorkActorKind::Agent,
            provider: Some("codex".to_string()),
            harness: None,
            model: None,
            redaction_class: WorkRedactionClass::LocalRedacted,
            source: RecordSource::Session,
            fidelity: RecordFidelity::Summary,
            trust: RecordTrust::Low,
            payload_json: Some(json!({
                "content": "raw assistant body from /tmp/test/private",
                "openai_api_key": "sk-12345678901234567890"
            })),
            redacted_text: Some("safe event from /tmp/test/private".to_string()),
            artifact_ref: Some(json!({
                "absolute_path": "/tmp/test/private/output.log",
                "token": "sk-12345678901234567890"
            })),
            created_at: Utc::now(),
            schema_version: AGENT_WORK_SCHEMA_VERSION,
        };
        store.append_work_event(&event).await.unwrap();
        let report_with_raw_event = build_work_report_value(&context, work_id.clone())
            .await
            .unwrap();
        assert_eq!(
            report_with_raw_event["raw_transcript_available"],
            Value::Bool(true)
        );
        assert_eq!(
            report_with_raw_event["raw_transcript_included"],
            Value::Bool(false)
        );
        let report_with_raw_event_text = serde_json::to_string(&report_with_raw_event).unwrap();
        assert!(!report_with_raw_event_text.contains("payload_json"));
        assert!(!report_with_raw_event_text.contains("raw assistant body"));
        assert!(!report_with_raw_event_text.contains("/tmp/test/private"));

        let mut timeline_output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Timeline(AgentWorkTimelineArgs {
                    store: source_store.clone(),
                    work_id: work_id.0.clone(),
                    limit: 50,
                    json: true,
                }),
            },
            &mut timeline_output,
        )
        .await
        .unwrap();
        let timeline_text = String::from_utf8(timeline_output).unwrap();
        assert!(!timeline_text.contains("payload_json"));
        assert!(!timeline_text.contains("artifact_ref"));
        assert!(!timeline_text.contains("raw assistant body"));
        assert!(!timeline_text.contains("sk-12345678901234567890"));
        assert!(!timeline_text.contains("/tmp/test/private"));

        let export_path = source_dir.path().join("work-export.json");
        let mut export_output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Export(AgentWorkExportArgs {
                    store: source_store,
                    output: Some(export_path.clone()),
                    redaction_profile: AgentWorkRedactionProfile::FullLocal,
                }),
            },
            &mut export_output,
        )
        .await
        .unwrap();
        let exported = read_json_file(&export_path).unwrap();
        validate_value(AgentWorkSchemaKind::AgentWork, &exported).unwrap();
        assert_eq!(exported["kind"], AGENT_WORK_EXPORT_ENVELOPE_KIND);
        assert_eq!(exported["schema_version"], 1);
        assert_eq!(exported["agent_work_schema_version"], 1);
        assert_eq!(
            exported["provenance"]["source_kind"],
            AGENT_WORK_EXPORT_SOURCE_KIND
        );
        assert_eq!(
            exported["provenance"]["workspace_id"],
            workspace.id.0.to_string()
        );
        assert_eq!(exported["redaction"]["profile"], "full_local");
        assert_eq!(exported["redaction"]["import_safe"], true);
        assert!(exported["redaction"]["stats"].is_object());
        assert_eq!(
            exported["agent_work"]["change_sets"][0]["id"],
            change_set_id.0
        );
        assert_eq!(
            exported["agent_work"]["contributions"][0]["id"],
            contribution_id.0
        );

        let target_dir = TempDir::new().unwrap();
        let target_manager = StoreManager::open(target_dir.path()).await.unwrap();
        target_manager
            .global()
            .upsert_workspace(&workspace)
            .await
            .unwrap();
        let mut import_output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Import(AgentWorkImportArgs {
                    store: store_args(target_dir.path(), Some(workspace.id)),
                    file: export_path,
                    dry_run: false,
                }),
            },
            &mut import_output,
        )
        .await
        .unwrap();
        let import_output = String::from_utf8(import_output).unwrap();
        assert!(import_output.contains("imported 1 change sets and 1 contributions"));
        assert!(import_output.contains("hosted/team enforcement state is not imported"));

        let target_store = target_manager.workspace(workspace.id).await.unwrap();
        assert!(target_store
            .get_workspace_change_set(workspace.id, change_set_id)
            .await
            .unwrap()
            .is_some());
        assert!(target_store
            .get_contribution(contribution_id)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn default_safe_summary_export_is_not_import_safe() {
        let (source_dir, workspace, _, _) = seeded_work_store().await;
        let export_path = source_dir.path().join("work-export-safe-summary.json");
        let mut export_output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Export(AgentWorkExportArgs {
                    store: store_args(source_dir.path(), Some(workspace.id)),
                    output: Some(export_path.clone()),
                    redaction_profile: AgentWorkRedactionProfile::SafeSummary,
                }),
            },
            &mut export_output,
        )
        .await
        .unwrap();

        let exported = read_json_file(&export_path).unwrap();
        validate_value(AgentWorkSchemaKind::AgentWork, &exported).unwrap();
        assert_eq!(exported["kind"], AGENT_WORK_EXPORT_ENVELOPE_KIND);
        assert_eq!(exported["redaction"]["profile"], "safe_summary");
        assert_eq!(exported["redaction"]["import_safe"], false);
        assert!(exported["redaction"]["stats"].is_object());
        assert_eq!(
            exported["agent_work"]["change_sets"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            exported["agent_work"]["contributions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let target_dir = TempDir::new().unwrap();
        let target_manager = StoreManager::open(target_dir.path()).await.unwrap();
        target_manager
            .global()
            .upsert_workspace(&workspace)
            .await
            .unwrap();
        let mut import_output = Vec::new();
        let error = run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Import(AgentWorkImportArgs {
                    store: store_args(target_dir.path(), Some(workspace.id)),
                    file: export_path,
                    dry_run: false,
                }),
            },
            &mut import_output,
        )
        .await
        .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("not marked import_safe"), "{error}");

        let target_store = target_manager.workspace(workspace.id).await.unwrap();
        assert!(target_store
            .list_workspace_change_sets(workspace.id)
            .await
            .unwrap()
            .is_empty());
        assert!(target_store
            .list_workspace_contributions(workspace.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn import_accepts_safe_summary_envelope_when_marked_import_safe() {
        let (source_dir, workspace, change_set_id, contribution_id) = seeded_work_store().await;
        let export_path = source_dir.path().join("work-export-import-safe.json");
        let mut export_output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Export(AgentWorkExportArgs {
                    store: store_args(source_dir.path(), Some(workspace.id)),
                    output: Some(export_path.clone()),
                    redaction_profile: AgentWorkRedactionProfile::SafeSummary,
                }),
            },
            &mut export_output,
        )
        .await
        .unwrap();

        let mut exported = read_json_file(&export_path).unwrap();
        exported["redaction"]["import_safe"] = json!(true);
        std::fs::write(&export_path, serde_json::to_vec_pretty(&exported).unwrap()).unwrap();

        let target_dir = TempDir::new().unwrap();
        let target_manager = StoreManager::open(target_dir.path()).await.unwrap();
        target_manager
            .global()
            .upsert_workspace(&workspace)
            .await
            .unwrap();
        let mut import_output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Import(AgentWorkImportArgs {
                    store: store_args(target_dir.path(), Some(workspace.id)),
                    file: export_path,
                    dry_run: false,
                }),
            },
            &mut import_output,
        )
        .await
        .unwrap();

        let target_store = target_manager.workspace(workspace.id).await.unwrap();
        assert!(target_store
            .get_workspace_change_set(workspace.id, change_set_id)
            .await
            .unwrap()
            .is_some());
        assert!(target_store
            .get_contribution(contribution_id)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn import_rejects_redacted_legacy_export_without_writing() {
        let temp = TempDir::new().unwrap();
        let manager = StoreManager::open(temp.path()).await.unwrap();
        let workspace = manager
            .global()
            .create_workspace(
                "target".to_string(),
                "/tmp/target".to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
        let mut change_set = test_change_set(workspace.id, ChangeSetId::new());
        change_set.title = Some("safe summary [redacted:secret]".to_string());
        let bundle = AgentWorkExport {
            change_sets: vec![change_set],
            contributions: Vec::new(),
        };
        let path = temp.path().join("legacy-redacted.json");
        write_json_file(&path, &serde_json::to_value(bundle).unwrap()).unwrap();

        let mut output = Vec::new();
        let error = run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Import(AgentWorkImportArgs {
                    store: store_args(temp.path(), Some(workspace.id)),
                    file: path,
                    dry_run: false,
                }),
            },
            &mut output,
        )
        .await
        .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("redaction markers"), "{error}");
        assert!(manager
            .workspace(workspace.id)
            .await
            .unwrap()
            .list_workspace_change_sets(workspace.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn import_rejects_workspace_mismatch_without_writing() {
        let temp = TempDir::new().unwrap();
        let manager = StoreManager::open(temp.path()).await.unwrap();
        let workspace = manager
            .global()
            .create_workspace(
                "target".to_string(),
                "/tmp/target".to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
        let other_workspace_id = WorkspaceId::new();
        let bundle = AgentWorkExport {
            change_sets: vec![test_change_set(other_workspace_id, ChangeSetId::new())],
            contributions: Vec::new(),
        };
        let path = temp.path().join("mismatch.json");
        write_json_file(&path, &serde_json::to_value(bundle).unwrap()).unwrap();

        let mut output = Vec::new();
        let error = run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Import(AgentWorkImportArgs {
                    store: store_args(temp.path(), Some(workspace.id)),
                    file: path,
                    dry_run: false,
                }),
            },
            &mut output,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("selected workspace"));
        assert!(manager
            .workspace(workspace.id)
            .await
            .unwrap()
            .list_workspace_change_sets(workspace.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn import_dry_run_validates_relations_without_writing() {
        let temp = TempDir::new().unwrap();
        let manager = StoreManager::open(temp.path()).await.unwrap();
        let workspace = manager
            .global()
            .create_workspace(
                "target".to_string(),
                "/tmp/target".to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
        let store = manager.workspace(workspace.id).await.unwrap();
        let change_set_id = ChangeSetId::new();
        let change_set = test_change_set(workspace.id, change_set_id.clone());
        let mut bad_contribution =
            test_contribution(workspace.id, change_set_id.clone(), ContributionId::new());
        bad_contribution.subject = ContributionEndpoint::Task {
            task_id: Some(TaskId::new()),
            id: None,
        };
        let bundle = AgentWorkExport {
            change_sets: vec![change_set],
            contributions: vec![bad_contribution],
        };
        let path = temp.path().join("dry-run-relations.json");
        write_json_file(&path, &serde_json::to_value(bundle).unwrap()).unwrap();

        let mut output = Vec::new();
        let error = run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Import(AgentWorkImportArgs {
                    store: store_args(temp.path(), Some(workspace.id)),
                    file: path,
                    dry_run: true,
                }),
            },
            &mut output,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("task does not exist"));
        assert!(store
            .get_workspace_change_set(workspace.id, change_set_id)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .list_workspace_contributions(workspace.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn evidence_run_stores_share_safe_bounded_output_previews() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let script = repo.path().join("emit-evidence.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf 'stdout-needle-not-indexed\\nGITHUB_TOKEN=ghp_123456789012345678901234\\n{}\\n{}\\nD:\\\\private\\\\output.log\\n\\\\\\\\buildshare\\\\private\\\\output.log\\n'\nprintf 'stderr-needle-not-indexed\\nOPENAI_API_KEY=sk-12345678901234567890\\n' >&2\n",
                "A".repeat(COMMAND_OUTPUT_PREVIEW_LIMIT + 256),
                repo.path().join("private/output.log").display(),
            ),
        )
        .unwrap();
        let status = Command::new("chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .unwrap();
        assert!(status.success());

        let manager = StoreManager::open(data.path()).await.unwrap();
        let workspace = manager
            .global()
            .create_workspace(
                "repo".to_string(),
                repo.path()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
        let store = manager.workspace(workspace.id).await.unwrap();
        let work = test_work_record(workspace.id);
        let work_id = work.work_id.clone();
        store.upsert_work_record(&work).await.unwrap();

        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Evidence(AgentWorkEvidenceArgs {
                    store: store_args(data.path(), Some(workspace.id)),
                    work_id: work_id.0.clone(),
                    command: AgentWorkEvidenceSubcommand::Run(AgentWorkEvidenceRunArgs {
                        kind: AgentWorkEvidenceKindArg::Test,
                        cwd: Some(repo.path().to_path_buf()),
                        command: vec![script.to_string_lossy().to_string()],
                    }),
                }),
            },
            &mut Vec::new(),
        )
        .await
        .unwrap();

        let evidence = store
            .list_work_evidence(workspace.id, work_id.clone())
            .await
            .unwrap();
        assert_eq!(evidence.len(), 1);
        let output_ref = evidence[0].output_ref.as_ref().unwrap();
        assert_eq!(output_ref["kind"], "ctx.work.command_output_preview");
        assert_eq!(output_ref["share_safe"], true);
        assert_eq!(output_ref["raw_stdout_retained"], false);
        assert_eq!(output_ref["raw_stderr_retained"], false);
        assert_eq!(
            output_ref["preview_limit_bytes"],
            COMMAND_OUTPUT_PREVIEW_LIMIT
        );
        assert!(output_ref["stdout_size_bytes"].as_u64().unwrap() > 0);
        assert!(output_ref["stderr_size_bytes"].as_u64().unwrap() > 0);
        assert_eq!(output_ref["stdout_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(output_ref["stderr_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(output_ref["stdout_truncated"], true);
        assert_eq!(output_ref["truncated"], true);

        let serialized = serde_json::to_string(output_ref).unwrap();
        assert!(serialized.contains("stdout-needle-not-indexed"));
        assert!(serialized.contains("[truncated]"));
        assert!(!serialized.contains("ghp_123456789012345678901234"));
        assert!(!serialized.contains("sk-12345678901234567890"));
        assert!(!serialized.contains(repo.path().to_string_lossy().as_ref()));
        assert!(!serialized.contains("D:\\\\private"));
        assert!(!serialized.contains("\\\\\\\\buildshare"));
        assert!(serialized.len() < COMMAND_OUTPUT_PREVIEW_LIMIT * 3);

        let output_hits = store
            .search_work_docs(
                workspace.id,
                WorkSearchQuery {
                    text: Some("stdout-needle-not-indexed".to_string()),
                    ..WorkSearchQuery::default()
                },
            )
            .await
            .unwrap();
        assert!(output_hits.is_empty());

        let command_hits = store
            .search_work_docs(
                workspace.id,
                WorkSearchQuery {
                    text: Some("emit-evidence".to_string()),
                    ..WorkSearchQuery::default()
                },
            )
            .await
            .unwrap();
        assert!(!command_hits.is_empty());
    }

    #[tokio::test]
    async fn project_session_command_backfills_ade_session_work() {
        let data = TempDir::new().unwrap();
        let manager = StoreManager::open(data.path()).await.unwrap();
        let workspace = manager
            .global()
            .create_workspace(
                "projector".to_string(),
                "/tmp/projector".to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
        let store = manager.workspace(workspace.id).await.unwrap();
        let task = store
            .create_task(
                workspace.id,
                "Project Work Inspector capture".to_string(),
                Some("Make the ADE session visible in Work".to_string()),
            )
            .await
            .unwrap();
        let worktree = store
            .create_worktree(
                workspace.id,
                "/tmp/projector".to_string(),
                "abc123".to_string(),
                Some("ctx/projector".to_string()),
            )
            .await
            .unwrap();
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ctx_core::models::ExecutionEnvironment::Host,
                "codex".to_string(),
                "gpt-5".to_string(),
                "implementer".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .append_session_event(
                session.id,
                None,
                None,
                ctx_core::models::SessionEventType::UserMessage,
                json!({"content": "please project this session"}),
            )
            .await
            .unwrap();

        let mut output = Vec::new();
        run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Project(AgentWorkProjectArgs {
                    store: store_args(data.path(), Some(workspace.id)),
                    command: AgentWorkProjectSubcommand::Session(AgentWorkProjectSessionArgs {
                        session_id: session.id.0.to_string(),
                        json: true,
                    }),
                }),
            },
            &mut output,
        )
        .await
        .unwrap();

        let output = String::from_utf8(output).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["source_kind"], "session");
        assert_eq!(value["links"], 3);
        assert!(value["events"].as_u64().unwrap() >= 2);

        let work = store
            .find_work_record_by_link(
                workspace.id,
                WorkLinkTargetKind::Session,
                &session.id.0.to_string(),
            )
            .await
            .unwrap()
            .expect("projected work record");
        let transcript = store
            .list_work_events(workspace.id, work.work_id, None)
            .await
            .unwrap();
        assert!(transcript.iter().any(|event| {
            event.event_type == WorkEventType::UserMessage
                && event
                    .redacted_text
                    .as_deref()
                    .is_some_and(|text| text.contains("please project this session"))
        }));
    }

    #[tokio::test]
    async fn import_rolls_back_change_set_update_when_later_contribution_fails() {
        let temp = TempDir::new().unwrap();
        let manager = StoreManager::open(temp.path()).await.unwrap();
        let workspace = manager
            .global()
            .create_workspace(
                "target".to_string(),
                "/tmp/target".to_string(),
                VcsKind::Git,
            )
            .await
            .unwrap();
        let store = manager.workspace(workspace.id).await.unwrap();
        let change_set_id = ChangeSetId::new();
        store
            .upsert_change_set(&test_change_set(workspace.id, change_set_id.clone()))
            .await
            .unwrap();

        let mut replacement = test_change_set(workspace.id, change_set_id.clone());
        replacement.title = Some("Replacement should roll back".to_string());
        let mut bad_contribution =
            test_contribution(workspace.id, change_set_id.clone(), ContributionId::new());
        bad_contribution.subject = ContributionEndpoint::Task {
            task_id: Some(TaskId::new()),
            id: None,
        };
        let bundle = AgentWorkExport {
            change_sets: vec![replacement],
            contributions: vec![bad_contribution],
        };
        let path = temp.path().join("rollback.json");
        write_json_file(&path, &serde_json::to_value(bundle).unwrap()).unwrap();

        let mut output = Vec::new();
        let error = run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Import(AgentWorkImportArgs {
                    store: store_args(temp.path(), Some(workspace.id)),
                    file: path,
                    dry_run: false,
                }),
            },
            &mut output,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("task does not exist"));
        let stored = store
            .get_workspace_change_set(workspace.id, change_set_id)
            .await
            .unwrap()
            .expect("original change set should remain");
        assert_eq!(stored.title.as_deref(), Some("Test change set"));
        assert!(store
            .list_workspace_contributions(workspace.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn aggregate_evidence_refresh_preserves_failed_trust_after_later_pass() {
        let (source_dir, workspace, _, _) = seeded_work_store().await;
        let manager = StoreManager::open(source_dir.path()).await.unwrap();
        let store = manager.workspace(workspace.id).await.unwrap();
        let mut work = test_work_record(workspace.id);
        work.trust_verdict = WorkTrustVerdict::MissingEvidence;
        let work_id = work.work_id.clone();
        store.upsert_work_record(&work).await.unwrap();
        let failed = test_work_evidence(
            workspace.id,
            work_id.clone(),
            WorkEvidenceStatus::ObservedFail,
            WorkEvidenceFreshness::Fresh,
        );
        let passed = test_work_evidence(
            workspace.id,
            work_id.clone(),
            WorkEvidenceStatus::ObservedPass,
            WorkEvidenceFreshness::Fresh,
        );
        store.upsert_work_evidence(&failed).await.unwrap();
        store.upsert_work_evidence(&passed).await.unwrap();
        let evidence = store
            .list_work_evidence(workspace.id, work_id.clone())
            .await
            .unwrap();

        refresh_work_trust_from_evidence_set(&store, workspace.id, &work_id, &evidence)
            .await
            .unwrap();

        let stored = store
            .get_workspace_work_record(workspace.id, work_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.trust_verdict, WorkTrustVerdict::Failed);
    }

    fn store_args(data_dir: &Path, workspace_id: Option<WorkspaceId>) -> AgentWorkStoreArgs {
        AgentWorkStoreArgs {
            data_dir: Some(data_dir.to_path_buf()),
            workspace_id: workspace_id.map(|id| id.0.to_string()),
        }
    }

    async fn seeded_work_store() -> (TempDir, Workspace, ChangeSetId, ContributionId) {
        let temp = TempDir::new().unwrap();
        let manager = StoreManager::open(temp.path()).await.unwrap();
        let workspace = manager
            .global()
            .create_workspace("test".to_string(), "/tmp/test".to_string(), VcsKind::Git)
            .await
            .unwrap();
        let store = manager.workspace(workspace.id).await.unwrap();
        let change_set_id = ChangeSetId::new();
        let contribution_id = ContributionId::new();
        store
            .upsert_change_set(&test_change_set(workspace.id, change_set_id.clone()))
            .await
            .unwrap();
        store
            .upsert_contribution(&test_contribution(
                workspace.id,
                change_set_id.clone(),
                contribution_id.clone(),
            ))
            .await
            .unwrap();
        (temp, workspace, change_set_id, contribution_id)
    }

    fn test_change_set(workspace_id: WorkspaceId, id: ChangeSetId) -> ChangeSet {
        ChangeSet {
            id,
            workspace_id,
            source_worktree_id: None,
            source: RecordSource::Manual,
            origin: RecordOrigin::User,
            fidelity: RecordFidelity::Declared,
            trust: Default::default(),
            title: Some("Test change set".to_string()),
            summary: None,
            description: None,
            fingerprint: None,
            base_revision: None,
            head_revision: None,
            target_branch: Some("main".to_string()),
            pull_requests: Vec::new(),
            source_records: Vec::new(),
            issuer: None,
            created_at: None,
            updated_at: None,
            schema_version: 1,
        }
    }

    fn test_work_record(workspace_id: WorkspaceId) -> WorkRecord {
        let now = Utc::now();
        WorkRecord {
            work_id: WorkRecordId::new(),
            workspace_id,
            title: Some("Test Work".to_string()),
            objective: None,
            lifecycle: WorkLifecycle::Active,
            primary_repo_root: None,
            primary_branch: Some("main".to_string()),
            base_commit: None,
            head_commit: Some("abc123".to_string()),
            current_diff_fingerprint: None,
            trust_verdict: WorkTrustVerdict::UntrustedLocalCapture,
            summary_freshness: WorkSummaryFreshness::Missing,
            metadata_json: Some(json!({
                "cwd": "/tmp/test/private",
                "safe": "kept"
            })),
            created_at: now,
            updated_at: now,
            schema_version: AGENT_WORK_SCHEMA_VERSION,
        }
    }

    fn test_work_evidence(
        workspace_id: WorkspaceId,
        work_id: WorkRecordId,
        status: WorkEvidenceStatus,
        freshness: WorkEvidenceFreshness,
    ) -> WorkEvidence {
        let now = Utc::now();
        WorkEvidence {
            evidence_id: WorkEvidenceId::new(),
            work_id,
            workspace_id,
            kind: WorkEvidenceKind::Test,
            status,
            freshness,
            claim: Some("Observed test command".to_string()),
            command: Some("cargo test".to_string()),
            argv: vec!["cargo".to_string(), "test".to_string()],
            cwd: None,
            exit_code: Some(if status == WorkEvidenceStatus::ObservedFail {
                1
            } else {
                0
            }),
            repo_root: None,
            head_sha: None,
            branch: None,
            fingerprint: None,
            current_fingerprint: None,
            output_ref: None,
            artifact_ref: None,
            source: RecordSource::Worktree,
            fidelity: RecordFidelity::Exact,
            trust: Default::default(),
            started_at: now,
            finished_at: now,
            created_at: now,
            updated_at: now,
            schema_version: AGENT_WORK_SCHEMA_VERSION,
        }
    }

    fn test_contribution(
        workspace_id: WorkspaceId,
        change_set_id: ChangeSetId,
        id: ContributionId,
    ) -> Contribution {
        Contribution {
            id,
            workspace_id,
            change_set_id: Some(change_set_id.clone()),
            subject: ContributionEndpoint::External {
                source: "test".to_string(),
                identifier: Some("task-1".to_string()),
                url: None,
            },
            target: ContributionEndpoint::ChangeSet { change_set_id },
            role: ContributionRole::Related,
            source: RecordSource::Manual,
            origin: RecordOrigin::User,
            fidelity: RecordFidelity::Declared,
            trust: Default::default(),
            summary: Some("Test contribution".to_string()),
            fingerprint: None,
            issuer: None,
            metadata_json: Some(json!({
                "cwd": "/tmp/test/private",
                "safe": "kept"
            })),
            source_records: Vec::new(),
            created_at: None,
            updated_at: None,
            schema_version: 1,
        }
    }

    fn init_git_repo(path: &Path) {
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

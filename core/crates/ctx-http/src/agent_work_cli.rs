use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Args, Subcommand, ValueEnum};
use ctx_core::ids::{ChangeSetId, ContributionId, WorkspaceId};
use ctx_core::models::PluginManifest;
use ctx_core::models::{ChangeSet, Contribution};
use ctx_store::{Store, StoreManager};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// Capture local Work records.
    Capture(AgentWorkStoreArgs),
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

#[derive(Debug, Clone, Copy, ValueEnum)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AgentWorkRedactionProfile {
    /// Redact obvious secrets, host paths, and transcript-like payloads.
    SafeSummary,
    /// Preserve full local records. Use only for trusted local imports/exports.
    FullLocal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentWorkExport {
    change_sets: Vec<ChangeSet>,
    contributions: Vec<Contribution>,
}

impl AgentWorkRecordKind {
    fn includes_change_sets(self) -> bool {
        matches!(self, Self::All | Self::ChangeSet)
    }

    fn includes_contributions(self) -> bool {
        matches!(self, Self::All | Self::Contribution)
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
                "ok: {} matches {} (structural validation; JSON Schema constraints are not fully evaluated in this slice)",
                args.file.display(),
                kind.as_str()
            )?;
            write_diagnostic(
                writer,
                DiagnosticSeverity::Info,
                "ctx.work.validate.ok",
                &format!("{} passed local structural validation", args.file.display()),
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
        AgentWorkSubcommand::Capture(_args) => {
            not_implemented("capture")?;
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

fn not_implemented(command: &str) -> Result<()> {
    bail!(
        "{}",
        durable_diagnostic(
            DiagnosticSeverity::Error,
            &format!("ctx.work.{command}.not_implemented"),
            &format!(
                "ctx work {command} is not implemented in this local CLI slice yet; use `ctx work schema`, `ctx work validate`, `ctx work inspect`, or `ctx work redaction-preview` for local schema and bundle checks"
            ),
        )
    )
}

async fn list_work_records(args: AgentWorkListArgs, writer: &mut dyn Write) -> Result<()> {
    let context = open_work_store(&args.store).await?;
    let bundle = load_work_export(&context.store, context.workspace_id).await?;

    if args.json {
        let value = filtered_export_value(&bundle, args.kind)?;
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
    let mut value = serde_json::to_value(&bundle).context("serializing Work export")?;
    validate_value(AgentWorkSchemaKind::AgentWork, &value)
        .context("generated Work export failed local validation")?;
    if args.redaction_profile == AgentWorkRedactionProfile::SafeSummary {
        value = redaction_preview(&value).value;
    }

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
                "exported Work records from {} for workspace {} with {:?} redaction",
                context.data_root.display(),
                context.workspace_id.0,
                args.redaction_profile
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
    validate_value(AgentWorkSchemaKind::AgentWork, &value).with_context(|| {
        durable_diagnostic(
            DiagnosticSeverity::Error,
            "ctx.work.import.invalid_agent_work",
            &format!(
                "{} is not a valid local AgentWork export",
                args.file.display()
            ),
        )
    })?;
    let bundle: AgentWorkExport =
        serde_json::from_value(value).context("decoding local AgentWork export")?;
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
    store: Store,
}

async fn open_work_store(args: &AgentWorkStoreArgs) -> Result<WorkStoreContext> {
    let data_root = resolve_data_root(args.data_dir.as_deref())?;
    let manager = StoreManager::open(&data_root)
        .await
        .with_context(|| format!("opening ctx store at {}", data_root.display()))?;
    let workspace_id = resolve_workspace_id(&manager, args.workspace_id.as_deref()).await?;
    let store = manager
        .workspace(workspace_id)
        .await
        .with_context(|| format!("opening workspace store {}", workspace_id.0))?;
    Ok(WorkStoreContext {
        data_root,
        workspace_id,
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

async fn resolve_workspace_id(
    manager: &StoreManager,
    workspace_id: Option<&str>,
) -> Result<WorkspaceId> {
    if let Some(workspace_id) = workspace_id {
        return parse_workspace_id(workspace_id);
    }

    let workspaces = manager
        .global()
        .list_workspaces()
        .await
        .context("listing local ctx workspaces")?;
    match workspaces.as_slice() {
        [workspace] => Ok(workspace.id),
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

fn filtered_export_value(bundle: &AgentWorkExport, kind: AgentWorkRecordKind) -> Result<Value> {
    let filtered = AgentWorkExport {
        change_sets: if kind.includes_change_sets() {
            bundle.change_sets.clone()
        } else {
            Vec::new()
        },
        contributions: if kind.includes_contributions() {
            bundle.contributions.clone()
        } else {
            Vec::new()
        },
    };
    serde_json::to_value(filtered).context("serializing filtered Work records")
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
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
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

    if object.contains_key("change_sets") || object.contains_key("contributions") {
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

fn validate_agent_work(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .context("agent-work must be a JSON object")?;
    let change_sets = object
        .get("change_sets")
        .and_then(Value::as_array)
        .context("agent-work requires `change_sets` array")?;
    let contributions = object
        .get("contributions")
        .and_then(Value::as_array)
        .context("agent-work requires `contributions` array")?;

    for (index, change_set) in change_sets.iter().enumerate() {
        validate_change_set(change_set, &format!("$.change_sets[{index}]"))?;
    }
    for (index, contribution) in contributions.iter().enumerate() {
        validate_contribution(contribution, &format!("$.contributions[{index}]"))?;
    }
    Ok(())
}

fn validate_change_set(value: &Value, path: &str) -> Result<()> {
    validate_required_fields(value, path, &["id", "workspace_id"])?;
    validate_schema_version(value, path)
}

fn validate_contribution(value: &Value, path: &str) -> Result<()> {
    validate_required_fields(value, path, &["id", "workspace_id", "subject", "target"])?;
    validate_schema_version(value, path)
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
    reject_plugin_manifest_unknown_properties(value)?;
    let manifest: PluginManifest =
        serde_json::from_value(value.clone()).context("plugin-manifest failed to deserialize")?;
    manifest
        .validate()
        .map_err(|error| anyhow::anyhow!("plugin-manifest failed structural validation: {error:?}"))
}

fn reject_plugin_manifest_unknown_properties(value: &Value) -> Result<()> {
    validate_allowed_object_keys(
        value,
        "$",
        &[
            "schema_version",
            "id",
            "name",
            "version",
            "description",
            "entrypoints",
            "contributes",
            "compatibility",
        ],
    )?;

    if let Some(entrypoints) = value.get("entrypoints").and_then(Value::as_array) {
        for (index, entrypoint) in entrypoints.iter().enumerate() {
            validate_allowed_object_keys(
                entrypoint,
                &format!("$.entrypoints[{index}]"),
                &["id", "kind", "command", "args", "cwd", "environment"],
            )?;
        }
    }

    if let Some(contributes) = value.get("contributes") {
        validate_allowed_object_keys(
            contributes,
            "$.contributes",
            &[
                "providers",
                "runtimes",
                "commands",
                "collectors",
                "observers",
                "ui_surfaces",
            ],
        )?;
        validate_plugin_named_contribution_keys(contributes, "providers", &["capabilities"])?;
        validate_plugin_named_contribution_keys(contributes, "runtimes", &["capabilities"])?;
        validate_plugin_command_contribution_keys(contributes)?;
        validate_plugin_named_contribution_keys(contributes, "collectors", &["events"])?;
        validate_plugin_named_contribution_keys(contributes, "observers", &["events"])?;
        validate_plugin_named_contribution_keys(
            contributes,
            "ui_surfaces",
            &["surface", "contexts"],
        )?;
    }

    if let Some(compatibility) = value.get("compatibility") {
        validate_allowed_object_keys(
            compatibility,
            "$.compatibility",
            &["min_ctx_version", "capabilities"],
        )?;
    }

    Ok(())
}

fn validate_plugin_named_contribution_keys(
    contributes: &Value,
    field: &str,
    extra_allowed_keys: &[&str],
) -> Result<()> {
    let Some(contributions) = contributes.get(field).and_then(Value::as_array) else {
        return Ok(());
    };
    let mut allowed = vec!["id", "name", "description", "entrypoint"];
    allowed.extend_from_slice(extra_allowed_keys);
    for (index, contribution) in contributions.iter().enumerate() {
        validate_allowed_object_keys(
            contribution,
            &format!("$.contributes.{field}[{index}]"),
            &allowed,
        )?;
    }
    Ok(())
}

fn validate_plugin_command_contribution_keys(contributes: &Value) -> Result<()> {
    let Some(commands) = contributes.get("commands").and_then(Value::as_array) else {
        return Ok(());
    };
    for (index, command) in commands.iter().enumerate() {
        validate_allowed_object_keys(
            command,
            &format!("$.contributes.commands[{index}]"),
            &["id", "title", "description", "category", "entrypoint"],
        )?;
    }
    Ok(())
}

fn validate_allowed_object_keys(value: &Value, path: &str, allowed_keys: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .with_context(|| format!("{path} must be a JSON object"))?;
    for key in object.keys() {
        if !allowed_keys.contains(&key.as_str()) {
            bail!("{path}.{key} is not part of the plugin-manifest schema");
        }
    }
    Ok(())
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
            writeln!(
                writer,
                "change_sets: {}",
                value
                    .get("change_sets")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            )?;
            writeln!(
                writer,
                "contributions: {}",
                value
                    .get("contributions")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            )?;
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

    #[tokio::test]
    async fn capture_returns_actionable_not_implemented_diagnostic() {
        let mut output = Vec::new();
        let error = run_with_writer(
            AgentWorkCommand {
                command: AgentWorkSubcommand::Capture(AgentWorkStoreArgs {
                    data_dir: None,
                    workspace_id: None,
                }),
            },
            &mut output,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("not implemented in this local CLI slice yet"));
        assert!(error.contains("ctx work validate"));
        assert!(error.contains("enforcement: none_local_diagnostic_only"));
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
            metadata_json: None,
            source_records: Vec::new(),
            created_at: None,
            updated_at: None,
            schema_version: 1,
        }
    }
}

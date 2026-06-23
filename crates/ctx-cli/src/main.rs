use std::{
    fs,
    io::{self, Read},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use clap::{Args, Parser, Subcommand, ValueEnum};
use uuid::Uuid;
use work_record_capture::{
    import_spool, inbox_dir as capture_inbox_dir, spool_counts, write_fixture, FixtureOptions,
};
use work_record_core::{
    blob_dir, database_path, default_data_root, device_path, inbox_dir, work_record_dir,
    AgentContextPacket, Evidence, WorkRecord, WorkRecordArchive,
};
use work_record_store::Store;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const DEFAULT_EVIDENCE_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_EVIDENCE_TIMEOUT_SECONDS: u64 = 300;
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
    #[command(about = "Capture evidence for work records")]
    Evidence(EvidenceCommand),
    #[command(about = "Import passive capture spool events")]
    Capture(CaptureCommand),
    #[command(about = "Attach a pull request URL to a work record")]
    LinkPr(LinkPrArgs),
    #[command(about = "Export work records and evidence as JSON")]
    Export(ExportArgs),
    #[command(about = "Import work records and evidence from JSON")]
    Import(ImportArgs),
    #[command(about = "Validate local Work Recorder storage")]
    Validate,
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
    #[command(about = "Import pending capture spool files")]
    Import(CaptureImportArgs),
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
struct LinkPrArgs {
    id: Uuid,
    pr_url: String,
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
        CommandRoot::Evidence(args) => {
            run_work_subcommand(WorkSubcommand::Evidence(args), data_root)
        }
        CommandRoot::Capture(args) => run_capture(args, data_root),
        CommandRoot::LinkPr(args) => run_work_subcommand(WorkSubcommand::LinkPr(args), data_root),
        CommandRoot::Export(args) => run_work_subcommand(WorkSubcommand::Export(args), data_root),
        CommandRoot::Import(args) => run_work_subcommand(WorkSubcommand::Import(args), data_root),
        CommandRoot::Validate => run_work_subcommand(WorkSubcommand::Validate, data_root),
        CommandRoot::Workspace(command) => run_workspace(command, data_root),
        CommandRoot::Work(command) => run_work(command, data_root),
    }
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
            let records = store.search_records(&args.query, args.limit)?;
            print_records(&records, args.json)?;
        }
        WorkSubcommand::Context(args) => {
            let context = store.context(args.query.as_deref(), args.limit)?;
            if args.json {
                let packet = AgentContextPacket::from_work_context(&context, 12_000);
                println!("{}", serde_json::to_string_pretty(&packet)?);
            } else {
                println!("{}", work_record_report::context_markdown(&context));
            }
        }
        WorkSubcommand::Report(args) => {
            let records = store.list_records(args.limit)?;
            let evidence = store.recent_evidence(args.limit)?;
            match args.format {
                ReportFormat::Text => {
                    print!("{}", work_record_report::render_text(&records, &evidence))
                }
                ReportFormat::Json => {
                    let summary = work_record_report::summarize(&records, &evidence);
                    print_json(serde_json::json!({
                        "schema_version": 1,
                        "summary": summary,
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
        WorkSubcommand::Validate => {
            let mut findings = store.validate()?;
            let counts = spool_counts(capture_inbox_dir(&data_root))?;
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
        }
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
    }
    Ok(())
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
            print_json(serde_json::json!({
                "schema_version": 1,
                "evidence": evidence,
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

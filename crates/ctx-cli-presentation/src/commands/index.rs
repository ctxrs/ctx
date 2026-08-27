use std::{
    io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{json, Value};

use crate::analytics::{count_bucket, IndexOperation, IndexState, IndexTelemetry, WaitOutcome};
use crate::output::{compact_json, print_json, JsonOutputFormat};
use crate::ui::{fields, outcome, Document, Field, LiveOutput, Outcome, OutcomeState, Ui};

use super::index_dashboard::{render_semantic_disabled_wait, IndexDashboard};

#[derive(Debug, Args)]
pub struct IndexArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
    #[command(subcommand)]
    command: Option<IndexCommand>,
    #[arg(skip)]
    wait_read_only: Option<bool>,
}

impl IndexArgs {
    pub fn json_output(&self) -> bool {
        self.format.is_json()
            || match &self.command {
                None => false,
                Some(IndexCommand::Mode(args)) => args.format.is_json(),
                Some(IndexCommand::Watch(args)) => args.format == IndexWatchFormat::Jsonl,
                Some(IndexCommand::Wait(args)) => args.format.is_json(),
            }
    }

    pub fn semantic_wait(format: JsonOutputFormat) -> Self {
        Self {
            format,
            wait_read_only: Some(false),
            command: Some(IndexCommand::Wait(IndexWaitArgs {
                format,
                lexical: false,
                semantic: true,
                all: false,
                timeout_seconds: None,
                interval_seconds: 2,
            })),
        }
    }
}

#[derive(Debug, Subcommand)]
enum IndexCommand {
    #[command(about = "Show or change automatic indexing mode")]
    Mode(IndexModeArgs),
    #[command(about = "Watch local indexing progress until ready")]
    Watch(IndexWatchArgs),
    #[command(about = "Wait until local indexing reaches a ready state")]
    Wait(IndexWaitArgs),
}

#[derive(Debug, Args)]
struct IndexModeArgs {
    #[arg(value_enum)]
    mode: Option<IndexModeArg>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IndexModeArg {
    Auto,
    Manual,
}

impl IndexModeArg {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    pub const fn is_auto(self) -> bool {
        matches!(self, Self::Auto)
    }
}

#[derive(Debug, Args)]
struct IndexWatchArgs {
    #[arg(long, value_enum, default_value_t = IndexWatchFormat::Text)]
    format: IndexWatchFormat,
    #[arg(long, default_value_t = 2, value_parser = parse_positive_seconds)]
    interval_seconds: u64,
}

#[derive(Debug, Args)]
struct IndexWaitArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
    #[arg(long, help = "Wait for lexical source indexing")]
    lexical: bool,
    #[arg(long, help = "Wait for semantic sidecar indexing")]
    semantic: bool,
    #[arg(long, help = "Wait for lexical and semantic indexing")]
    all: bool,
    #[arg(long, value_parser = parse_positive_seconds)]
    timeout_seconds: Option<u64>,
    #[arg(long, default_value_t = 2, value_parser = parse_positive_seconds)]
    interval_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum IndexWatchFormat {
    Text,
    Jsonl,
}

pub trait IndexReadinessPort {
    fn snapshot(&mut self, data_root: &Path) -> Result<Value>;
}

pub trait IndexModePort {
    fn current(&mut self, data_root: &Path) -> Result<Value>;
    fn update(&mut self, data_root: &Path, mode: IndexModeArg) -> Result<Value>;
}

pub fn run_index(
    args: IndexArgs,
    data_root: PathBuf,
    quiet: bool,
    telemetry: &mut IndexTelemetry,
    readiness: &mut dyn IndexReadinessPort,
    mode: &mut dyn IndexModePort,
    ui: &mut Ui,
) -> Result<()> {
    let parent_json = args.format.is_json();
    let wait_read_only = args.wait_read_only.unwrap_or(true);
    match args.command {
        None => {
            telemetry.operation = Some(IndexOperation::Status);
            let status = readiness.snapshot(&data_root)?;
            record_index_telemetry(telemetry, &status);
            if args.format.is_json() {
                print_json(status)?;
            } else if !quiet {
                let mut document = IndexDashboard.render(&status, ui.stdout_context());
                append_indexing_mode(&mut document, &status, ui);
                ui.write_stdout(&document)?;
            }
            Ok(())
        }
        Some(IndexCommand::Mode(args)) => {
            telemetry.operation = Some(IndexOperation::Mode);
            let report = match args.mode {
                Some(requested) => mode.update(&data_root, requested)?,
                None => mode.current(&data_root)?,
            };
            if parent_json || args.format.is_json() {
                print_json(report)?;
            } else if !quiet {
                let document = render_index_mode(&report, args.mode.is_some(), ui);
                ui.write_stdout(&document)?;
            }
            Ok(())
        }
        Some(IndexCommand::Watch(mut args)) => {
            telemetry.operation = Some(IndexOperation::Watch);
            if parent_json {
                args.format = IndexWatchFormat::Jsonl;
            }
            run_index_watch(args, &data_root, quiet, telemetry, readiness, ui)
        }
        Some(IndexCommand::Wait(mut args)) => {
            telemetry.operation = Some(IndexOperation::Wait);
            if parent_json {
                args.format = JsonOutputFormat::Json;
            }
            run_index_wait(
                args,
                &data_root,
                quiet,
                wait_read_only,
                telemetry,
                readiness,
                ui,
            )
        }
    }
}

fn append_indexing_mode(document: &mut Document, report: &Value, ui: &Ui) {
    let mode = report
        .pointer("/indexing/mode")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if !document.is_empty() {
        document.push_blank();
    }
    document.append(fields(
        ui.stdout_context(),
        &[Field::new("Indexing mode", mode)],
    ));
}

fn render_index_mode(report: &Value, changed: bool, ui: &Ui) -> Document {
    let mode = report
        .pointer("/indexing/mode")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if !changed {
        return fields(ui.stdout_context(), &[Field::new("Indexing mode", mode)]);
    }

    let requested_mode = report
        .pointer("/indexing/requested_mode")
        .and_then(Value::as_str)
        .unwrap_or(mode);
    let overridden = report
        .pointer("/indexing/overridden")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if overridden {
        let title = format!("Indexing mode remains {mode}");
        let detail = format!(
            "The requested {requested_mode} mode was saved, but a process-level override keeps {mode} mode active."
        );
        return outcome(
            ui.stdout_context(),
            Outcome {
                state: OutcomeState::Warning,
                title: &title,
                detail: Some(&detail),
            },
        );
    }

    let detail = match mode {
        "auto" => "ctx will keep the index current in the background.",
        "manual" => "Run `ctx import` or use `ctx search --refresh wait` to update the index.",
        _ => "The indexing mode was updated.",
    };
    outcome(
        ui.stdout_context(),
        Outcome {
            state: OutcomeState::Success,
            title: &format!("Indexing mode set to {mode}"),
            detail: Some(detail),
        },
    )
}

fn run_index_watch(
    args: IndexWatchArgs,
    data_root: &Path,
    quiet: bool,
    telemetry: &mut IndexTelemetry,
    readiness: &mut dyn IndexReadinessPort,
    ui: &mut Ui,
) -> Result<()> {
    let interval = Duration::from_secs(args.interval_seconds);
    let jsonl_output = args.format == IndexWatchFormat::Jsonl;
    let mut output = index_watch_output(ui);
    loop {
        let status = readiness.snapshot(data_root)?;
        let selection = IndexSelection::default_for(&status);
        record_index_telemetry(telemetry, &status);
        if let Some(message) = index_terminal_error(&status, selection) {
            if jsonl_output {
                output.print_json(&status)?;
            } else if !quiet {
                drop(output);
                let document = IndexDashboard.render(&status, ui.stderr_context());
                ui.write_stderr(&document)?;
            }
            return Err(forward_index_terminal_error(
                message,
                !jsonl_output && !quiet,
            ));
        }
        if jsonl_output {
            output.print_json(&status)?;
        } else if !quiet {
            output.print_human(&status)?;
        }
        if index_ready(&status, selection) {
            break;
        }
        thread::sleep(interval);
    }
    Ok(())
}

fn index_watch_output(ui: &mut Ui) -> IndexWatchOutput<&mut (dyn io::Write + Send)> {
    IndexWatchOutput::new(ui.stdout_live_output())
}

#[doc(hidden)]
pub struct IndexWatchOutput<W: io::Write> {
    output: LiveOutput<W>,
    dashboard: IndexDashboard,
}

impl<W: io::Write> IndexWatchOutput<W> {
    pub fn new(output: LiveOutput<W>) -> Self {
        Self {
            output,
            dashboard: IndexDashboard,
        }
    }

    #[doc(hidden)]
    pub fn for_test(writer: W, interactive: bool, terminal_width: usize) -> Self {
        let test_context = if interactive {
            crate::ui::TestContext::tty(crate::ui::StreamKind::Stdout, terminal_width)
        } else {
            crate::ui::TestContext::pipe(crate::ui::StreamKind::Stdout)
        };
        let context = crate::ui::RenderContext::for_test(test_context);
        Self::new(LiveOutput::new(writer, context))
    }

    pub fn print_json(&mut self, status: &Value) -> Result<()> {
        self.output.write_line(&serde_json::to_string(status)?)?;
        Ok(())
    }

    pub fn print_human(&mut self, status: &Value) -> io::Result<()> {
        self.output
            .render_frame(false, |context| self.dashboard.render(status, context))
    }

    #[doc(hidden)]
    pub fn reset_dashboard(&mut self) {
        self.dashboard = IndexDashboard;
    }

    #[doc(hidden)]
    pub fn writer(&self) -> &W {
        self.output.inner()
    }

    #[doc(hidden)]
    pub fn into_writer(self) -> W {
        self.output.into_inner()
    }
}

#[doc(hidden)]
pub fn render_dashboard_for_fixture(
    readiness: &Value,
    context: &crate::ui::RenderContext,
) -> Document {
    IndexDashboard.render(readiness, context)
}

#[derive(Default)]
struct IndexWaitHumanOutput {
    dashboard: IndexDashboard,
    last_frame: Option<String>,
}

impl IndexWaitHumanOutput {
    fn print(&mut self, ui: &mut Ui, status: &Value, selection: IndexSelection) -> Result<()> {
        let (document, frame) = self.render(ui, status, selection);
        ui.write_stdout(&document)?;
        self.last_frame = Some(frame);
        Ok(())
    }

    fn print_final(
        &mut self,
        ui: &mut Ui,
        status: &Value,
        selection: IndexSelection,
    ) -> Result<()> {
        let (document, frame) = self.render(ui, status, selection);
        if self.last_frame.as_ref() != Some(&frame) {
            ui.write_stdout(&document)?;
            self.last_frame = Some(frame);
        }
        Ok(())
    }

    fn render(&mut self, ui: &Ui, status: &Value, selection: IndexSelection) -> (Document, String) {
        let context = *ui.stdout_context();
        let document = self
            .dashboard
            .render_wait(status, &context, selection.refresh_convergence);
        let frame = document.render(&context);
        (document, frame)
    }
}

fn run_index_wait(
    args: IndexWaitArgs,
    data_root: &Path,
    quiet: bool,
    read_only: bool,
    telemetry: &mut IndexTelemetry,
    readiness: &mut dyn IndexReadinessPort,
    ui: &mut Ui,
) -> Result<()> {
    let explicit_selection = IndexSelection::from_wait_args(&args);
    let interval = Duration::from_secs(args.interval_seconds);
    let started = Instant::now();
    let mut human_output = IndexWaitHumanOutput::default();
    loop {
        let status = readiness.snapshot(data_root)?;
        let selection = explicit_selection.unwrap_or_else(|| IndexSelection::default_for(&status));
        telemetry.wait_lexical = Some(selection.lexical);
        telemetry.wait_semantic = Some(selection.semantic);
        record_index_telemetry(telemetry, &status);
        if let Some(message) = index_terminal_error(&status, selection) {
            telemetry.wait_outcome = Some(WaitOutcome::Blocked);
            if args.format.is_json() {
                print_json(index_wait_json(status, selection, "blocked", read_only))?;
            } else if !quiet {
                if selection.semantic
                    && !bool_at(&status, &["semantic", "enabled"])
                    && lexical_ready(&status)
                {
                    let document = render_semantic_disabled_wait(&status, ui.stderr_context());
                    ui.write_stderr(&document)?;
                } else {
                    let document = IndexDashboard.render(&status, ui.stderr_context());
                    ui.write_stderr(&document)?;
                }
            }
            return Err(forward_index_terminal_error(
                message,
                !args.format.is_json() && !quiet,
            ));
        }
        if index_ready(&status, selection) {
            telemetry.wait_outcome = Some(WaitOutcome::Ready);
            if args.format.is_json() {
                print_json(index_wait_json(status, selection, "ready", read_only))?;
            } else if !quiet {
                human_output.print(ui, &status, selection)?;
            }
            return Ok(());
        }
        if args
            .timeout_seconds
            .is_some_and(|timeout| started.elapsed() >= Duration::from_secs(timeout))
        {
            telemetry.wait_outcome = Some(WaitOutcome::Timeout);
            if args.format.is_json() {
                print_json(index_wait_json(status, selection, "timeout", read_only))?;
            } else if !quiet {
                human_output.print_final(ui, &status, selection)?;
            }
            return Err(anyhow!(
                "ctx index wait timed out before indexing was ready"
            ));
        }
        if !quiet && !args.format.is_json() {
            human_output.print(ui, &status, selection)?;
            ui.write_stdout(&crate::ui::Document::from_line(crate::ui::Line::new()))?;
        }
        thread::sleep(interval);
    }
}

fn record_index_telemetry(telemetry: &mut IndexTelemetry, status: &Value) {
    telemetry.initialized = Some(bool_at(status, &["initialized"]));
    telemetry.lexical_state = Some(IndexState::from_safe_summary(&string_at(
        status,
        &["lexical", "status"],
        "unknown",
    )));
    telemetry.semantic_state = Some(IndexState::from_safe_summary(&semantic_job_status(status)));
    telemetry.indexed_items = u64_at(status, &["lexical", "indexed_items"]).map(count_bucket);
}

#[derive(Debug, Clone, Copy)]
struct IndexSelection {
    lexical: bool,
    semantic: bool,
    refresh_convergence: bool,
}

impl IndexSelection {
    fn all() -> Self {
        Self {
            lexical: true,
            semantic: true,
            refresh_convergence: true,
        }
    }

    fn from_wait_args(args: &IndexWaitArgs) -> Option<Self> {
        if args.all {
            Some(Self::all())
        } else if args.lexical || args.semantic {
            Some(Self {
                lexical: args.lexical,
                semantic: args.semantic,
                refresh_convergence: false,
            })
        } else {
            None
        }
    }

    fn default_for(status: &Value) -> Self {
        Self {
            lexical: true,
            semantic: bool_at(status, &["semantic", "enabled"]),
            refresh_convergence: true,
        }
    }
}

fn index_ready(status: &Value, selection: IndexSelection) -> bool {
    (!selection.lexical || lexical_ready(status))
        && (!selection.refresh_convergence || refresh_converged(status))
        && (!selection.semantic || semantic_ready(status))
}

fn lexical_ready(status: &Value) -> bool {
    string_at(status, &["lexical", "status"], "unknown") == "ready"
}

fn refresh_converged(status: &Value) -> bool {
    let refresh_status = string_at(status, &["refresh", "status"], "unknown");
    let refresh_reason = string_at(status, &["refresh", "reason"], "");
    refresh_status == "ready"
        || (refresh_status == "partial"
            && string_at(status, &["refresh", "request_state"], "unknown") == "published"
            && bool_at(status, &["refresh", "generation_matches"]))
        || (refresh_status == "unavailable"
            && matches!(
                refresh_reason.as_str(),
                "daemon_unavailable" | "refresh_not_observed"
            ))
}

fn semantic_ready(status: &Value) -> bool {
    matches!(semantic_job_status(status).as_str(), "ready" | "empty")
}

fn index_terminal_error(status: &Value, selection: IndexSelection) -> Option<String> {
    let lexical_status = string_at(status, &["lexical", "status"], "unknown");
    let lexical_reason = string_at(status, &["lexical", "reason"], "");
    let refresh_status = string_at(status, &["refresh", "status"], "unknown");
    if selection.lexical
        && lexical_reason == "generation_not_published"
        && refresh_status != "pending"
    {
        return Some("ctx index does not exist yet; run `ctx setup` first".to_owned());
    }
    if selection.lexical
        && matches!(lexical_status.as_str(), "stale" | "unavailable")
        && refresh_status != "pending"
        && (lexical_reason == "core_refresh_failed" || !bool_at(status, &["daemon", "running"]))
    {
        return Some("history refresh is unavailable; run `ctx doctor` for details".to_owned());
    }
    let refresh_reason = string_at(status, &["refresh", "reason"], "");
    if selection.refresh_convergence
        && matches!(refresh_status.as_str(), "stale" | "unavailable")
        && !matches!(
            refresh_reason.as_str(),
            "daemon_unavailable" | "refresh_not_observed"
        )
        && (refresh_reason == "core_refresh_failed" || !bool_at(status, &["daemon", "running"]))
    {
        return Some("history refresh is unavailable; run `ctx doctor` for details".to_owned());
    }
    if selection.semantic {
        let semantic_status = semantic_job_status(status);
        let semantic_job_status = string_at(
            status,
            &["daemon", "jobs", "semantic_index", "status"],
            "unknown",
        );
        let reason = string_at(status, &["daemon", "jobs", "semantic_index", "reason"], "");
        if semantic_job_status == "skipped" && reason == "model_cache_missing" {
            return Some(
                "semantic indexing is skipped because the local embedding model cache is missing"
                    .to_owned(),
            );
        }
        if matches!(
            semantic_status.as_str(),
            "disabled" | "failed" | "stale_lock" | "unavailable"
        ) {
            return Some(format!("semantic indexing is {semantic_status}"));
        }
    }
    if selected_pending_work(status, selection) && !bool_at(status, &["daemon", "running"]) {
        return Some(
            "background indexing stopped before the index was ready; run `ctx doctor` for details"
                .to_owned(),
        );
    }
    None
}

fn selected_pending_work(status: &Value, selection: IndexSelection) -> bool {
    let refresh_pending =
        pending_state(&string_at(status, &["refresh", "request_state"], "unknown"))
            || pending_state(&string_at(status, &["refresh", "status"], "unknown"));
    let lexical_pending = !lexical_ready(status)
        && (pending_state(&string_at(status, &["lexical", "status"], "unknown"))
            || refresh_pending);
    let semantic_pending = pending_state(&string_at(status, &["semantic", "status"], "unknown"))
        || pending_state(&string_at(
            status,
            &["daemon", "jobs", "semantic_index", "status"],
            "unknown",
        ));

    (selection.lexical && lexical_pending)
        || (selection.refresh_convergence && refresh_pending)
        || (selection.semantic && semantic_pending)
}

fn pending_state(state: &str) -> bool {
    matches!(
        state,
        "admission_pending" | "pending" | "queued" | "running"
    )
}

fn forward_index_terminal_error(message: String, human_output_rendered: bool) -> anyhow::Error {
    if human_output_rendered {
        crate::rendered_cli_error()
    } else {
        anyhow!(message)
    }
}

fn index_wait_json(
    status: Value,
    selection: IndexSelection,
    wait_status: &str,
    read_only: bool,
) -> Value {
    let local_only = status["local_only"].as_bool().unwrap_or(true);
    compact_json(json!({
        "schema_version": 1,
        "status": wait_status,
        "selection": {
            "lexical": selection.lexical,
            "semantic": selection.semantic,
        },
        "readiness": status,
        "local_only": local_only,
        "read_only": read_only,
    }))
}

fn semantic_job_status(status: &Value) -> String {
    value_at(status, &["semantic", "status"])
        .and_then(Value::as_str)
        .or_else(|| {
            value_at(status, &["daemon", "jobs", "semantic_index", "status"])
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown")
        .to_owned()
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn string_at(value: &Value, path: &[&str], default: &str) -> String {
    value_at(value, path)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn bool_at(value: &Value, path: &[&str]) -> bool {
    value_at(value, path)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn u64_at(value: &Value, path: &[&str]) -> Option<u64> {
    value_at(value, path).and_then(Value::as_u64)
}

fn parse_positive_seconds(value: &str) -> std::result::Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|err| format!("invalid seconds: {err}"))?;
    if !(1..=86_400).contains(&parsed) {
        return Err("seconds must be between 1 and 86400".to_owned());
    }
    Ok(parsed)
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
